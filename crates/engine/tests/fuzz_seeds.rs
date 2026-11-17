//! Replays the fuzz seed corpus on stable; drop a crashing input into `fuzz/seeds/<target>/`
//! to add a regression.

use std::path::{Path, PathBuf};

use bookmole::koreader::parse_sidecar;

fn seeds(target: &str) -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fuzz/seeds")
        .join(target);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    out.sort();
    out
}

/// Every seed must be handled without panicking and within a time bound: looping forever is a
/// different failure from returning an error.
#[test]
fn every_parse_sidecar_seed_is_survivable() {
    let seeds = seeds("parse_sidecar");
    assert!(
        !seeds.is_empty(),
        "no seeds found — fuzz/seeds/parse_sidecar/ should be committed"
    );

    let started = std::time::Instant::now();
    for path in &seeds {
        let Ok(src) = std::fs::read_to_string(path) else {
            continue;
        };
        let per_case = std::time::Instant::now();
        let _ = parse_sidecar(&src);
        assert!(
            per_case.elapsed() < std::time::Duration::from_secs(30),
            "{} took {:?} — the instruction budget is not holding",
            path.display(),
            per_case.elapsed()
        );
    }
    eprintln!(
        "replayed {} parse_sidecar seeds in {:?}",
        seeds.len(),
        started.elapsed()
    );
}

#[test]
fn no_seed_can_reach_the_lua_standard_library() {
    for name in ["sandbox-escape.lua", "require.lua"] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fuzz/seeds/parse_sidecar")
            .join(name);
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        let err = parse_sidecar(&src)
            .err()
            .unwrap_or_else(|| panic!("{name} was accepted — the sandbox is open"));
        // It must fail because the function does not exist, not because the shape was rejected later.
        let msg = err.to_string();
        assert!(
            msg.contains("lua eval") || msg.contains("nil value"),
            "{name} failed for an unexpected reason: {msg}"
        );
    }
}

#[test]
fn every_epub_seed_is_survivable() {
    for path in seeds("epub_info") {
        let _ = bookmole::epub::epub_info(&path);
    }
}
