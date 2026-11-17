//! What this crate may link. A host links it in-process, so every dependency
//! here is one that host carries too.

use std::process::Command;

const ALLOWED: &[&str] = &["bookmole", "serde", "serde_json", "ts-rs"];

#[test]
fn the_api_depends_only_on_the_engine_and_serde() {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .args([
            "metadata",
            "--no-deps",
            "--offline",
            "--format-version",
            "1",
        ])
        .arg("--manifest-path")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .output()
        .expect("run cargo metadata");
    assert!(
        out.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout).expect("metadata json");
    let package = meta["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .find(|p| p["name"] == env!("CARGO_PKG_NAME"))
        .expect("this package in the metadata");

    let linked: Vec<&str> = package["dependencies"]
        .as_array()
        .expect("dependencies")
        .iter()
        .filter(|d| d["kind"] != "dev")
        .map(|d| d["name"].as_str().expect("dependency name"))
        .collect();
    let extra: Vec<&&str> = linked.iter().filter(|d| !ALLOWED.contains(d)).collect();
    assert!(
        extra.is_empty(),
        "bookmole-api may depend only on {ALLOWED:?}; it also depends on {extra:?}"
    );
}
