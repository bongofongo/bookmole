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
        // Unmatched is a decision, not a dead end. Say what the two moves are,
        // and — when the library holds something close — which book it probably
        // is, rather than leaving the user to find it.
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

/// Create the book from the sidecar's own metadata and import its highlights.
pub async fn pull(engine: &Engine, path: &Path, new: bool) -> Result<()> {
    // Look before creating. The whole reason `match_candidates` exists is that a
    // variant title used to become a silent duplicate, and creating first and
    // warning after would reproduce exactly that.
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
    // The pull is offline by design, so the book arrives bare. This line used to
    // name `search` then `ko link` — hand-assembling, out of two commands meant
    // for something else, exactly what `enrich` does. It is a real command
    // now, and it is one.
    println!(
        "  #{} — no ISBN, cover or description yet: `bookmole enrich {}`",
        report.stats.book_id, report.stats.book_id
    );
    Ok(())
}

/// Record that this sidecar is that book, then import into it.
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
    // The link only pays off on the next import, so run it now: otherwise
    // `link` looks like it did nothing.
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
            // Unmatched is a decision, not a dead end — say what the two moves
            // are, and which book it probably already is.
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

/// Wait for a reader to be plugged in, and scan it when it is. Writes nothing.
///
/// This is the headless half of the mount watcher, and the instrument the wired
/// path is confirmed with on real hardware — the TUI does the same thing behind
/// a screen, which is one more thing to be wrong when a device does not show up.
///
/// **Scans, never syncs**, like everything else on the automatic path: it prints
/// the command that brings books across rather than running it.
/// Open the door and wait, printing what arrives.
///
/// The one command in this file that **blocks on purpose**: the listener is
/// runtime state, so a one-shot process that opened a window and exited would
/// have closed it on the way out. This is therefore the CLI's honest shape for
/// a feature that is otherwise the daemon's or the GUI's — and it names which
/// readers could use the door before it opens one, because a door held open for
/// a reader that is not paired is a door nothing can come through.
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
    // For the reader that cannot find us by itself: it keeps a typed address,
    // and this is the number to type. Said only when there is one.
    if let Some(address) = engine.lan_address() {
        println!("this computer is at {address} on this network");
    }
    println!("on the reader: Tools → bookmole → Send highlights to this computer");

    // Polled rather than notified, because the engine emits no stream for this
    // — the same reason the mount watcher is absent from the API. A second is
    // far below human patience and costs one lock.
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

/// Fetch from a reader whose own window is open.
///
/// `listen`'s mirror, and the reason both exist: push is one tap on the reader
/// and pull is one click here, and which is convenient depends entirely on
/// which device is already in your hand. This one needs **no listener of ours**
/// — a seeker sends first, so the reply comes back to the port it sent from.
pub async fn fetch(engine: &Engine, typed: &str) -> Result<()> {
    let device = resolve_device(engine, typed).await?;
    println!("looking for {} on the network…", device_name(&device));

    let reports = match engine.pull_from_reader(&device.device_id).await {
        Ok(r) => r,
        // A refusal is a decision with a next move in it, never a stack of
        // prose — `ko plugin`'s tone, and the same shape the reader's own
        // failure message takes in the other direction.
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
    // The distinction migration `0020` exists for, said out loud rather than
    // left to be inferred from a timestamp somewhere else.
    println!("what the reader had open to send — plug it in for everything.");
    Ok(())
}

/// Pair with a reader over wifi, using the code on its screen.
///
/// The cable-free twin of `ko plugin install`, for a reader that already
/// carries the plugin. The code is an argument rather than a prompt because it
/// is spent by one use and dead in two minutes — the things that make a secret
/// on a command line a bad idea are that it is reusable and that it lasts.
pub async fn pair(engine: &Engine, code: &str, host: Option<&str>) -> Result<()> {
    println!("looking for the reader showing that code…");
    let device = match engine.pair_with_reader(code, host).await {
        Ok(d) => d,
        // A refusal with its next move, in `fetch`'s shape. The engine's own
        // sentence already names the three things to check; what it cannot
        // know is how the code gets onto the reader's screen.
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
    // A watcher reports changes, so a reader already plugged in would otherwise
    // make this look like it had failed to notice one.
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

/// Pull a selection of a mounted reader in.
pub async fn sync(engine: &Engine, path: &Path, all: bool, selectors: &[String]) -> Result<()> {
    // `--all` is `sync_mount` and takes **the whole function**, before this
    // command scans anything itself. Three reasons, and the first two were
    // found by running it. It is the only verb that knows the mount and so the
    // only one that can stamp `last_synced_at` (migration `0020`); scanning
    // here first and calling it afterwards prints every warning **twice**,
    // since it scans too; and an early return on "nothing to sync" placed
    // above it skipped the stamp entirely, so a reader you were fully up to
    // date with could never record that it was — the CLI and the engine
    // disagreeing about a column whose whole job is to be the same on both.
    //
    // A per-book `--book` pull deliberately does *not* stamp: it leaves the
    // question that column answers — *is this reader's reading here* —
    // unchanged.
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
        // Two sentences, because they are two facts: a reader you have read
        // nothing on and a reader you are up to date with are not the same
        // picture, and one "nothing to sync" renders them identically.
        if done.synced == 0 {
            if done.found == 0 {
                // Not "nothing to read **yet**". bookmole has no
                // completion framing, and "yet" is how it creeps in; this is
                // a fact about a volume, not a thing the reader owes.
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

    // Neither flag is not "sync everything": it is a question. Show what would
    // happen and name the flag rather than writing forty books unasked.
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

/// Which mount to work on: the one given, else the one plugged in.
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
            // Picking one for the user would be a guess about which reader they
            // meant, and both are plugged in.
            let list: Vec<String> = mounts.iter().map(|m| m.display().to_string()).collect();
            bail!(
                "several KOReader devices are mounted; name one:\n  {}",
                list.join("\n  ")
            )
        }
    }
}

/// Resolve `--book` selectors against the scanned rows: a case-insensitive
/// fragment of the title, or of the sidecar path.
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
        // Silently syncing nothing, or the wrong book, is worse than stopping.
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

/// `ko stats` — measured reading time out of the device's own
/// `statistics.sqlite3`.
///
/// **Its own verb, and not part of `sync`.** Arrival (plugging a reader in) is
/// read-only by design, and a scan that quietly began importing months of
/// timing data would not be read-only in spirit even though every byte written
/// is ours.
///
/// The counts are printed even when nothing was imported, because
/// `books_in_db` against `books_matched` is the whole answer to "why did this
/// import so little" — a device full of books none of which the library holds
/// is a linking problem, and a device with no statistics database at all is a
/// plugin that was never enabled. Those need different next moves, so they must
/// not print the same line.
pub async fn stats(engine: &Engine, path: &Path) -> Result<()> {
    let report = engine.import_device_statistics(path).await?;

    for w in &report.warnings {
        // Absence is ordinary here: no database, an unknown schema, a book the
        // library does not hold. None of it is an error and none of it stops
        // the rest, so these are warnings and the report below still prints.
        eprintln!("warning: {w}");
    }

    match report.schema_version {
        Some(v) => println!("{}: statistics database, schema {v}", path.display()),
        // Not "0 books": there was nothing to read, which is a different fact
        // from a database that held nothing.
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
        // A linking problem wearing the shape of an empty import. `ko scan`
        // is what shows which books the library does not hold.
        println!("    nothing here matches a book you have.");
        println!("    bookmole ko scan {}", path.display());
    }
    Ok(())
}

// ---- the plugin ------------------------------------------------------------

/// What is on the reader, and — when no reader is named — every reader we have
/// ever paired with.
///
/// The second half is the point of the `paired_devices` table: a reader in a
/// bag is still paired, and an answer that could only be produced by walking a
/// mount would have no way to say so.
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
            // Where and **when**, said apart. This line used to read "last seen
            // at <path>", labelling a place with a time word — and until item
            // 55 there was no honest date to print anyway, because
            // `last_seen_at` moved on install alone.
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
            // *Not since we started recording* rather than *never*: migration
            // `0020` arrived with no back-fill, because nothing recorded which
            // device a past sync read from.
            println!(
                "      everything brought across: {}",
                match d.last_synced_at {
                    Some(t) => crate::render::date(t),
                    None => "not since bookmole started recording it".to_string(),
                }
            );
            // **A push is not a sync, and this is the line that keeps that
            // honest rather than merely true.** `last_synced_at` deliberately
            // does not move when a reader pushes — a push carries what its
            // reading history holds and a sync is the whole volume — so with
            // only the line above, a reader that had just sent 491 highlights
            // over the LAN reported *nothing brought across, ever*, which reads
            // as a failure of the thing that had just worked. The column was
            // already written; nothing outside `ko listen`'s own live output
            // had ever read it.
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

    // The verdict is the engine's and the words are ours. This used to
    // compare `installed_version` against `our_version` here, a second
    // spelling of a domain rule that belongs in the engine — and every other
    // frontend would have needed a third.
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
        // Which obstruction it is comes from the three lines below, which is
        // why this one names none of them.
        PluginCondition::Obstructed => match status.installed_version {
            Some(v) if v > status.our_version => println!(
                "installed  : v{v} — newer than this bookmole (v{})",
                status.our_version
            ),
            Some(v) => println!("installed  : v{v}"),
            None => println!("installed  : yes"),
        },
    }

    // A reader can be paired with several computers, so the two
    // questions have come apart: `device_id` is *ours* and `pairings` is who
    // else is there. Naming the others matters — installing here adds a
    // computer rather than taking the reader over, and a line that said
    // "with another bookmole" would now be describing a state that is
    // ordinary rather than a conflict to resolve.
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

/// Install or upgrade the plugin. Explicit, never automatic, and it prints
/// where it is about to write before it writes — anything that writes to a
/// device that is not ours must show the path first, and a path shown afterwards is not the same promise.
pub async fn plugin_install(engine: &Engine, path: Option<&Path>, yes: bool) -> Result<()> {
    let mount = resolve_mount(path)?;
    let status = engine.plugin_status(&mount).await?;

    // Three verbs, not two. A reinstall of the same version is not an upgrade,
    // and saying so on a device that already has v1 reads as if bookmole
    // had something newer to give it.
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
    // `upgraded_from` is about the plugin **files** and `paired` is about the
    // **pairing**, and they are separate facts: a reader can already
    // carry the plugin because another computer put it there, in which case
    // "reinstalled, still paired as …" claims a relationship this machine has
    // never had. `status` is the reading from *before* the install, so it is
    // the only thing that can tell the difference.
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
        // The plugin was already there and none of its pairings was ours.
        // Nothing was taken over: the other computers' entries are untouched
        // and this one was added beside them.
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
    // The first wireless release said here "the reader has no address for bookmole yet, so
    // it sends nothing", which was true while there was no listener to name and
    // is now merely alarming: the plugin finds this computer by broadcast and by
    // hostname, and needs no address written into it. What is still true — and
    // is the whole posture — is that nothing crosses the LAN unless both ends
    // are asked, so say *that*, and say where the two asks are.
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
    // The plugin is one directory, so taking it off ends **every** computer's
    // pairing with this reader — and the other computers cannot be told,
    // because bookmole has no way to reach them. Their rows will go on
    // claiming a pairing the device no longer has. That is a real consequence
    // and it is the user's to know about, so it is said rather than left to be
    // discovered when a push stops working.
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

/// A y/n on stdin. Not a prompt library: this is the only question the CLI
/// asks, and it asks it because the alternative is writing to somebody's
/// hardware without saying so.
fn confirm() -> Result<bool> {
    use std::io::Write as _;
    print!("go ahead? [y/N] ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes"))
}

/// What to call a reader: its name, else the head of its id.
///
/// The fallback is short on purpose — a full uuid in a column of names reads as
/// noise, and `plugin_status` prints the whole id on the line beneath, which is
/// what `rename`/`forget` actually take.
fn device_name(d: &bookmole::PairedDevice) -> &str {
    match d.label.as_deref() {
        Some(l) => l,
        None => &d.device_id[..8.min(d.device_id.len())],
    }
}

/// Resolve a device id the user typed, allowing a unique prefix.
///
/// Uuids are not memorable and `ko plugin status` prints them in full beside a
/// name; asking for all 32 characters to rename something would make the verb
/// unusable without a clipboard. Ambiguity is a refusal that lists the
/// candidates rather than picking one.
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

/// Give a reader a name.
pub async fn plugin_rename(engine: &Engine, typed: &str, label: &str) -> Result<()> {
    let device = resolve_device(engine, typed).await?;
    let was = device_name(&device).to_string();
    engine.rename_device(&device.device_id, label).await?;

    // Re-read rather than echoing what was sent: a blank clears the name, and
    // what comes back is then the fallback rather than the empty string.
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

/// Forget a reader we do not have in hand.
///
/// The second line is not decoration. This drops our half of the pairing and
/// cannot reach the device, so the plugin and the token stay where they are —
/// and a message that said only *forgotten* would leave somebody believing a
/// reader they lent out had been cleaned.
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
