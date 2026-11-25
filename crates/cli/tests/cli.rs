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

    /// As `try_run_in`, with `PATH` prefixed by `bin_dir`: a per-child value, not a `set_var` racing
    /// other tests.
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

    /// Parses the id from `render::book_line`'s `#<id>  …`, so tests assert the contract between
    /// commands.
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

/// Compares subcommand names, not help text, so only an added, removed or renamed command fails.
#[test]
fn the_subcommand_set_is_what_we_decided() {
    let cli = Cli::new();
    let help = cli.run(&["--help"]).stdout;

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
        "sync",
        "toc",
    ];
    assert_eq!(
        found, expected,
        "the subcommand set changed. If that was deliberate, update this list \
         in the same commit — that is the point of it."
    );
}

/// `config` must not open the engine, so must not create `database/`; observable only from outside.
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

/// Spelled out per platform rather than asked of the resolver under test.
fn default_library_under(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/bookmole")
    } else {
        home.join(".local/share/bookmole")
    }
}

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

/// EOF at a confirmation prompt must decline; reading it as yes would destroy a library unattended.
#[test]
fn a_destructive_command_declines_when_it_cannot_ask() {
    let cli = Cli::new();
    let device = cli.root.path().join("device");
    let sidecar = place(&device, "Gen-Summary.sdr");
    cli.run(&["ko", "pull", sidecar.to_str().unwrap()]);
    let id = cli.run(&["list"]).book_id();

    cli.run(&["rm", &id]).has("kept.");
    cli.run(&["list"]).has("A Rated Book");

    cli.run(&["rm", &id, "--yes"]);
    cli.run(&["list"]).has("library is empty");
}

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
    cli.run(&["show", &id]).lacks("reading 1/1");

    cli.run(&["reflect", &id, "--no-edit"]);
    cli.run(&["notes"]).has("Reflection:");
    cli.run(&["show", &id]).has("reading 1/1");
}

#[test]
fn a_failed_editor_keeps_the_draft_and_says_where() {
    let cli = Cli::new();
    let tmp = cli.root.path().join("tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    let out = cli
        .command()
        .env("EDITOR", "false")
        .env("TMPDIR", &tmp)
        .args(["--data-dir", cli.data_dir().to_str().unwrap(), "note"])
        .output()
        .expect("the binary runs");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    let kept: Vec<_> = std::fs::read_dir(&tmp)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert!(
        stderr.contains(&format!("the draft is kept at {}", kept[0].display())),
        "{stderr}"
    );
    cli.run(&["notes"]).lacks("Untitled");
}

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

    cli.run(&["links", "Dorothea"])
        .has("links out:")
        .has("“Vocation”")
        .has("links in:");

    let han = cli.run(&["links", "Vocation"]);
    han.has("“Dorothea”");
    han.has("“Lydgate”").has("text");

    let missing = cli.try_run(&["links", "Lydgate"]);
    assert!(!missing.ok, "a selector matching no note must not exit 0");
    missing.has("no note matches");
}

/// The binary finds calibre on `PATH`. The absent branch is asserted in `commands::calibre::tests`,
/// where it is deterministic.
#[cfg(unix)]
#[test]
fn calibre_is_found_on_path_and_both_tiers_run_through_the_binary() {
    use std::os::unix::fs::PermissionsExt;

    let cli = Cli::new();
    let bin = cli.root.path().join("fakebin");
    std::fs::create_dir_all(&bin).unwrap();
    let lib = cli.root.path().join("callib");
    std::fs::create_dir_all(&lib).unwrap();
    // The marker the engine requires before running calibredb; without it a mistyped path makes
    // calibre create a library.
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

    let status = with_fakes(&["calibre", "status"]);
    assert!(status.ok, "status must not fail\n{}", status.stderr);
    status
        .has("ebook-convert")
        .has("calibredb")
        .has("conversion and library import are available.");

    with_fakes(&["calibre", "import", "--library", lib.to_str().unwrap()]).has("War and Peace");
    cli.run(&["list"]).has("War and Peace");

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

#[test]
fn a_bad_selector_fails_loudly_and_an_underspecified_sync_writes_nothing() {
    let cli = Cli::new();
    let bad = cli.try_run(&["show", "zzz-no-such-book"]);
    assert!(!bad.ok, "an unmatched selector must not exit 0");
    bad.has("zzz-no-such-book");

    let device = cli.root.path().join("mount");
    place(&device, "Gen-Summary.sdr");

    let vague = cli.run(&["ko", "sync", device.to_str().unwrap()]);
    vague.has("--all");
    cli.run(&["list"]).has("library is empty");
}

#[test]
fn an_empty_activity_log_names_the_move_and_a_refill_measures_nothing_it_did_not() {
    let cli = Cli::new();
    let device = cli.root.path().join("device");
    let sidecar = place(&device, "Gen-Summary.sdr");
    cli.run(&["ko", "pull", sidecar.to_str().unwrap()]);
    let id = cli.run(&["list"]).book_id();

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
    refilled.has("not measured").lacks("nothing recorded here");
    refilled.lacks("streak").lacks("goal");

    // These days are `measured` yet carry no minutes: confidence is about the day, not the columns
    // beside it.
    cli.run(&["activity", "--book", &id])
        .has("koreader")
        .has("measured")
        .has("—");
}

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

    cli.run(&["show", &id])
        .has("Dune #2")
        .has("Fiction / Literary");

    cli.run(&["set", &id, "--series-index", "3"])
        .has("series_index");
    cli.run(&["show", &id]).has("Dune #3");
}

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

#[test]
fn an_imported_highlight_is_findable_from_the_command_line() {
    let cli = Cli::new();
    let device = cli.root.path().join("device");
    let sidecar = place(&device, "Gen-Summary.sdr");
    cli.run(&["ko", "pull", sidecar.to_str().unwrap()]);

    cli.run(&["find", "passage"])
        .has("highlight")
        .has(">>passage<<");

    cli.run(&["find", "passage", "--notes"])
        .has("no notes match");
    cli.run(&["find", "passage", "--highlights"])
        .has(">>passage<<");
}

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
    cli.run(&["find", "don't"])
        .has("no notes or highlights match");
    cli.run(&["find", "C++"])
        .has("no notes or highlights match");
}

/// The sort-key back-fill verb exists, opens the engine and exits 0; every book is filed on the way
/// in, so the already-done line must not be a zero.
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

/// `~/Dropbox` under the sandbox `HOME` is a synced root on macOS and Linux; no real folder is
/// touched.
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
