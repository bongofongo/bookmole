//! The TUI finds the same library wherever it is started from.
//!
//! The data root used to fall back to the current directory, so a TUI opened
//! from two different directories looked at two different, empty libraries.
//! Now the default is the platform data dir under `HOME`
//! (`crates/engine/src/data_dir.rs`), and this runs the real binary from two
//! working directories to show it.
//!
//! `--dump-frame` is the one TUI path that needs no terminal: it opens the
//! engine, renders the book once to stdout and exits.

mod sandbox;

use std::path::{Path, PathBuf};

use bookmole::{Book, Engine, EngineConfig};

/// Spelled out per platform rather than asked of the resolver under test.
fn default_library_under(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/bookmole")
    } else {
        home.join(".local/share/bookmole")
    }
}

fn dumped_title(root: &Path, cwd: &Path, book_id: i64) -> String {
    let out = sandbox::command(root)
        .args(["--dump-frame", "8x4", "--book", &book_id.to_string()])
        .current_dir(cwd)
        .env_remove("BOOKMOLE_DATA_DIR")
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

    assert_eq!(dumped_title(root.path(), &a, id), "Two Rooms, One Shelf");
    assert_eq!(dumped_title(root.path(), &b, id), "Two Rooms, One Shelf");
    for cwd in [&a, &b] {
        assert!(
            !cwd.join("database").exists(),
            "{} grew a library of its own",
            cwd.display()
        );
    }
}
