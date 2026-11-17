//! The TUI finds the same library wherever it is started from.
//!
//! The data root used to fall back to the current directory, so a TUI opened
//! from two different directories looked at two different, empty libraries.
//! Now the default is the platform data dir under `HOME`
//! (`crates/engine/src/data_dir.rs`), and this runs the real binary from two
//! working directories to show it.
//!
//! `--dump-frame` is the one TUI path that needs no terminal: it opens the
//! engine, renders the book once to stdout and exits. **`HOME` is always a temp
//! dir here** — without it the binary would open the developer's real library.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use bookmole::{Book, Engine, EngineConfig};

const BIN: &str = env!("CARGO_BIN_EXE_bookmole-tui");

/// Spelled out per platform rather than asked of the resolver under test.
fn default_library_under(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/bookmole")
    } else {
        home.join(".local/share/bookmole")
    }
}

/// The first line `--dump-frame` prints is the title of the book it drew.
fn dumped_title(home: &Path, cwd: &Path, book_id: i64) -> String {
    let out = Command::new(BIN)
        .args(["--dump-frame", "8x4", "--book", &book_id.to_string()])
        .current_dir(cwd)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env_remove("BOOKMOLE_DATA_DIR")
        .env_remove("XDG_DATA_HOME")
        .env_remove("APPDATA")
        .env_remove("GOOGLE_BOOKS_API_KEY")
        .env_remove("RUST_LOG")
        .stdin(Stdio::null())
        .output()
        .expect("the binary runs");
    assert!(
        out.status.success(),
        "--dump-frame from {} failed: {}",
        cwd.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test]
async fn two_working_directories_see_one_library_at_the_default_location() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let a = root.path().join("cwd-a");
    let b = root.path().join("cwd-b");
    for d in [&home, &a, &b] {
        std::fs::create_dir_all(d).unwrap();
    }

    // Seed the default location directly, as a library that already exists.
    let library = default_library_under(&home);
    let id = {
        let engine = Engine::open(EngineConfig::rooted_at(&library))
            .await
            .unwrap();
        let book = Book {
            title: Some("Two Rooms, One Shelf".into()),
            ..Book::default()
        };
        engine.save_book(&book).await.unwrap().id.unwrap()
    };

    assert_eq!(dumped_title(&home, &a, id), "Two Rooms, One Shelf");
    assert_eq!(dumped_title(&home, &b, id), "Two Rooms, One Shelf");
    for cwd in [&a, &b] {
        assert!(
            !cwd.join("database").exists(),
            "{} grew a library of its own",
            cwd.display()
        );
    }
}
