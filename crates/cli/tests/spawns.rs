//! Every test that starts a workspace binary goes through its crate's
//! `tests/sandbox/mod.rs`, which points `HOME`, the XDG directories and the
//! data dir at a temp dir. A binary started any other way opens the user's real
//! library, so this reads the test sources and fails on any other spawn.

use std::path::{Path, PathBuf};

// Built from halves so this file does not match itself.
const BIN_ENV: &str = concat!("CARGO_BIN", "_EXE_");
const SPAWN: &str = concat!("Command", "::new");

fn workspace_crates() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn is_sandbox(path: &Path) -> bool {
    path.ends_with("tests/sandbox/mod.rs")
}

fn is_test_source(crates: &Path, path: &Path) -> bool {
    path.strip_prefix(crates)
        .ok()
        .and_then(|rel| rel.components().nth(1))
        .is_some_and(|c| c.as_os_str() == "tests")
}

#[test]
fn only_the_sandbox_helpers_start_a_binary() {
    let crates = workspace_crates();
    let mut files = Vec::new();
    rust_files(&crates, &mut files);
    assert!(
        files.iter().filter(|f| is_sandbox(f)).count() >= 2,
        "the sandbox helpers moved; this scan no longer knows where they are"
    );

    let mut offenders = Vec::new();
    for file in &files {
        if is_sandbox(file) {
            continue;
        }
        let text = std::fs::read_to_string(file).unwrap();
        for (n, line) in text.lines().enumerate() {
            let spawns =
                line.contains(BIN_ENV) || (is_test_source(&crates, file) && line.contains(SPAWN));
            if spawns {
                offenders.push(format!("{}:{}: {}", file.display(), n + 1, line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "start binaries through the crate's tests/sandbox/mod.rs:\n{}",
        offenders.join("\n")
    );
}
