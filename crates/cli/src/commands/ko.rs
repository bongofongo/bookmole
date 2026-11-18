use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use bookmole::{BookImportStats, DeviceBook, DeviceState, Engine, MatchCandidate, PluginCondition};

use super::resolve_one;

pub async fn import(engine: &Engine, path: &Path, dry_run: bool) -> Result<()> {
    let report = engine.import_koreader(path, dry_run).await?;
    let mode = if dry_run { " (dry run)" } else { "" };

    for w in &report.warnings {
        eprintln!("warning: {w}");
    }
    for s in &report.imported {
        println!("{}", stats_line(s, mode));
    }
    for u in &report.unmatched {
        println!(
            "unmatched{mode}: {} ({})",
            u.title.as_deref().unwrap_or("unknown title"),
            u.path.display()
        );
        print_candidates(&u.candidates);
        println!("    pull it in : bookmole ko pull {}", u.path.display());
        println!(
            "    or link it : bookmole ko link {} <book>",
            u.path.display()
        );
    }
    if report.imported.is_empty() && report.unmatched.is_empty() && report.warnings.is_empty() {
        println!("nothing to import.");
    }
    Ok(())
}

pub async fn pull(engine: &Engine, path: &Path, new: bool) -> Result<()> {
    // Look before creating: creating first and warning after would leave a silent duplicate.
    if !new {
        let candidates = engine.sidecar_candidates(path).await?;
        if !candidates.is_empty() {
            println!("{} looks like a book you already have:", path.display());
            print_candidates(&candidates);
            println!(
                "    link it    : bookmole ko link {} <book>",
                path.display()
            );
            println!("    or pull it as a new book: --new");
            return Ok(());
        }
    }

    let report = engine.pull_book_from_sidecar(path).await?;
    for w in &report.warnings {
        eprintln!("warning: {w}");
    }
    println!("{}", stats_line(&report.stats, ""));
    println!(
        "  #{} — no ISBN, cover or description yet: `bookmole enrich {}`",
        report.stats.book_id, report.stats.book_id
    );
    Ok(())
}

pub async fn link(engine: &Engine, path: &Path, selector: &str) -> Result<()> {
    let book = resolve_one(engine, selector).await?;
    let Some(book_id) = book.id else {
        bail!("'{selector}' resolved to an unsaved book");
    };
    let md5 = engine.link_sidecar(path, book_id).await?;
    println!(
        "linked {} to {} ({md5})",
        path.display(),
        book.display_title()
    );
    import(engine, path, false).await
}

/// Show the state of every book on a mounted reader. Writes nothing.
pub async fn scan(engine: &Engine, path: Option<&Path>) -> Result<()> {
    let root = resolve_mount(path)?;
    let scan = engine.scan_device(&root).await?;

    for w in &scan.warnings {
        eprintln!("warning: {w}");
    }
    for b in &scan.books {
        println!("{}", device_line(b));
        match &b.state {
            DeviceState::New { candidates } => {
                print_candidates(candidates);
                if !candidates.is_empty() {
                    println!(
                        "    link it    : bookmole ko link {} <book>",
                        b.path.display()
                    );
                }
            }
            DeviceState::Unreadable(d) => println!("    {}", d.detail),
            _ => {}
        }
    }

    let syncable = scan.syncable().count();
    println!();
    println!(
        "{} books on {} ({} read, {} unchanged since last scan)",
        scan.books.len(),
        root.display(),
        scan.parsed,
        scan.cached
    );
    if syncable > 0 {
        println!(
            "  bring them across: bookmole ko sync {} --all",
            root.display()
        );
    }
    Ok(())
}

/// Opens the pairing window and waits, printing what arrives.
/// Blocks on purpose: the listener lives only as long as this process.
pub async fn listen(engine: &Engine, minutes: u32) -> Result<()> {
    let paired = engine.paired_devices().await?;
    if paired.is_empty() {
        println!("no reader is paired, so nothing can reach this computer.");
        println!("    pair one : plug it in, then bookmole ko plugin install");
        println!("    or, if it already has the plugin : bookmole ko pair <code>");
        return Ok(());
    }

    let status = engine.start_listening(Some(minutes)).await?;
    let port = status
        .tcp_port
        .map(|p| p.to_string())
        .unwrap_or_else(|| "?".into());
    match minutes {
        0 => println!("listening on port {port} — ctrl-c to stop"),
        1 => println!("listening on port {port} for a minute, or until a reader pushes"),
        m => println!("listening on port {port} for {m} minutes, or until a reader pushes"),
    }
    for d in &paired {
        println!("  {} can reach it", device_name(d));
    }
    if let Some(address) = engine.lan_address() {
        println!("this computer is at {address} on this network");
    }
    println!("on the reader: Tools → bookmole → Send highlights to this computer");

    // Polled: the engine emits no event stream for this.
    let mut seen = status.pushes;
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let now = engine.listener_status().await?;
        if now.pushes > seen {
            seen = now.pushes;
            println!("a reader pushed.");
            for d in engine.paired_devices().await? {
                if d.last_wireless_at.is_some() {
                    println!(
                        "  from {} at {}",
                        device_name(&d),
                        d.last_lan_addr
                            .as_deref()
                            .unwrap_or("an address we could not read")
                    );
                }
            }
        }
        if !matches!(now.mode, bookmole::ListenerMode::Off) {
            continue;
        }
        println!("the door is shut.");
        return Ok(());
    }
}

/// Fetch from a reader whose own window is open; needs no listener of ours.
pub async fn fetch(engine: &Engine, typed: &str) -> Result<()> {
    let device = resolve_device(engine, typed).await?;
    println!("looking for {} on the network…", device_name(&device));

    let reports = match engine.pull_from_reader(&device.device_id).await {
        Ok(r) => r,
        Err(e) => {
            println!("{e}");
            println!("    on the reader : Tools → bookmole → Let a computer fetch highlights");
            println!("    then          : bookmole ko fetch {typed}");
            return Ok(());
        }
    };

    if reports.is_empty() {
        println!("{} had nothing to send.", device_name(&device));
        return Ok(());
    }
    let mut inserted = 0;
    let mut updated = 0;
    for r in &reports {
        inserted += r.stats.inserted;
        updated += r.stats.updated;
        for w in &r.warnings {
            println!("  {w}");
        }
    }
    println!(
        "{} book{} came across: {inserted} new, {updated} updated.",
        reports.len(),
        if reports.len() == 1 { "" } else { "s" }
    );
    println!("what the reader had open to send — plug it in for everything.");
    Ok(())
}

/// Pair with a reader over wifi using the code on its screen (single-use, so an argument).
pub async fn pair(engine: &Engine, code: &str, host: Option<&str>) -> Result<()> {
    println!("looking for the reader showing that code…");
    let device = match engine.pair_with_reader(code, host).await {
        Ok(d) => d,
        Err(e) => {
            println!("{e}");
            println!("    on the reader : Tools → bookmole → Link a computer over wifi");
            println!("    then          : bookmole ko pair <the code it shows>");
            if host.is_none() {
                println!(
                    "    if it still cannot be found, add the address the reader shows: --host <address>"
                );
            }
            return Ok(());
        }
    };
    println!(
        "paired with {} over wifi ({}).",
        device_name(&device),
        device.device_id
    );
    println!(
        "nothing is sent by itself. open the door here with `bookmole ko listen`, \n\
         then on the reader: Tools -> bookmole -> Send highlights to this computer."
    );
    Ok(())
}

pub async fn watch(engine: &Engine) -> Result<()> {
    let mut watcher = bookmole::watch_mounts()?;

    let roots = bookmole::mount_roots();
    println!(
        "watching {} — ctrl-c to stop",
        roots
            .iter()
            .map(|r| r.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    match bookmole::candidate_mounts().as_slice() {
        [] => println!("nothing mounted yet."),
        mounts => {
            for mount in mounts {
                println!("already here: {}", mount.display());
            }
            println!("  scan one now: bookmole ko scan <path>");
        }
    }

    while let Some(event) = watcher.next().await {
        match event {
            bookmole::MountEvent::Arrived(mount) => {
                println!();
                println!("reader mounted: {}", mount.display());
                scan(engine, Some(&mount)).await?;
            }
            bookmole::MountEvent::Departed(mount) => {
                println!();
                println!("unplugged: {}", mount.display());
            }
        }
    }
    Ok(())
}

pub async fn sync(engine: &Engine, path: &Path, all: bool, selectors: &[String]) -> Result<()> {
    // `--all` goes straight to `sync_mount` before any local scan: only it stamps
    // `last_synced_at`, and scanning first would print every warning twice.
    if all {
        let done = engine.sync_mount(path).await?;
        for w in &done.warnings {
            eprintln!("warning: {w}");
        }
        for report in &done.reports {
            for w in &report.warnings {
                eprintln!("warning: {w}");
            }
            println!("{}", stats_line(&report.stats, ""));
        }
        if done.synced == 0 {
            if done.found == 0 {
                println!("bookmole found no books on {}.", path.display());
            } else {
                println!(
                    "nothing to sync — everything on {} is already here.",
                    path.display()
                );
            }
        }
        return Ok(());
    }

    let scan = engine.scan_device(path).await?;
    for w in &scan.warnings {
        eprintln!("warning: {w}");
    }

    let syncable: Vec<&DeviceBook> = scan.syncable().collect();
    if syncable.is_empty() {
        println!(
            "nothing to sync — everything on {} is already here.",
            path.display()
        );
        return Ok(());
    }

    if selectors.is_empty() {
        println!("{} books to bring across:", syncable.len());
        for b in &syncable {
            println!("{}", device_line(b));
        }
        println!();
        println!("  all of them : bookmole ko sync {} --all", path.display());
        println!(
            "  or one      : bookmole ko sync {} --book \"{}\"",
            path.display(),
            syncable[0].display_title()
        );
        return Ok(());
    }

    let chosen: Vec<&DeviceBook> = select(&syncable, selectors)?;

    let paths: Vec<PathBuf> = chosen.iter().map(|b| b.path.clone()).collect();
    for report in engine.sync_device(&paths).await? {
        for w in &report.warnings {
            eprintln!("warning: {w}");
        }
        println!("{}", stats_line(&report.stats, ""));
    }
    Ok(())
}

fn resolve_mount(path: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = path {
        return Ok(p.to_path_buf());
    }
    let mut mounts = bookmole::candidate_mounts();
    match mounts.len() {
        0 => bail!(
            "no mounted KOReader device found (looked under /Volumes, /run/media/$USER and \
             /media/$USER). Pass the path to scan a library directory instead."
        ),
        1 => Ok(mounts.remove(0)),
        _ => {
            let list: Vec<String> = mounts.iter().map(|m| m.display().to_string()).collect();
            bail!(
                "several KOReader devices are mounted; name one:\n  {}",
                list.join("\n  ")
            )
        }
    }
}

fn select<'a>(books: &[&'a DeviceBook], selectors: &[String]) -> Result<Vec<&'a DeviceBook>> {
    let mut chosen: Vec<&DeviceBook> = Vec::new();
    for want in selectors {
        let needle = want.to_lowercase();
        let hits: Vec<&&DeviceBook> = books
            .iter()
            .filter(|b| {
                b.display_title().to_lowercase().contains(&needle)
                    || b.path.to_string_lossy().to_lowercase().contains(&needle)
            })
            .collect();
        match hits.len() {
            0 => bail!("no book to sync matches '{want}'"),
            _ => {
                for b in hits {
                    if !chosen.iter().any(|c| c.path == b.path) {
                        chosen.push(b);
                    }
                }
            }
        }
    }
    Ok(chosen)
}

fn device_line(b: &DeviceBook) -> String {
    let detail = match &b.state {
        DeviceState::Updated {
            new_highlights,
            refreshed,
        } => {
            let mut parts = Vec::new();
            if *new_highlights > 0 {
                parts.push(format!("{new_highlights} new"));
            }
            if *refreshed > 0 {
                parts.push(format!("{refreshed} edited on the device"));
            }
            if parts.is_empty() {
                parts.push("progress changed on the device".to_string());
            }
            format!("  ({})", parts.join(", "))
        }
        DeviceState::New { .. } => String::new(),
        _ => String::new(),
    };
    let progress = b
        .ko_percent
        .map(|p| format!("  [{:.0}%]", p * 100.0))
        .unwrap_or_default();
    let authors = b
        .authors
        .as_deref()
        .map(|a| format!(" — {}", a.replace('\n', ", ")))
        .unwrap_or_default();
    format!(
        "  {:<10} {}{authors}{progress}{detail}",
        b.state.label(),
        b.display_title()
    )
}

fn stats_line(s: &BookImportStats, mode: &str) -> String {
    format!(
        "{}{mode}: {} new, {} updated from the device, {} already known, {} flashcard candidates \
         (matched by {})",
        s.book_title, s.inserted, s.updated, s.skipped, s.flashcards, s.matched_by
    )
}

fn print_candidates(candidates: &[MatchCandidate]) {
    for c in candidates {
        println!(
            "    maybe #{}: {} ({:.0}%)",
            c.book_id,
            c.title,
            c.score * 100.0
        );
    }
}

/// Its own verb, not part of `sync`: arriving at a mount stays read-only.
pub async fn stats(engine: &Engine, path: &Path) -> Result<()> {
    let report = engine.import_device_statistics(path).await?;

    for w in &report.warnings {
        eprintln!("warning: {w}");
    }

    match report.schema_version {
        Some(v) => println!("{}: statistics database, schema {v}", path.display()),
        None => {
            println!("{}: no statistics database here.", path.display());
            println!("    enable KOReader's statistics plugin on the device, then read a page.");
            return Ok(());
        }
    }

    println!(
        "  {} of {} books are in your library",
        report.books_matched, report.books_in_db
    );
    println!("  {} days measured", report.days);
    println!(
        "  {} new, {} changed",
        report.events.inserted, report.events.updated
    );
    if report.books_in_db > 0 && report.books_matched == 0 {
        println!("    nothing here matches a book you have.");
        println!("    bookmole ko scan {}", path.display());
    }
    Ok(())
}

/// What is on the reader; with no reader named, every reader ever paired.
pub async fn plugin_status(engine: &Engine, path: Option<&Path>) -> Result<()> {
    if path.is_none() && bookmole::candidate_mounts().is_empty() {
        let paired = engine.paired_devices().await?;
        if paired.is_empty() {
            println!("no reader is plugged in, and none has been paired.");
            println!("    plug one in, then: bookmole ko plugin install");
            return Ok(());
        }
        println!("no reader is plugged in. paired readers:");
        for d in &paired {
            println!("  {}  {}", device_name(d), d.device_id);
            println!(
                "      plugin v{}, last plugged in at {}",
                d.plugin_version,
                d.last_mount_path.as_deref().unwrap_or("an unknown path"),
            );
            println!(
                "      last in your hands: {}",
                match d.last_seen_at {
                    Some(t) => crate::render::date(t),
                    None => "not since bookmole started recording it".to_string(),
                }
            );
            println!(
                "      everything brought across: {}",
                match d.last_synced_at {
                    Some(t) => crate::render::date(t),
                    None => "not since bookmole started recording it".to_string(),
                }
            );
            if let Some(t) = d.last_wireless_at {
                println!(
                    "      over the air: {}{}",
                    crate::render::date(t),
                    match d.last_lan_addr.as_deref() {
                        Some(addr) => format!(", from {addr}"),
                        None => String::new(),
                    }
                );
            }
        }
        println!();
        println!("    plug one in : bookmole ko sync <mount> --all");
        println!("    sold it?    : bookmole ko plugin forget <device-id>");
        return Ok(());
    }

    let mount = resolve_mount(path)?;
    let status = engine.plugin_status(&mount).await?;
    println!("reader     : {}", status.mount.display());
    println!("plugin dir : {}", status.plugin_dir.display());

    match status.condition() {
        PluginCondition::Absent => println!(
            "installed  : no (this bookmole carries v{})",
            status.our_version
        ),
        PluginCondition::Current => println!(
            "installed  : v{} (up to date)",
            status.installed_version.unwrap_or(status.our_version)
        ),
        PluginCondition::Upgradable => println!(
            "installed  : v{} (this bookmole carries v{})",
            status.installed_version.unwrap_or_default(),
            status.our_version
        ),
        PluginCondition::Unversioned => println!("installed  : yes, but it carries no version"),
        PluginCondition::Obstructed => match status.installed_version {
            Some(v) if v > status.our_version => println!(
                "installed  : v{v} — newer than this bookmole (v{})",
                status.our_version
            ),
            Some(v) => println!("installed  : v{v}"),
            None => println!("installed  : yes"),
        },
    }

    match &status.device_id {
        Some(id) => println!("paired     : yes, as {id}"),
        None if status.pairings.is_empty() => println!("paired     : no"),
        None => println!("paired     : not with this bookmole"),
    }
    let others: Vec<&bookmole::plugin::Pairing> = status
        .pairings
        .iter()
        .filter(|p| Some(p.device_id.as_str()) != status.device_id.as_deref())
        .collect();
    for p in &others {
        match &p.name {
            Some(name) => println!("also paired: {name} ({})", p.device_id),
            None => println!("also paired: {}", p.device_id),
        }
    }
    if status.device_id.is_none() && !others.is_empty() {
        println!("    pair it here too: bookmole ko plugin install");
    }

    for m in &status.modified {
        println!("edited here: {m} — bookmole will not overwrite or remove it");
    }
    for u in &status.unrecognised {
        println!("not ours   : {u} — bookmole will not remove it");
    }
    if !status.installed && !status.is_obstructed() {
        println!("    install it : bookmole ko plugin install");
    }
    Ok(())
}

/// Install or upgrade the plugin. Explicit, and prints the target path before writing.
pub async fn plugin_install(engine: &Engine, path: Option<&Path>, yes: bool) -> Result<()> {
    let mount = resolve_mount(path)?;
    let status = engine.plugin_status(&mount).await?;

    println!(
        "{} bookmole's plugin into:",
        match () {
            _ if status.is_version_upgrade() => "upgrade",
            _ if status.is_reinstall() => "reinstall",
            _ => "install",
        }
    );
    println!("    {}", status.plugin_dir.display());
    println!(
        "nothing else on the reader is written to, and `ko plugin uninstall` removes exactly this."
    );
    if !yes && !confirm()? {
        println!("nothing was written.");
        return Ok(());
    }

    let report = engine.install_plugin(&mount).await?;
    for f in &report.written {
        println!("wrote {}", report.plugin_dir.join(f).display());
    }
    // `status` is the pre-install reading: only it tells whether this computer was already
    // paired, as opposed to another computer having put the plugin there.
    let was_ours = status.paired;
    match report.upgraded_from {
        Some(from) if from < report.version && was_ours => println!(
            "upgraded v{from} → v{}, paired as {}",
            report.version, report.device_id
        ),
        Some(_) if was_ours => println!(
            "reinstalled v{}, still paired as {}",
            report.version, report.device_id
        ),
        Some(_) => println!(
            "the plugin was already installed; this computer is now paired too, as {}",
            report.device_id
        ),
        None => println!(
            "installed v{}, paired as {}",
            report.version, report.device_id
        ),
    }
    let others = status
        .pairings
        .iter()
        .filter(|p| p.device_id != report.device_id)
        .count();
    if others > 0 {
        println!(
            "this reader is also paired with {others} other computer{}, and \
             bookmole left {} alone.",
            if others == 1 { "" } else { "s" },
            if others == 1 { "it" } else { "them" }
        );
    }
    println!(
        "nothing is sent by itself. open the door here with `bookmole ko listen`, \n\
         then on the reader: Tools -> bookmole -> Send highlights to this computer."
    );
    Ok(())
}

/// Remove exactly what we installed, and forget the pairing.
pub async fn plugin_uninstall(engine: &Engine, path: Option<&Path>) -> Result<()> {
    let mount = resolve_mount(path)?;
    let report = engine.uninstall_plugin(&mount).await?;
    if report.removed.is_empty() {
        println!("nothing of bookmole's was on {}.", mount.display());
        return Ok(());
    }
    for f in &report.removed {
        println!("removed {}", report.plugin_dir.join(f).display());
    }
    if let Some(id) = &report.forgot_device {
        println!("forgot the pairing with {id}.");
    }
    let others = report
        .removed_pairings
        .iter()
        .filter(|id| Some(id.as_str()) != report.forgot_device.as_deref())
        .count();
    if others > 0 {
        println!(
            "this also ended {others} other computer{} pairing with this reader. \
             bookmole cannot tell {}, so install again from {} to restore it.",
            if others == 1 { "'s" } else { "s'" },
            if others == 1 { "it" } else { "them" },
            if others == 1 { "there" } else { "each" }
        );
    }
    Ok(())
}

fn confirm() -> Result<bool> {
    use std::io::Write as _;
    print!("go ahead? [y/N] ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes"))
}

fn device_name(d: &bookmole::PairedDevice) -> &str {
    match d.label.as_deref() {
        Some(l) => l,
        None => &d.device_id[..8.min(d.device_id.len())],
    }
}

async fn resolve_device(engine: &Engine, typed: &str) -> Result<bookmole::PairedDevice> {
    let paired = engine.paired_devices().await?;
    let hits: Vec<bookmole::PairedDevice> = paired
        .into_iter()
        .filter(|d| d.device_id.starts_with(typed))
        .collect();
    match hits.len() {
        1 => Ok(hits.into_iter().next().expect("one")),
        0 => anyhow::bail!(
            "no paired reader starts with \"{typed}\".\n    \
             which ones there are: bookmole ko plugin status"
        ),
        _ => {
            let names: Vec<String> = hits
                .iter()
                .map(|d| format!("{} ({})", d.device_id, device_name(d)))
                .collect();
            anyhow::bail!(
                "\"{typed}\" names {} readers: {}.\n    \
                 type more of the id",
                hits.len(),
                names.join(", ")
            )
        }
    }
}

pub async fn plugin_rename(engine: &Engine, typed: &str, label: &str) -> Result<()> {
    let device = resolve_device(engine, typed).await?;
    let was = device_name(&device).to_string();
    engine.rename_device(&device.device_id, label).await?;

    let now = engine.paired_devices().await?;
    let now = now
        .iter()
        .find(|d| d.device_id == device.device_id)
        .expect("just renamed");
    if now.label.is_none() {
        println!(
            "{was} has no name of its own again — it is {}.",
            device_name(now)
        );
    } else {
        println!("{was} is now {}.", device_name(now));
    }
    Ok(())
}

/// Forget a reader we do not have in hand. Drops only our half of the pairing; the device is
/// untouched.
pub async fn plugin_forget(engine: &Engine, typed: &str) -> Result<()> {
    let device = resolve_device(engine, typed).await?;
    let name = device_name(&device).to_string();
    engine.forget_device(&device.device_id).await?;
    println!("forgotten: {name}.");
    println!(
        "    the plugin is still on that reader — bookmole cannot reach it from here.\n    \
         plugged in? this removes it: bookmole ko plugin uninstall"
    );
    Ok(())
}
