//! `bookmole sync`: turning sync on, syncing, and leaving. A recovery code never comes in as an
//! argument, where shell history and the process list would keep it, and goes out on stdout alone.

use std::io::{BufRead, IsTerminal};

use anyhow::{Result, anyhow, bail};
use bookmole::{Engine, EngineError, SyncError, SyncOutcome, SyncStatus};
use clap::Subcommand;

use crate::prompt;

const LABEL: &str = "This computer";

#[derive(Subcommand)]
pub enum SyncCmd {
    /// Whether this library syncs, and with what
    Status,
    /// Make a synced library on a sync server from this one, and print its recovery code
    ///
    /// The recovery code is all that goes to stdout. It is the only way to join another device,
    /// and whoever has it has the library.
    Create {
        /// The server's address: https://host[:port] (http:// is accepted too)
        server: String,
        /// What to call this device in `sync status`
        #[arg(long, default_value = LABEL)]
        label: String,
    },
    /// Join this library to a synced library, with its recovery code
    ///
    /// The code is read from a hidden prompt, or from the first line of stdin when that is not a
    /// terminal; it is never an argument. Every book here is kept and joins the library.
    Join {
        /// The server's address: https://host[:port] (http:// is accepted too)
        server: String,
        /// What to call this device in `sync status`
        #[arg(long, default_value = LABEL)]
        label: String,
    },
    /// Send what changed here and bring in what changed on other devices
    Now {
        /// Leave covers that changed on other devices for a later sync to fetch
        #[arg(long)]
        no_covers: bool,
    },
    /// Print the recovery code of the library this one syncs with
    RecoveryCode,
    /// Stop syncing on this device; every book, note and highlight here stays
    Leave {
        /// Skip the confirmation prompt
        #[arg(long)]
        yes: bool,
    },
}

pub async fn run(engine: &Engine, cmd: SyncCmd, quiet: bool) -> Result<()> {
    match cmd {
        SyncCmd::Status => status(engine).await,
        SyncCmd::Create { server, label } => create(engine, &server, &label, quiet).await,
        SyncCmd::Join { server, label } => join(engine, &server, &label).await,
        SyncCmd::Now { no_covers } => now(engine, no_covers, quiet).await,
        SyncCmd::RecoveryCode => recovery_code(engine, quiet).await,
        SyncCmd::Leave { yes } => leave(engine, yes).await,
    }
}

/// The engine's refusal, with the command that gets past it.
fn explain(e: EngineError) -> anyhow::Error {
    let EngineError::Sync(sync) = &e else {
        return e.into();
    };
    let next = match sync {
        SyncError::Off => {
            "turn it on with `bookmole sync create <server>`, or join a synced library with \
             `bookmole sync join <server>`"
        }
        SyncError::AlreadyOn => "`bookmole sync leave` first to sync with another library",
        SyncError::NeedsCredentials => NEEDS_CREDENTIALS,
        SyncError::CodeInvalid => "check the code and try again",
        SyncError::Unreachable => "check the address and the connection, then try again",
        SyncError::Refused { .. } => {
            "check the address; if this device was removed from the library, \
             `bookmole sync leave` and join again with the recovery code"
        }
        SyncError::ClockSkew => "set this computer's clock right, then try again",
        SyncError::SchemaMismatch => "update bookmole, then try again",
        SyncError::Interrupted => "try again",
        _ => return e.into(),
    };
    anyhow!("{e} — {next}")
}

const NEEDS_CREDENTIALS: &str = "`bookmole sync leave` stops syncing here and keeps everything; \
     then `bookmole sync join <server>` with the recovery code";

async fn status(engine: &Engine) -> Result<()> {
    match engine.sync_status().await? {
        SyncStatus::Off => println!(
            "sync is off for this library.\nturn it on with `bookmole sync create <server>`, \
             or join a synced library with `bookmole sync join <server>`."
        ),
        SyncStatus::NeedsCredentials => println!(
            "this library syncs, but this device's sync identity is missing from the data \
             directory, so nothing syncs.\n{NEEDS_CREDENTIALS}."
        ),
        SyncStatus::On(state) => {
            println!("syncs with {} as \"{}\"", state.server, state.device_label);
            println!("library {}", state.library_id);
            if state.log_position == 0 {
                println!("not synced yet — `bookmole sync now` syncs");
            } else {
                println!("read the server's log to position {}", state.log_position);
            }
            if let Some(problem) = state.last_problem {
                println!("the last sync stopped: {problem}");
            }
        }
    }
    Ok(())
}

async fn create(engine: &Engine, server: &str, label: &str, quiet: bool) -> Result<()> {
    let code = engine
        .create_sync_library(server, label)
        .await
        .map_err(explain)?;
    if !quiet {
        eprintln!(
            "this library now syncs; `bookmole sync now` sends it to the server.\n\
             below is its recovery code. `bookmole sync join <server>` on another device asks \
             for it, and whoever has it can read and change the library: keep it somewhere \
             safe. `bookmole sync recovery-code` shows it again."
        );
    }
    println!("{}", code.as_str());
    Ok(())
}

async fn join(engine: &Engine, server: &str, label: &str) -> Result<()> {
    let code = read_code()?;
    engine
        .join_sync_library(server, &code, label)
        .await
        .map_err(explain)?;
    println!(
        "joined. `bookmole sync now` brings in the library and sends the books that were here."
    );
    Ok(())
}

fn read_code() -> Result<String> {
    let code = if std::io::stdin().is_terminal() {
        rpassword::prompt_password("recovery code (input hidden): ")?
    } else {
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        line
    };
    let code = code.trim();
    if code.is_empty() {
        bail!(
            "no recovery code given — type it at the prompt, or pass it on stdin: \
             `bookmole sync join <server> < code.txt`"
        );
    }
    Ok(code.to_owned())
}

async fn now(engine: &Engine, no_covers: bool, quiet: bool) -> Result<()> {
    let outcome = engine.sync_now().await.map_err(explain)?;
    for line in summary(&outcome) {
        println!("{line}");
    }
    if !outcome.covers_to_fetch.is_empty() {
        if no_covers {
            println!(
                "covers changed on other devices; `bookmole sync now` without --no-covers \
                 fetches them."
            );
        } else {
            let fetched = fetch_covers(engine, &outcome.covers_to_fetch, quiet).await;
            if fetched > 0 {
                let covers = if fetched == 1 { "cover" } else { "covers" };
                println!("fetched {fetched} {covers} that changed on other devices.");
            }
        }
    }
    if !quiet {
        for line in warnings(&outcome) {
            eprintln!("{line}");
        }
    }
    Ok(())
}

async fn fetch_covers(engine: &Engine, books: &[i64], quiet: bool) -> usize {
    let mut fetched = 0;
    for &book in books {
        match engine.fetch_cover(book).await {
            Ok(Some(_)) => fetched += 1,
            Ok(None) => {}
            Err(e) => {
                if !quiet {
                    eprintln!("the cover of book #{book} was not fetched: {e}");
                }
            }
        }
    }
    fetched
}

/// What a sync did, for stdout. Says whether things happened, never how many.
fn summary(o: &SyncOutcome) -> Vec<String> {
    let mut lines = vec![
        match (o.sent, o.received) {
            (false, false) => "nothing to send or bring in.",
            (true, false) => "sent what changed here.",
            (false, true) => "brought in what changed on other devices.",
            (true, true) => "sent what changed here and brought in what changed on other devices.",
        }
        .to_string(),
    ];
    for c in &o.conflicts {
        let row = match &c.row.key {
            Some(key) => format!("{} {key}", c.row.collection),
            None => c.row.collection.clone(),
        };
        let kept = if c.remote_won {
            "another device's"
        } else {
            "this device's"
        };
        lines.push(format!(
            "{row} `{}` was edited here and on another device; the newer edit, {kept}, was kept.",
            c.field
        ));
    }
    if !o.refused_notes.is_empty() {
        let ids: Vec<String> = o.refused_notes.iter().map(|id| format!("#{id}")).collect();
        lines.push(format!(
            "notes from another device whose path leaves the vault are kept, but never written \
             to disk: {}",
            ids.join(", ")
        ));
    }
    lines
}

/// What a sync left for later, for stderr.
fn warnings(o: &SyncOutcome) -> Vec<&'static str> {
    let mut lines = Vec::new();
    if !o.held.is_empty() {
        lines.push(
            "some changes from other devices wait on others that have not arrived; the next \
             sync tries them again.",
        );
    }
    if o.blocked {
        lines.push(
            "reading stopped at a change from a device the server does not list yet; the next \
             sync tries again.",
        );
    }
    lines
}

async fn recovery_code(engine: &Engine, quiet: bool) -> Result<()> {
    let code = engine.sync_recovery_code().await.map_err(explain)?;
    if !quiet {
        eprintln!(
            "the recovery code of the library this one syncs with. whoever has it can read and \
             change the library: keep it somewhere safe, and give it only to `bookmole sync join`."
        );
    }
    println!("{}", code.as_str());
    Ok(())
}

async fn leave(engine: &Engine, yes: bool) -> Result<()> {
    if engine.sync_status().await? == SyncStatus::Off {
        engine.leave_sync_library().await.map_err(explain)?;
        println!("this library does not sync; there is nothing to leave.");
        return Ok(());
    }
    let question = "stop syncing on this device? every book, note and highlight here stays, \
                    and other devices keep theirs";
    if !yes && !prompt::confirm(question)? {
        println!("still syncing.");
        return Ok(());
    }
    engine.leave_sync_library().await.map_err(explain)?;
    println!(
        "this device no longer syncs. every book, note and highlight here stays.\n\
         `bookmole sync join <server>` with the recovery code joins again, as a new device."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use bookmole::{SyncConflict, SyncRow};

    use super::*;

    #[test]
    fn a_sync_that_moved_nothing_says_so_and_never_counts() {
        let quiet = summary(&SyncOutcome::default());
        assert_eq!(quiet, ["nothing to send or bring in."]);
        assert!(warnings(&SyncOutcome::default()).is_empty());
        let busy = SyncOutcome {
            sent: true,
            received: true,
            held: vec![SyncRow {
                collection: "highlights".into(),
                key: None,
            }],
            ..SyncOutcome::default()
        };
        let lines = summary(&busy).join("\n") + &warnings(&busy).join("\n");
        assert!(!lines.chars().any(|c| c.is_ascii_digit()), "{lines}");
    }

    #[test]
    fn a_conflict_names_the_field_and_whose_edit_was_kept() {
        let o = SyncOutcome {
            conflicts: vec![SyncConflict {
                row: SyncRow {
                    collection: "books".into(),
                    key: Some("[7]".into()),
                },
                field: "title".into(),
                remote_won: true,
            }],
            refused_notes: vec![12, 15],
            ..SyncOutcome::default()
        };
        let lines = summary(&o);
        assert!(lines[1].contains("books [7] `title`"), "{lines:?}");
        assert!(lines[1].contains("another device's"), "{lines:?}");
        assert!(lines[2].ends_with("#12, #15"), "{lines:?}");
    }
}
