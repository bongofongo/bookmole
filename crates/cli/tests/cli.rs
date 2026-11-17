//! The CLI as a process — the only thing that can observe what the binary
//! actually does.
//!
//! Everything else in this workspace tests the engine. `crates/cli` had tests
//! in `logging.rs` and nowhere else, so argument parsing, selector resolution,
//! the confirmation prompts and the "this command must not open the engine"
//! rules were all enforced by the code alone.
//!
//! Every child is started through `sandbox::command`, under a throwaway `HOME`.
//!
//! Offline throughout — nothing here searches, and the one import path used
//! (`ko pull`) reads a committed fixture.

mod sandbox;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SYNTHETIC: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../engine/tests/fixtures/koreader/synthetic"
);

/// A sandboxed invocation of the real binary.
struct Cli {
    root: tempfile::TempDir,
}

struct Out {
    ok: bool,
    stdout: String,
    stderr: String,
}

impl Cli {
    fn new() -> Cli {
        Cli {
            root: tempfile::tempdir().expect("tempdir"),
        }
    }

    /// The binary, sandboxed under this test's root.
    fn command(&self) -> Command {
        sandbox::command(self.root.path())
    }

    fn data_dir(&self) -> PathBuf {
        self.root.path().join("data")
    }

    fn try_run(&self, args: &[&str]) -> Out {
        self.try_run_in(&self.data_dir(), args)
    }

    /// The same, started from `cwd`.
    fn try_run_in(&self, cwd: &Path, args: &[&str]) -> Out {
        self.try_run_with_path(cwd, None, args)
    }

    /// The same, with `PATH` prefixed by `bin_dir`.
    ///
    /// This is the only place `PATH` can honestly be exercised: it belongs to a
    /// *child* process here, so setting it is a per-spawn value rather than a
    /// `set_var` racing every other test in the binary. It is what covers
    /// calibre feature detection finding a tool the ordinary way — the engine's
    /// own suite points `EngineConfig::calibre_bin_dir` at a directory instead,
    /// precisely to keep out of the process environment.
    fn try_run_with_path(&self, cwd: &Path, bin_dir: Option<&Path>, args: &[&str]) -> Out {
        let path = match bin_dir {
            Some(dir) => {
                let existing = std::env::var("PATH").unwrap_or_default();
                format!("{}:{existing}", dir.display())
            }
            None => std::env::var("PATH").unwrap_or_default(),
        };
        let out: Output = self
            .command()
            .env("PATH", path)
            .args(["--data-dir", self.data_dir().to_str().unwrap()])
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("the binary runs");
        Out {
            ok: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn run(&self, args: &[&str]) -> Out {
        let out = self.try_run(args);
        assert!(
            out.ok,
            "`bookmole {}` failed\n--- stdout ---\n{}\n--- stderr ---\n{}",
            args.join(" "),
            out.stdout,
            out.stderr
        );
        out
    }
}

impl Out {
    fn has(&self, needle: &str) -> &Out {
        assert!(
            self.stdout.contains(needle) || self.stderr.contains(needle),
            "expected {needle:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.stdout,
            self.stderr
        );
        self
    }

    fn lacks(&self, needle: &str) -> &Out {
        assert!(
            !self.stdout.contains(needle) && !self.stderr.contains(needle),
            "did not expect {needle:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.stdout,
            self.stderr
        );
        self
    }

    /// The one place these tests are coupled to output *formatting*, and it is
    /// deliberate: parsing an id out of one command and feeding it to the next
    /// is what lets everything else assert on the contract *between* commands
    /// rather than on their wording. `render::book_line` prints `#<id>  …`.
    fn book_id(&self) -> String {
        self.stdout
            .split('#')
            .nth(1)
            .and_then(|rest| {
                let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
                (!digits.is_empty()).then_some(digits)
            })
            .unwrap_or_else(|| panic!("no `#<id>` in output:\n{}", self.stdout))
    }
}

/// Copy a committed sidecar fixture into a writable tree.
fn place(dst_root: &Path, fixture: &str) -> PathBuf {
    let src = Path::new(SYNTHETIC).join(fixture);
    let dst = dst_root.join(fixture);
    std::fs::create_dir_all(&dst).unwrap();
    let mut sidecar = None;
    for entry in std::fs::read_dir(&src).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        std::fs::copy(entry.path(), dst.join(&name)).unwrap();
        let n = name.to_string_lossy().into_owned();
        if n.starts_with("metadata.") && n.ends_with(".lua") {
            sidecar = Some(dst.join(n));
        }
    }
    sidecar.expect("fixture has a sidecar")
}

/// The subcommand set is a promise, not a detail — so this one golden is right
/// where a golden of whole output would be wrong.
///
/// Comparing the *names* rather than the help text makes it immune to clap's
/// formatting and to every cosmetic change, while still failing when a
/// subcommand appears, vanishes or is renamed without anyone deciding to.
#[test]
fn the_subcommand_set_is_what_we_decided() {
    let cli = Cli::new();
    let help = cli.run(&["--help"]).stdout;

    // Everything indented by exactly two spaces in clap's Commands block, up to
    // the first whitespace.
    let mut found: Vec<&str> = help
        .lines()
        .skip_while(|l| !l.starts_with("Commands:"))
        .skip(1)
        .take_while(|l| l.starts_with("  ") && !l.trim().is_empty())
        .filter_map(|l| l.split_whitespace().next())
        .filter(|name| name.chars().all(|c| c.is_ascii_lowercase() || c == '-'))
        .collect();
    found.sort_unstable();
    found.dedup();

    let expected = [
        "activity",
        "add",
        "calibre",
        "cards",
        "cite",
        "config",
        "covers",
        "enrich",
        "epub",
        "find",
        "goodreads",
        "help",
        "highlights",
        "ko",
        "links",
        "list",
        "merge",
        "note",
        "notes",
        "progress",
        "rating",
        "reflect",
        "repl",
        "review",
        "rm",
        "search",
        "set",
        "show",
        "sort-keys",
        "toc",
    ];
    assert_eq!(
        found, expected,
        "the subcommand set changed. If that was deliberate, update this list \
         in the same commit — that is the point of it."
    );
}

/// `config` must not open the engine, and therefore must not create `database/`.
///
/// That is a rule of the CLI (configuring a key must not create a library as a
/// side effect) and the code respects it, but nothing could
/// *observe* it: from inside the process the difference between "opened the
/// engine" and "didn't" is invisible, while from outside it is one directory.
#[test]
fn config_never_opens_the_engine() {
    let cli = Cli::new();
    let scratch = cli.root.path().join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();

    cli.try_run_in(&scratch, &["config", "show"]);

    assert!(
        !scratch.join("database").exists(),
        "`config` created a database/ in the working directory"
    );
    assert!(
        !cli.data_dir().join("database").exists(),
        "`config` created a database/ in the data dir"
    );
}

/// An empty library says so, and creates its data dir where it was told rather
/// than where it happened to be run.
#[test]
fn an_empty_library_says_so_and_writes_only_where_told() {
    let cli = Cli::new();
    let scratch = cli.root.path().join("scratch2");
    std::fs::create_dir_all(&scratch).unwrap();

    cli.try_run_in(&scratch, &["list"]).has("library is empty");

    assert!(cli.data_dir().join("database/app.db").is_file());
    assert!(
        !scratch.join("database").exists(),
        "--data-dir was ignored and the cwd was used instead"
    );
}

/// Run the binary with **no** `--data-dir` and no `BOOKMOLE_DATA_DIR`, from
/// `cwd`, so the library lands at the default location under the sandbox's
/// `HOME`.
fn run_without_data_dir(cli: &Cli, cwd: &Path, args: &[&str]) -> Out {
    let out = cli
        .command()
        .args(args)
        .current_dir(cwd)
        .env_remove("BOOKMOLE_DATA_DIR")
        .output()
        .expect("the binary runs");
    let out = Out {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    };
    assert!(
        out.ok,
        "`bookmole {}` failed\n--- stdout ---\n{}\n--- stderr ---\n{}",
        args.join(" "),
        out.stdout,
        out.stderr
    );
    out
}

/// Where the default library lives under a given home, spelled out per
/// platform rather than asked of the resolver this test is checking.
fn default_library_under(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/bookmole")
    } else {
        home.join(".local/share/bookmole")
    }
}

/// Two working directories, one library.
///
/// The bug this pins: the data root used to fall back to the cwd, so every
/// directory a binary was run from was a separate, empty library. Now a book
/// pulled from one directory is listed from another, and both live under the
/// platform data dir of `HOME` — with nothing written into either cwd.
#[test]
fn two_working_directories_see_one_library_at_the_default_location() {
    let cli = Cli::new();
    let a = cli.root.path().join("cwd-a");
    let b = cli.root.path().join("cwd-b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let device = cli.root.path().join("device");
    let sidecar = place(&device, "Gen-Summary.sdr");

    run_without_data_dir(&cli, &a, &["list"]).has("library is empty");
    let library = default_library_under(&cli.root.path().join("home"));
    assert!(
        library.join("database/app.db").is_file(),
        "no library at {}",
        library.display()
    );

    run_without_data_dir(&cli, &a, &["ko", "pull", sidecar.to_str().unwrap()]);
    run_without_data_dir(&cli, &b, &["list"]).has("A Rated Book");

    for cwd in [&a, &b] {
        assert!(
            !cwd.join("database").exists(),
            "{} grew a library of its own",
            cwd.display()
        );
    }
    assert!(
        !cli.data_dir().join("database").exists(),
        "the sandbox's --data-dir was never passed, so nothing belongs there"
    );
}

/// With no home there is no default, and the binary says so rather than
/// quietly making a library wherever it was started.
#[test]
fn no_home_is_an_error_not_a_library_in_the_cwd() {
    let cli = Cli::new();
    let cwd = cli.root.path().join("homeless");
    std::fs::create_dir_all(&cwd).unwrap();
    let out = cli
        .command()
        .arg("list")
        .current_dir(&cwd)
        .env_remove("BOOKMOLE_DATA_DIR")
        .env_remove("XDG_DATA_HOME")
        .env_remove("APPDATA")
        .env_remove("HOME")
        .output()
        .expect("the binary runs");
    assert!(!out.status.success(), "ran with no home and no data dir");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("BOOKMOLE_DATA_DIR"), "{stderr}");
    assert!(!cwd.join("database").exists(), "fell back to the cwd");
}

/// Pull a book off a fixture sidecar, set progress, read it back.
///
/// Three commands agreeing about one book, which is the contract worth pinning:
/// `ko pull` reports an id, `progress` accepts it, `show` reflects what
/// `progress` set. It also exercises the offline import path end to end through
/// the binary — `epub` is unusable here, since a found ISBN goes to the
/// providers and this suite makes no network calls.
#[test]
fn pull_then_progress_then_show_agree_about_the_same_book() {
    let cli = Cli::new();
    let device = cli.root.path().join("device");
    let sidecar = place(&device, "Gen-Summary.sdr");

    cli.run(&["ko", "pull", sidecar.to_str().unwrap()]);

    let listed = cli.run(&["list"]);
    listed.has("A Rated Book");
    let id = listed.book_id();

    cli.run(&["progress", &id, "120"]);
    cli.run(&["show", &id]).has("120").has("A Rated Book");
}

/// A confirmation prompt reading EOF must decline.
///
/// This is the one that would actually hurt. `prompt::confirm` treats anything
/// but `y`/`yes` as no, so a closed stdin declines — but nothing asserted it,
/// and the opposite reading (EOF as "assume yes") is a plausible thing for
/// someone to change while making a command scriptable. It would destroy a
/// library from a cron job.
#[test]
fn a_destructive_command_declines_when_it_cannot_ask() {
    let cli = Cli::new();
    let device = cli.root.path().join("device");
    let sidecar = place(&device, "Gen-Summary.sdr");
    cli.run(&["ko", "pull", sidecar.to_str().unwrap()]);
    let id = cli.run(&["list"]).book_id();

    cli.run(&["rm", &id]).has("kept.");
    cli.run(&["list"]).has("A Rated Book");

    // …and with an explicit --yes it goes through, so the guard above is the
    // prompt declining rather than `rm` being broken.
    cli.run(&["rm", &id, "--yes"]);
    cli.run(&["list"]).has("library is empty");
}

/// `reflect --show` on a book with no reflection must not create one — and, the
/// part that is easy to get wrong, must not open a *reading* either.
///
/// `open_reflection` resolves its anchor through `ensure_reading`, so a
/// read-only inspection that reached it would quietly start reading a book the
/// user only looked at. `--show` exists so that opening one is never forced
/// through an editor, and this is what keeps it read-only.
///
/// The second half — actually opening it — is not decoration. Without it the
/// two `lacks` assertions above would also pass against a `reflect` that did
/// nothing at all, which is the failure mode a negative assertion invites.
#[test]
fn reflect_show_creates_neither_a_note_nor_a_reading() {
    let cli = Cli::new();
    let device = cli.root.path().join("device");
    let sidecar = place(&device, "Unmatched.sdr");
    cli.run(&["ko", "pull", sidecar.to_str().unwrap()]);
    let id = cli.run(&["list"]).book_id();

    cli.run(&["reflect", &id, "--show"])
        .has("no reflection yet");
    cli.run(&["notes"]).lacks("Reflection:");
    // `show` lists a book's readings; the pull created none, and looking at the
    // reflection must not either.
    cli.run(&["show", &id]).lacks("reading 1/1");

    // Now open it for real, and both appear — so the absences above were real.
    cli.run(&["reflect", &id, "--no-edit"]);
    cli.run(&["notes"]).has("Reflection:");
    cli.run(&["show", &id]).has("reading 1/1");
}

/// `links` reads the graph in both directions, and says so about the half that
/// is not there yet.
///
/// The first note's `[[Vocation]]` is written before any note called Vocation exists — a
/// forward reference, which the engine back-resolves when the target appears.
/// That is the property `Storage::backlinks` is a plain `WHERE to_note = ?`
/// because of, and this is it observed from outside the process: if
/// back-resolution ever stopped happening, the inbound half of the *first*
/// note's output would silently go empty.
///
/// The dangling `[[Lydgate]]` is the other half of the contract: a target with no
/// note is printed as the text it is, never dropped. A note you have not
/// written yet is a place to go, not an error.
#[test]
fn links_reads_both_directions_and_prints_a_dangling_target_as_text() {
    let cli = Cli::new();

    cli.run(&[
        "note",
        "Her whole life is [[Vocation]].",
        "--title",
        "Dorothea",
    ]);
    cli.run(&[
        "note",
        "Grief with no bottom. Cf. [[Dorothea]], and [[Lydgate]] later.",
        "--title",
        "Vocation",
    ]);

    // By title, which is what a wikilink names — and the forward reference
    // resolved, so it is a real edge in both directions.
    cli.run(&["links", "Dorothea"])
        .has("links out:")
        .has("“Vocation”")
        .has("links in:");

    let han = cli.run(&["links", "Vocation"]);
    han.has("“Dorothea”");
    han.has("“Lydgate”").has("text");

    // Nothing resolves to Lydgate, so it is not a note at all.
    let missing = cli.try_run(&["links", "Lydgate"]);
    assert!(!missing.ok, "a selector matching no note must not exit 0");
    missing.has("no note matches");
}

/// Calibre is feature-detected off `PATH`, and the binary finds it there.
///
/// The engine's own suite points `EngineConfig::calibre_bin_dir` at a directory
/// of fakes, deliberately, because `set_var("PATH")` from a test is a data race.
/// That leaves the `PATH` half — the one every real user actually takes —
/// unasserted, and this is where it can be asserted safely: the environment
/// belongs to the child.
///
/// The *absent* half is deliberately not here. Detection falls through to `PATH`
/// and then to the directories calibre installs itself in — which is right, and
/// which makes "no calibre" unreachable from outside the process on a machine
/// that has it. The wording of that branch has a rule attached to it
/// (bookmole never asks the user to install or configure other
/// software), so it is asserted where it is deterministic:
/// `commands::calibre::tests::an_absent_calibre_is_reported_and_never_prescribed`.
#[cfg(unix)]
#[test]
fn calibre_is_found_on_path_and_both_tiers_run_through_the_binary() {
    use std::os::unix::fs::PermissionsExt;

    let cli = Cli::new();
    let bin = cli.root.path().join("fakebin");
    std::fs::create_dir_all(&bin).unwrap();
    let lib = cli.root.path().join("callib");
    std::fs::create_dir_all(&lib).unwrap();
    // The marker the engine insists on before it will run calibredb — without
    // it a mistyped path has calibre *create* a library there.
    std::fs::write(lib.join("metadata.db"), b"").unwrap();

    let write = |name: &str, body: &str| {
        let p = bin.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    write("ebook-convert", "cp \"$1\" \"$2\"");
    write(
        "calibredb",
        r#"cat <<'EOF'
[{"id": 1, "uuid": "aaaa-bbbb", "title": "War and Peace", "authors": "Leo Tolstoy & Constance Garnett", "tags": ["fiction"]}]
EOF"#,
    );

    let with_fakes = |args: &[&str]| cli.try_run_with_path(&cli.data_dir(), Some(&bin), args);

    // Found on PATH, and the status line says what that enables.
    let status = with_fakes(&["calibre", "status"]);
    assert!(status.ok, "status must not fail\n{}", status.stderr);
    status
        .has("ebook-convert")
        .has("calibredb")
        .has("conversion and library import are available.");

    // Tier (ii), through the real binary: the fake's JSON becomes a book.
    with_fakes(&["calibre", "import", "--library", lib.to_str().unwrap()]).has("War and Peace");
    cli.run(&["list"]).has("War and Peace");

    // Tier (i): the two paths reach `ebook-convert`, and a second run is
    // refused with the flag that would allow it.
    let input = cli.root.path().join("in.epub");
    let output = cli.root.path().join("out.azw3");
    std::fs::write(&input, b"epub").unwrap();
    with_fakes(&[
        "calibre",
        "convert",
        input.to_str().unwrap(),
        output.to_str().unwrap(),
    ])
    .has("out.azw3");
    assert!(output.is_file());
    with_fakes(&[
        "calibre",
        "convert",
        input.to_str().unwrap(),
        output.to_str().unwrap(),
    ])
    .has("--force");
}

/// A selector that matches nothing is an error, and an under-specified sync
/// writes nothing and names the flag that would.
///
/// Both are deliberate behaviour: an unmatched `--book` selector is an error,
/// never a silent no-op, and `ko sync` with neither `--all` nor `--book` prints
/// what would happen and names the flag rather than writing forty books
/// unasked.
#[test]
fn a_bad_selector_fails_loudly_and_an_underspecified_sync_writes_nothing() {
    let cli = Cli::new();
    let bad = cli.try_run(&["show", "zzz-no-such-book"]);
    assert!(!bad.ok, "an unmatched selector must not exit 0");
    bad.has("zzz-no-such-book");

    // A mount with one book on it, and a sync that was not told what to do.
    let device = cli.root.path().join("mount");
    place(&device, "Gen-Summary.sdr");

    let vague = cli.run(&["ko", "sync", device.to_str().unwrap()]);
    vague.has("--all");
    cli.run(&["list"]).has("library is empty");
}

/// The activity log, end to end — and the two rules it exists to keep.
///
/// **One: an empty log is not "you did not read", it is "nobody has built it
/// yet".** No importer fills `reading_events` (deliberately — a log that filled
/// itself as a side effect would be whichever importer ran last), so a fresh
/// library reports nothing and has to name the move that changes that. A dead
/// end — a screen or a line with no move out of it — is what bookmole never
/// prints.
///
/// **Two: absence is not zero.** A sidecar's highlight stamps say which days you
/// were in the book and measure nothing at all, so the minutes total must read
/// as unmeasured. `0` there is a false statement about the user's own reading,
/// and it is one `unwrap_or(0)` away at every layer between the SQL and this
/// line.
#[test]
fn an_empty_activity_log_names_the_move_and_a_refill_measures_nothing_it_did_not() {
    let cli = Cli::new();
    let device = cli.root.path().join("device");
    let sidecar = place(&device, "Gen-Summary.sdr");
    cli.run(&["ko", "pull", sidecar.to_str().unwrap()]);
    let id = cli.run(&["list"]).book_id();

    // Nothing has filled the log, so it says so and names what does.
    cli.run(&["activity", "--from", "2020-01-01", "--to", "2035-01-01"])
        .has("nothing recorded here yet")
        .has("activity --refill");

    let refilled = cli.run(&[
        "activity",
        "--refill",
        "--days",
        "--from",
        "2020-01-01",
        "--to",
        "2035-01-01",
    ]);
    refilled.has("rebuilt from what was already here");
    // Highlight stamps place the days. They do not time them.
    refilled.has("not measured").lacks("nothing recorded here");
    // Nothing counts what was not done — no streak, no "n of 30".
    refilled.lacks("streak").lacks("goal");

    // The per-book view says who supplied each day and how much it claims —
    // and here that is the sharpest form of the rule: these days are
    // `measured`, because a reading's own endpoints are dates somebody
    // recorded, and they still carry **no minutes**. Confidence is about the
    // day, not about the columns beside it, and a renderer that read
    // `measured` as "so there is a number" would print a zero nobody measured.
    cli.run(&["activity", "--book", &id])
        .has("koreader")
        .has("measured")
        .has("—");
}

/// A book with no owned file has **no chapter list to read**, which is not the
/// same answer as an epub carrying no TOC — and the command has to say which.
#[test]
fn a_book_with_no_epub_says_there_is_no_file_rather_than_no_chapters() {
    let cli = Cli::new();
    let device = cli.root.path().join("device");
    let sidecar = place(&device, "Gen-Summary.sdr");
    cli.run(&["ko", "pull", sidecar.to_str().unwrap()]);
    let id = cli.run(&["list"]).book_id();

    cli.run(&["toc", &id])
        .has("no epub here")
        .has("bookmole epub");
}

/// `set` writes the subject, series and series-index columns, echoes them, and
/// refuses half a pair — a number with no series to belong to.
#[test]
fn set_writes_the_series_pair_and_refuses_half_of_it() {
    let cli = Cli::new();
    let device = cli.root.path().join("device");
    let sidecar = place(&device, "Gen-Summary.sdr");
    cli.run(&["ko", "pull", sidecar.to_str().unwrap()]);
    let id = cli.run(&["list"]).book_id();

    let orphan = cli.try_run(&["set", &id, "--series-index", "2"]);
    assert!(!orphan.ok, "an index with no series is not a correction");
    assert!(orphan.stderr.contains("in no series"), "{}", orphan.stderr);

    cli.run(&[
        "set",
        &id,
        "--series",
        "Dune",
        "--series-index",
        "2",
        "--subject",
        "Fiction / Literary",
    ])
    .has("series")
    .has("Fiction / Literary");

    // The pair reads as one fact, written the way a person writes it.
    cli.run(&["show", &id])
        .has("Dune #2")
        .has("Fiction / Literary");

    // …and numbering a book whose series is already recorded is allowed, which
    // is the ordinary use of the flag the refusal above must not block.
    cli.run(&["set", &id, "--series-index", "3"])
        .has("series_index");
    cli.run(&["show", &id]).has("Dune #3");
}

/// The cover back-fill has a door, and it is reachable from the binary.
///
/// `Engine::measure_stored_covers` once existed with no caller — tested on the
/// facade, unreachable from outside the
/// process, and therefore never run over the library a shelf would be built
/// against. That is the same failure as `list --sort author`: the engine can do
/// it and nothing can ask. The counts are the facade test's business
/// (`engine_facade.rs`); what only this can observe is that the verb exists,
/// opens the engine and exits 0.
///
/// A book pulled from a sidecar has no cover, so the work list is empty — and
/// the empty answer must not be a zero.
#[test]
fn the_cover_back_fill_is_reachable_from_the_binary() {
    let cli = Cli::new();
    let device = cli.root.path().join("device");
    let sidecar = place(&device, "Gen-Summary.sdr");
    cli.run(&["ko", "pull", sidecar.to_str().unwrap()]);

    cli.run(&["covers"])
        .has("already measured")
        .lacks("0 covers");
}

// ---- the search door -------------------------------------------------------

/// The whole point of `rb find`: a highlight that arrived through the KOReader
/// path is findable from a terminal.
///
/// Through the binary rather than the facade, because that is the claim — the
/// engine could search and, until this command, nothing outside the process
/// could ask it to. That is the failure `measure_stored_covers` and
/// `list --sort author` both had.
#[test]
fn an_imported_highlight_is_findable_from_the_command_line() {
    let cli = Cli::new();
    let device = cli.root.path().join("device");
    let sidecar = place(&device, "Gen-Summary.sdr");
    cli.run(&["ko", "pull", sidecar.to_str().unwrap()]);

    cli.run(&["find", "passage"])
        .has("highlight")
        .has(">>passage<<");

    // The narrowing is a filter over the same list, not a second search.
    cli.run(&["find", "passage", "--notes"])
        .has("no notes match");
    cli.run(&["find", "passage", "--highlights"])
        .has(">>passage<<");
}

/// Absence is an answer, in all three of its shapes: nothing matched, nothing
/// was asked, and something was typed that fts5 reads as syntax.
///
/// The last one is a live defect this item fixed rather than a hypothetical —
/// `notes --search "don't"` used to fail with a raw database error, because the
/// query went into `MATCH` unquoted.
#[test]
fn a_search_that_finds_nothing_is_never_an_error() {
    let cli = Cli::new();
    let device = cli.root.path().join("device");
    let sidecar = place(&device, "Gen-Summary.sdr");
    cli.run(&["ko", "pull", sidecar.to_str().unwrap()]);

    let none = cli.run(&["find", "thermodynamics"]);
    none.has("no notes or highlights match");
    assert!(
        !none.stdout.contains('0'),
        "absence is not zero: {}",
        none.stdout
    );

    cli.run(&["find", "   "]).has("nothing to search for");
    // Two things fts5 would have raised a syntax error on.
    cli.run(&["find", "don't"])
        .has("no notes or highlights match");
    cli.run(&["find", "C++"])
        .has("no notes or highlights match");
}

/// The sort-key back-fill has a door too, and the same argument put it there.
///
/// The sort-key back-fill has a sharper edge than the cover one: until it runs, a book
/// files at the *top* of `list --sort author` rather than under its author, so a
/// function nothing calls is not merely unexercised but visibly wrong. The verb
/// exists, opens the engine, exits 0 — and, because every book the engine writes
/// is filed on the way in, the answer here is the already-done branch, which
/// must not be a zero.
///
/// `make dev-db` runs this against a library seeded by raw SQL, which is the
/// only place the counting branch is reachable.
#[test]
fn the_sort_key_back_fill_is_reachable_from_the_binary() {
    let cli = Cli::new();
    let device = cli.root.path().join("device");
    let sidecar = place(&device, "Gen-Summary.sdr");
    cli.run(&["ko", "pull", sidecar.to_str().unwrap()]);

    cli.run(&["sort-keys"])
        .has("already files")
        .lacks("0 books");
}

/// `cards export` writes Anki's text-import format, and a second run with no
/// new cards writes nothing and names `--all` rather than an empty file.
#[test]
fn cards_export_writes_anki_headers_and_a_stable_guid() {
    let cli = Cli::new();
    let device = cli.root.path().join("device");
    let sidecar = place(&device, "Middlemarch.sdr");
    cli.run(&["ko", "pull", sidecar.to_str().unwrap()]);

    let out = cli.root.path().join("deck.txt");
    cli.run(&["cards", "export", "--out", out.to_str().unwrap()])
        .has("1 card ->")
        .has("deck.txt");
    let first = std::fs::read_to_string(&out).unwrap();
    let lines: Vec<&str> = first.lines().collect();
    assert_eq!(
        &lines[..6],
        [
            "#separator:tab",
            "#html:true",
            "#notetype:Basic",
            "#deck column:3",
            "#tags column:4",
            "#guid column:5",
        ]
    );
    assert_eq!(lines.len(), 7, "{first}");
    assert_eq!(lines[6].split('\t').count(), 5, "{first}");

    cli.run(&["cards", "export", "--out", out.to_str().unwrap()])
        .has("nothing written: every card had gone out in an earlier file")
        .has("--all");
    assert_eq!(std::fs::read_to_string(&out).unwrap(), first, "rewritten");

    let again = cli.root.path().join("again.txt");
    cli.run(&["cards", "export", "--all", "--out", again.to_str().unwrap()]);
    let guid = |t: &str| {
        t.lines()
            .nth(6)
            .unwrap()
            .rsplit('\t')
            .next()
            .unwrap()
            .to_string()
    };
    assert_eq!(
        guid(&first),
        guid(&std::fs::read_to_string(&again).unwrap())
    );
}

/// Run the binary against `data`, under the sandbox's own `HOME`, with `extra`
/// flags first. For the cloud-folder line, whose answer depends on where the
/// data dir sits relative to `HOME`.
fn run_at(cli: &Cli, data: &Path, extra: &[&str], args: &[&str]) -> Out {
    let out = cli
        .command()
        .args(extra)
        .args(["--data-dir", data.to_str().unwrap()])
        .args(args)
        .output()
        .expect("the binary runs");
    let out = Out {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    };
    assert!(
        out.ok,
        "`bookmole {}` failed\n--- stdout ---\n{}\n--- stderr ---\n{}",
        args.join(" "),
        out.stdout,
        out.stderr
    );
    out
}

/// A library in a synced folder opens, and the binary says so once, on stderr
/// only; `--quiet` silences it; a library anywhere else hears nothing.
///
/// `~/Dropbox` because it is a synced root on both macOS and Linux, the two
/// platforms this suite runs on, and `HOME` is the sandbox's, so no real
/// synced folder is touched.
#[test]
fn a_library_in_a_synced_folder_is_named_once_on_stderr() {
    const SAID: &str = "The library is in Dropbox";
    let cli = Cli::new();
    let synced = cli.root.path().join("home/Dropbox/rb");

    let out = run_at(&cli, &synced, &[], &["list"]);
    out.has("library is empty");
    assert_eq!(out.stderr.matches(SAID).count(), 1, "{}", out.stderr);
    assert!(
        !out.stdout.contains(SAID),
        "stdout is for scripts: {}",
        out.stdout
    );
    assert!(out.stderr.contains("BOOKMOLE_DATA_DIR"), "{}", out.stderr);
    assert!(synced.join("database/app.db").is_file(), "it opened anyway");

    run_at(&cli, &synced, &["--quiet"], &["list"]).lacks(SAID);
    run_at(&cli, &cli.data_dir(), &[], &["list"]).lacks("The library is in");
}
