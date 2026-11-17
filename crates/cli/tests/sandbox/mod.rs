//! The only way a test in this crate starts the binary.
//!
//! Run without `--data-dir` and with the real `HOME`, the binary opens the
//! user's own library and config. Every child made here gets `HOME`, the XDG
//! directories, `APPDATA` and `BOOKMOLE_DATA_DIR` inside `root` instead, so a
//! test can drop `BOOKMOLE_DATA_DIR` to exercise the default location and still
//! land in the sandbox. `tests/spawns.rs` fails if a binary is started anywhere
//! else.

use std::path::Path;
use std::process::{Command, Stdio};

pub const BIN: &str = env!("CARGO_BIN_EXE_bookmole");

/// `bookmole` under `root`: `root/home` is `HOME`, `root/config` is
/// `XDG_CONFIG_HOME`, `root/data` is `BOOKMOLE_DATA_DIR`, and `root` is the cwd.
pub fn command(root: &Path) -> Command {
    let home = root.join("home");
    for dir in [&home, &root.join("config"), &root.join("data")] {
        std::fs::create_dir_all(dir).expect("sandbox dirs");
    }
    let mut cmd = Command::new(BIN);
    cmd.current_dir(root)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("APPDATA", home.join("AppData/Roaming"))
        .env("BOOKMOLE_DATA_DIR", root.join("data"))
        // clap reads the key from the environment, and a key would let `search`
        // reach the network.
        .env_remove("GOOGLE_BOOKS_API_KEY")
        .env_remove("RUST_LOG")
        // An editor that cannot read its terminal would hang the suite.
        .env("EDITOR", "true")
        .env("VISUAL", "true")
        .stdin(Stdio::null());
    cmd
}
