//! Installing bookmole's KOReader plugin onto a mounted reader.
//!
//! **This is the only code in the engine that writes to somebody else's
//! hardware**, and it is a module of its own for that reason — the same reason
//! `device.rs` was split out of `koreader.rs`. A scan is not an import, and an
//! install is not a scan: `device.rs` is read-only by construction and its
//! whole test suite rests on that. If you want to audit the claim that
//! bookmole touches nothing on a reader but its own directory, this file is
//! the only one you have to read.
//!
//! # What the KOReader source settles
//!
//! Read from `koreader/master`. As everywhere this crate reads a KOReader
//! format, the source is the specification and a fixture is only ever
//! evidence.
//!
//! - **`pluginloader.lua:178-200`** — plugins load from `DEFAULT_PLUGIN_PATH`
//!   (`plugins/`) plus `extra_plugin_paths`. On a device
//!   `datastorage.lua:getDataDir()` returns `"."` (cwd is the install
//!   directory) and the loader registers the extra path only `if data_dir ~=
//!   "."`. So on a device `{data_dir}/plugins/` **is** `koreader/plugins/`, and
//!   we never write `extra_plugin_paths` — that would be editing the user's
//!   `settings.reader.lua`, and nothing outside our own plugin directory is
//!   ever written; it would also buy nothing. On a desktop KOReader the same
//!   directory is `$XDG_CONFIG_HOME/koreader/plugins/` (or
//!   `$KO_HOME/plugins/`), which is the development loop and is **not** what
//!   these functions target.
//! - **A directory is a plugin if it matches `".+%.koplugin"`** and holds
//!   `main.lua`. `_meta.lua` is optional, its fields are merged into the module
//!   except `name`, and the loader gates on none of them — so our `version`
//!   rides along inert.
//! - **Our directory survives a KOReader OTA update.** The update is a plain
//!   tar unpack with no `--delete`; the only removal is a manifest diff,
//!   `grep -xvFf .../ota/package.index /tmp/package.index | xargs -r rm -vf`,
//!   identical in the Kobo, Kindle, Cervantes and remarkable startup scripts.
//!   It deletes files that were in the *old* KOReader manifest and are absent
//!   from the *new* one; a path that never appeared in any KOReader manifest is
//!   untouched. This is what makes the install durable, and it is written down
//!   here so nobody has to re-derive it.
//! - **`kosync.koplugin` keeps its credentials in `settings/kosync.lua`**,
//!   outside its own directory. That is the one convention we break: it would
//!   leave a token behind after an uninstall we have promised is exact.
//!
//! # Why sha256 and never mtime
//!
//! These are FAT32 volumes. Two-second timestamp granularity, no permissions,
//! no symlinks, case-insensitive. Anything resting on mtime is subtly wrong on
//! real hardware and perfectly fine in `tempfile`, which is the worst of both.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::device::{is_koreader_mount, koreader_dir};
use crate::error::Result;
use crate::files::sha256_of;
use crate::koreader::{eval_table, get_int, get_str, sandboxed_lua};

/// The directory we own on a reader. Nothing outside it is ever written.
pub const PLUGIN_DIR_NAME: &str = "bookmole.koplugin";

/// The version of the plugin this binary carries. An integer, because
/// `_meta.lua` is compared with `>` and nothing about a plugin on a reader
/// needs semver's ordering rules.
///
/// **2 since the network half**, which grew `main.lua` from a 73-line
/// pairing stub to the push/pull client. Bumping it is not bookkeeping: a
/// reader carrying the version-1 plugin is a reader that cannot push, and left
/// at 1 every surface we have — `ko plugin status`, a frontend's device view —
/// reports it *up to date* and tells the user there is nothing to do. The
/// number is the only thing that can tell those two plugins apart, since
/// neither the manifest nor the menu is read from off the device.
///
/// **3 since linking over wifi.** A version-2 plugin has no code to show and
/// keeps one address per computer, so a reader carrying it cannot be paired
/// without a cable — and *up to date* would again be the wrong thing to tell
/// the user.
pub const PLUGIN_VERSION: i64 = 3;

/// Written by us at install, **and by the plugin** when a computer is linked or
/// unlinked over wifi; read by the plugin at startup.
///
/// Two writers since plugin version 3, which is why [`write_pairings`] and the
/// plugin's `serialisePairings` are pinned byte-for-byte against each other:
/// the installer reads this file back and rewrites its own entry, and if the
/// two disagreed about spacing every connection would rewrite the other's
/// work. It stays unhashed for the reason it always was — its content is a
/// fact about a relationship, not a file we shipped.
const PAIRING_FILE: &str = "pairing.lua";
/// Written by us at install; read by us at uninstall. Never read on-device.
const MANIFEST_FILE: &str = "installed.lua";
/// Written by **the plugin, on the device**; never by us, never hashed.
///
/// The learned half of the discovery ladder — where a computer was last
/// actually reached — as against `pairing.lua`, which is what we told the
/// reader over the cable. They are two files because they have two authors,
/// and the split is what makes the manifest check mean anything: every byte in
/// `installed.lua` is a byte we wrote, so a mismatch is a person's edit.
///
/// It must be **skipped by [`inspect`] and removed by [`uninstall`]**, and that
/// is not tidiness. `inspect` files anything absent from the manifest under
/// `unrecognised`, and `refuse_if_obstructed` turns that into a refusal of
/// *install and uninstall both* — so a plugin that cached its endpoint beside
/// itself would brick its own installer the first time it ran.
const ENDPOINT_FILE: &str = "endpoint.lua";

/// The three names in our directory that the manifest does not describe.
///
/// One list rather than three comparisons, because `inspect` and `uninstall`
/// have to agree about it exactly: a name skipped by one and not the other is
/// either a file that blocks an uninstall or a file left behind by one.
const UNHASHED_FILES: [&str; 3] = [MANIFEST_FILE, PAIRING_FILE, ENDPOINT_FILE];

/// Whether `name` is something the *operating system* left in our directory
/// while we were writing to it — not a file of ours, and not a person's.
///
/// **Found by the first install ever made from a Mac** (2026-11-11, onto a real
/// Kindle). macOS tags every file it writes with a `com.apple.provenance`
/// attribute; a FAT volume cannot hold attributes, so the kernel stores each
/// one in an AppleDouble sibling — `._main.lua` beside `main.lua`. Five files
/// written, five shadows, and [`inspect`] filed all five under `unrecognised`:
/// the installer had obstructed itself, and would have refused the next
/// install *and* the next uninstall on a reader nobody had touched.
///
/// The rule is deliberately narrow. Only the shadow of **a file we own** —
/// one we ship, or one of [`UNHASHED_FILES`] — is litter; `._notes.txt` is the
/// shadow of somebody's `notes.txt` and stays their business, as does anything
/// else we cannot account for. `.DS_Store` is the same species (Finder writes
/// it into any folder somebody looks at) and carries nothing of a person's.
fn is_os_litter(name: &str) -> bool {
    if name == ".DS_Store" {
        return true;
    }
    name.strip_prefix("._").is_some_and(|shadowed| {
        UNHASHED_FILES.contains(&shadowed) || FILES.iter().any(|(ours, _)| *ours == shadowed)
    })
}

/// Take the operating system's litter back out of our directory. Best-effort:
/// a shadow that will not delete is an annoyance, not a failed install, and
/// [`inspect`] ignores it either way.
fn sweep_os_litter(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if is_os_litter(&entry.file_name().to_string_lossy()) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The plugin, embedded.
///
/// `include_str!` and an explicit list rather than an `include_dir` dependency:
/// a new crate buys nothing here, and the shipped file list being a
/// compile-time constant is exactly what an exact uninstall needs.
const FILES: &[(&str, &str)] = &[
    (
        "_meta.lua",
        include_str!("../koplugin/bookmole.koplugin/_meta.lua"),
    ),
    (
        "main.lua",
        include_str!("../koplugin/bookmole.koplugin/main.lua"),
    ),
];

/// Why an install or an uninstall refused.
///
/// A refusal is a decision, not a fault, and every arm here is one of the
/// install's safety rules (verify the mount is really KOReader, write only our
/// own directory, never overwrite a newer version, uninstall exactly what we
/// installed) turned into something a caller can branch on. It is an
/// enum rather than an `EngineError::Other(String)` for exactly that reason.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PluginRefusal {
    /// The path is not a KOReader install. The gate `device.rs` was built for.
    #[error("{} is not a KOReader install", path.display())]
    NotAKoreaderMount { path: PathBuf },
    /// The mount is a symlink. `device.rs`'s rule, applied to a write.
    #[error("{} is a symlink, not a mounted reader", path.display())]
    MountIsASymlink { path: PathBuf },
    /// A newer plugin than ours is already there.
    #[error(
        "a newer plugin is already installed (version {installed}; this bookmole carries {ours})"
    )]
    NewerAlreadyInstalled { installed: i64, ours: i64 },
    /// Files of ours that were edited on the device since we wrote them.
    #[error("these files were edited on the device since bookmole wrote them: {}", paths.join(", "))]
    Modified { paths: Vec<String> },
    /// Something is in our directory that we never put there.
    #[error("these files are in the plugin directory and bookmole did not put them there: {}", paths.join(", "))]
    Unrecognised { paths: Vec<String> },
    /// Installed, but not by a bookmole that left a manifest behind.
    #[error(
        "{} holds a plugin with no bookmole manifest, so it cannot be removed exactly",
        path.display()
    )]
    NoManifest { path: PathBuf },
}

/// What is on the reader, and what we know about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginStatus {
    pub mount: PathBuf,
    /// Exactly where an install would write. Shown to the user *before* it
    /// happens, because installing is never automatic, and a path in a prompt
    /// is the difference between an explicit action and an automatic one.
    pub plugin_dir: PathBuf,
    pub installed: bool,
    pub installed_version: Option<i64>,
    pub our_version: i64,
    /// The device's `pairing.lua` names a device we have a row for.
    pub paired: bool,
    /// **Our** id for this reader, when one of its pairings is ours.
    ///
    /// [`inspect`] cannot tell whose is whose — it has no database — so it
    /// fills this with the file's first entry, which is exactly what it always
    /// meant while a reader could hold only one. `Engine::plugin_status`
    /// re-points it at the entry it actually has a row for, so a reader paired
    /// with somebody else's computer reports `None` here rather than a
    /// stranger's id.
    pub device_id: Option<String>,
    /// Every computer this reader is paired with, ours included, in file order.
    ///
    /// Not `device_id`'s replacement: that one answers *which of these is us*,
    /// and this answers *who else is there* — the question the single-computer
    /// file could not represent, and the reason a second install used to kill
    /// the first one's pairing in silence.
    pub pairings: Vec<Pairing>,
    /// Files of ours whose bytes no longer match the manifest.
    pub modified: Vec<String>,
    /// Files in our directory that are not ours.
    pub unrecognised: Vec<String>,
}

/// What an install onto this reader would do — the one question a screen
/// branches on.
///
/// The three predicates below it are still the truth and still overlap by
/// design; this collapses them into the **single mutually-exclusive** answer a
/// frontend needs, so that comparing `installed_version` against `our_version`
/// happens once, here, rather than once in the CLI's `ko` commands and again
/// in every other frontend. A derived fact spelled twice is how two frontends
/// come to disagree, and the CLI had already grown the first spelling.
///
/// **`Obstructed` wins over every version case**, because it is the one that
/// gates the action: a reader carrying a plugin we edited, a stranger's file in
/// our directory, or a version newer than ours is a reader we will not write
/// to, and *which* of those it is comes from `modified` / `unrecognised` /
/// `installed_version` beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginCondition {
    /// Nothing of ours is there. An install writes.
    Absent,
    /// Ours, at the version we carry. An install rewrites it in place.
    Current,
    /// Ours, older. An install upgrades.
    Upgradable,
    /// Ours, carrying no version at all — a `_meta.lua` we could not read a
    /// number out of. An install rewrites it, and this is deliberately not
    /// folded into `Upgradable`: "older than ours" is a fact and "we cannot
    /// tell" is not the same fact.
    Unversioned,
    /// We will not write here until the user resolves it.
    Obstructed,
}

impl PluginStatus {
    /// What an install here would do. See [`PluginCondition`].
    pub fn condition(&self) -> PluginCondition {
        if self.is_obstructed() {
            return PluginCondition::Obstructed;
        }
        if !self.installed {
            return PluginCondition::Absent;
        }
        match self.installed_version {
            None => PluginCondition::Unversioned,
            Some(v) if v < self.our_version => PluginCondition::Upgradable,
            // `>` was taken by `is_obstructed` above, so this is equality.
            Some(_) => PluginCondition::Current,
        }
    }

    /// An install here would land on top of a plugin that is already there.
    ///
    /// Says nothing about *versions* — a reinstall of the same version is one
    /// of these too, and calling that an upgrade is what made the CLI print
    /// "upgraded v1 → v1" on a real device. Ask [`Self::is_version_upgrade`]
    /// when the distinction is the point.
    pub fn is_reinstall(&self) -> bool {
        self.installed
    }

    /// An install here would replace an *older* plugin with ours.
    pub fn is_version_upgrade(&self) -> bool {
        self.installed && self.installed_version.is_some_and(|v| v < self.our_version)
    }

    /// Nothing bookmole can do to this reader until the user resolves it.
    pub fn is_obstructed(&self) -> bool {
        !self.modified.is_empty()
            || !self.unrecognised.is_empty()
            || self.installed_version.is_some_and(|v| v > self.our_version)
    }
}

/// What an install did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallReport {
    pub plugin_dir: PathBuf,
    pub device_id: String,
    pub version: i64,
    /// Relative paths written, sorted. `pairing.lua` and the manifest included:
    /// an install's whole footprint, so a caller can print it.
    pub written: Vec<String>,
    pub upgraded_from: Option<i64>,
}

/// What an uninstall did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UninstallReport {
    pub plugin_dir: PathBuf,
    pub removed: Vec<String>,
    /// The pairing row we dropped, if there was one.
    ///
    /// [`uninstall`] fills it from the file's first entry and
    /// `Engine::uninstall_plugin` re-points it, for [`PluginStatus::device_id`]'s
    /// reason: only the database knows which of a reader's computers we are.
    pub forgot_device: Option<String>,
    /// Every pairing this uninstall destroyed, ours and other computers'.
    ///
    /// Taking the plugin off ends *all* of them, because the file goes with it
    /// — so the report says so rather than naming only the one we had a row
    /// for. Nothing else on this volume is touched, which is the promise the
    /// whole-tree snapshot test checks.
    pub removed_pairings: Vec<String>,
}

/// Where our plugin lives on a mount, whether or not it is there yet.
///
/// `None` when the path is not a KOReader install — the same three layouts
/// `koreader_dir` knows (Kindle `koreader/`, Kobo `.adds/koreader`, PocketBook
/// `applications/koreader`).
pub fn plugin_dir(mount: &Path) -> Option<PathBuf> {
    koreader_dir(mount).map(|k| k.join("plugins").join(PLUGIN_DIR_NAME))
}

/// The gate. Every public entry point calls this first, and it is the only
/// place the two refusals live.
fn check_mount(mount: &Path) -> Result<PathBuf> {
    // A symlink is checked before the contents, because the contents of a
    // symlink's target can be a perfectly good KOReader install that lives
    // somewhere we were never pointed at.
    if mount
        .symlink_metadata()
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err(PluginRefusal::MountIsASymlink {
            path: mount.to_path_buf(),
        }
        .into());
    }
    if !is_koreader_mount(mount) {
        return Err(PluginRefusal::NotAKoreaderMount {
            path: mount.to_path_buf(),
        }
        .into());
    }
    plugin_dir(mount).ok_or_else(|| {
        PluginRefusal::NotAKoreaderMount {
            path: mount.to_path_buf(),
        }
        .into()
    })
}

// ---- the four files we can read back off a device --------------------------

/// `path` → sha256, as recorded at install time.
type Manifest = BTreeMap<String, String>;

fn read_manifest(dir: &Path) -> Result<Option<(i64, Manifest)>> {
    let path = dir.join(MANIFEST_FILE);
    let Ok(src) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    let lua = sandboxed_lua()?;
    let root = eval_table(&lua, &src, MANIFEST_FILE)?;
    let version = get_int(&root, "version").unwrap_or(0);
    let mut files = Manifest::new();
    if let Ok(mlua::Value::Table(t)) = root.get::<mlua::Value>("files") {
        for pair in t.pairs::<String, String>().flatten() {
            files.insert(pair.0, pair.1);
        }
    }
    Ok(Some((version, files)))
}

fn write_manifest(dir: &Path, version: i64, files: &Manifest) -> Result<()> {
    let mut s = String::from(
        "-- Written by bookmole. What it installed, and the sha256 of each\n\
         -- file as written, so that removing the plugin can remove exactly what\n\
         -- was put here and nothing a person added afterwards.\n\
         return {\n",
    );
    s.push_str(&format!("    version = {version},\n    files = {{\n"));
    for (name, hash) in files {
        s.push_str(&format!("        [\"{name}\"] = \"{hash}\",\n"));
    }
    s.push_str("    },\n}\n");
    std::fs::write(dir.join(MANIFEST_FILE), s)?;
    Ok(())
}

fn read_installed_version(dir: &Path) -> Result<Option<i64>> {
    let Ok(src) = std::fs::read_to_string(dir.join("_meta.lua")) else {
        return Ok(None);
    };
    let lua = sandboxed_lua()?;
    let root = eval_table(&lua, &src, "_meta.lua")?;
    Ok(get_int(&root, "version"))
}

/// One computer this reader is paired with, as `pairing.lua` records it.
///
/// **It carries no token**, and that is structural rather than an omission:
/// this type hangs off [`PluginStatus`], which crosses into a DTO, so a secret
/// field here would reach the wire the first time somebody surfaced the list.
/// The token stays in [`StoredPairing`], which is private to this module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pairing {
    /// Who this reader is **to that computer**. Each computer mints its own,
    /// so this doubles as the handle a computer recognises its own entry by.
    pub device_id: String,
    /// What to call the computer. The machine's hostname at install time, and
    /// `None` for an entry written before there was one — a menu falls back to
    /// the id's first bytes rather than inventing a name.
    pub name: Option<String>,
    pub paired_at: Option<i64>,
}

/// A [`Pairing`] plus the secret. Never leaves this module.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredPairing {
    device_id: String,
    token: String,
    name: Option<String>,
    paired_at: i64,
}

impl StoredPairing {
    fn public(&self) -> Pairing {
        Pairing {
            device_id: self.device_id.clone(),
            name: self.name.clone(),
            paired_at: Some(self.paired_at),
        }
    }
}

/// `s` as a Lua string literal.
///
/// Every value written here has been hex or nothing until now, so the old
/// `format!("\"{device_id}\"")` was safe by accident. It stops being safe the
/// moment an entry carries a **name**: a hostname is not our string, and one
/// containing a quote would write a `pairing.lua` that does not parse — which
/// the reader reports as *not paired* and the installer, reading its own
/// output back, reports as a fresh device. Escaping is the cheaper end of that.
fn lua_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            // A literal NUL is legal in a Lua string and unreadable in a file
            // somebody may open on a device; the numeric escape is both.
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\{:03}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Every computer the reader's `pairing.lua` names, with their tokens.
///
/// **Two shapes are accepted and only one is written.** The version-1 plugin
/// wrote a flat table holding a single `device_id`/`token`/`paired_at`, on the
/// reasoning that there was one computer; that was a bug, since a second
/// bookmole install overwrote the file and killed the first one's pairing
/// while its `paired_devices` row still claimed it. So the file is now
/// a list under `computers`, and a flat table is read as a one-entry list —
/// detected by *shape* rather than by a version number, because a number
/// nothing branches on is decoration and the reader's Lua has to make the same
/// test anyway.
///
/// An entry missing an id or a token is dropped rather than repaired: half a
/// credential proves nothing, and keeping it would let the installer decide it
/// had found its own entry and then write a token beside somebody else's name.
fn read_stored_pairings(dir: &Path) -> Result<Vec<StoredPairing>> {
    let Ok(src) = std::fs::read_to_string(dir.join(PAIRING_FILE)) else {
        return Ok(Vec::new());
    };
    let lua = sandboxed_lua()?;
    let root = eval_table(&lua, &src, PAIRING_FILE)?;

    let entry = |t: &mlua::Table| -> Option<StoredPairing> {
        let device_id = get_str(t, "device_id").filter(|s| !s.is_empty())?;
        let token = get_str(t, "token").filter(|s| !s.is_empty())?;
        Some(StoredPairing {
            device_id,
            token,
            name: get_str(t, "name").filter(|s| !s.is_empty()),
            paired_at: get_int(t, "paired_at").unwrap_or(0),
        })
    };

    if let Ok(mlua::Value::Table(list)) = root.get::<mlua::Value>("computers") {
        let mut out = Vec::new();
        for value in list.sequence_values::<mlua::Value>().flatten() {
            if let mlua::Value::Table(t) = value
                && let Some(p) = entry(&t)
            {
                out.push(p);
            }
        }
        return Ok(out);
    }
    Ok(entry(&root).into_iter().collect())
}

/// Write the whole list back.
///
/// There is no partial write: the file is small, it is ours, and rewriting it
/// whole is what lets the installer keep another computer's entry byte-for-byte
/// without parsing its way around it.
fn write_pairings(dir: &Path, computers: &[StoredPairing]) -> Result<()> {
    // Still no `host` and no `port` in an entry, and now for a sharper reason
    // than the first wireless release's "there is no listener yet": the endpoint the device *learns*
    // is the device's fact and lives in `endpoint.lua`, which the device owns
    // and we never hash. `name` is the wired hint — a hostname is stable across
    // a DHCP lease, which is the rung of the ladder an address cannot be.
    let mut body = String::from(
        "-- Written by bookmole: by the installer over USB, and by the plugin\n\
         -- on this device when a computer is linked or unlinked over wifi. Do not\n\
         -- edit.\n\
         --\n\
         -- A *list*, because a reader can be paired with more than one computer.\n\
         -- Each entry is one computer: the id this reader has to that computer,\n\
         -- the secret it proves itself with, and a name to show in the menu. A\n\
         -- writer changes its own entry and leaves every other one alone.\n\
         return {\n\
         \x20   computers = {\n",
    );
    for c in computers {
        body.push_str("        {\n");
        body.push_str(&format!(
            "            device_id = {},\n",
            lua_quote(&c.device_id)
        ));
        body.push_str(&format!(
            "            token     = {},\n",
            lua_quote(&c.token)
        ));
        if let Some(name) = &c.name {
            body.push_str(&format!("            name      = {},\n", lua_quote(name)));
        }
        body.push_str(&format!("            paired_at = {},\n", c.paired_at));
        body.push_str("        },\n");
    }
    body.push_str("    },\n}\n");
    std::fs::write(dir.join(PAIRING_FILE), body)?;
    Ok(())
}

/// One address of one computer, as `endpoint.lua` records it.
///
/// A computer has **several** since plugin version 3 — a laptop lives on a home
/// network and an office one — so this is a row of a list, and a `Vec` of them
/// keeps the file's own order within each `device_id`. That order is the order
/// the reader tries them in, and it is positional rather than sorted by
/// `seen_at` on purpose: the installer stamps an address it has not *reached*,
/// and that address still belongs first.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredEndpoint {
    device_id: String,
    host: String,
    /// The **TCP** port a listener last announced, and `0` for "not known".
    ///
    /// Zero is what an install writes, and it costs nothing: the cached rung
    /// probes the *rendezvous* port like every other rung, and the TCP port is
    /// announced in the reply. The field is kept because the plugin's own writer
    /// has it and the two writers must produce the same bytes.
    port: u16,
    seen_at: i64,
    /// A person typed this address into the reader. The plugin never drops one
    /// to make room, and neither may we: it is the answer to a network where
    /// nothing can be discovered, so losing it on a plugin upgrade would undo
    /// the one thing the user did by hand.
    typed: bool,
}

/// Read `endpoint.lua`, through the same sandbox every other on-device file
/// goes through. Absent, unreadable and empty are one answer: no endpoints.
///
/// **Two shapes, detected by shape.** Version 2 kept one flat
/// `{ host, port, seen_at }` per computer; that is a one-entry list, exactly as
/// the version-1 flat `pairing.lua` is a one-entry list of computers.
fn read_endpoints(dir: &Path) -> Result<Vec<StoredEndpoint>> {
    let path = dir.join(ENDPOINT_FILE);
    let Ok(body) = std::fs::read_to_string(&path) else {
        return Ok(Vec::new());
    };
    let lua = sandboxed_lua()?;
    let Ok(root) = eval_table(&lua, &body, ENDPOINT_FILE) else {
        return Ok(Vec::new());
    };
    let address = |device_id: &str, t: &mlua::Table| -> Option<StoredEndpoint> {
        let host = get_str(t, "host").filter(|h| !h.is_empty())?;
        Some(StoredEndpoint {
            device_id: device_id.to_string(),
            host,
            port: get_int(t, "port").unwrap_or(0).clamp(0, 65535) as u16,
            seen_at: get_int(t, "seen_at").unwrap_or(0),
            typed: matches!(
                t.get::<mlua::Value>("typed"),
                Ok(mlua::Value::Boolean(true))
            ),
        })
    };
    let mut ids = Vec::new();
    for pair in root.pairs::<String, mlua::Value>().flatten() {
        if let (device_id, mlua::Value::Table(t)) = pair {
            ids.push((device_id, t));
        }
    }
    // Sorted for `serialiseEndpoints`' reason, and because `pairs` has no order.
    // Only the computers: the addresses under each keep the order they had.
    ids.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = Vec::new();
    for (device_id, t) in ids {
        if matches!(t.get::<mlua::Value>("host"), Ok(mlua::Value::String(_))) {
            out.extend(address(&device_id, &t));
            continue;
        }
        for value in t.sequence_values::<mlua::Value>().flatten() {
            if let mlua::Value::Table(one) = value {
                out.extend(address(&device_id, &one));
            }
        }
    }
    Ok(out)
}

/// Write the whole file back, **byte-for-byte as the plugin's own
/// `serialiseEndpoints` would write it**.
///
/// That is not tidiness. This file has two writers — the device, after a
/// successful discovery or a typed address, and the installer, over the cable —
/// and if they disagreed about spacing then every install would rewrite what
/// the device wrote and every push would rewrite what the installer wrote,
/// which on a FAT32 volume is a write nobody asked for on every connection. The
/// round-trip is asserted against the plugin's own writer under mlua.
fn write_endpoints(dir: &Path, endpoints: &[StoredEndpoint]) -> Result<()> {
    let mut body = String::from(
        "-- Written by the bookmole plugin on this device.\n\
         -- Where each paired computer has been reached, most recent first, and\n\
         -- any address typed in by hand. Safe to delete: discovery rebuilds what\n\
         -- it learned, and a typed address can be typed again.\n\
         return {\n",
    );
    // A stable sort, so the computers come out in id order and each one's
    // addresses stay in the order they were given — which is the order to try.
    let mut sorted: Vec<&StoredEndpoint> = endpoints.iter().collect();
    sorted.sort_by(|a, b| a.device_id.cmp(&b.device_id));
    let mut open: Option<&str> = None;
    for e in sorted {
        if open != Some(e.device_id.as_str()) {
            if open.is_some() {
                body.push_str("    },\n");
            }
            body.push_str(&format!("    [{}] = {{\n", lua_quote(&e.device_id)));
            open = Some(e.device_id.as_str());
        }
        body.push_str(&format!(
            "        {{ host = {}, port = {}, seen_at = {}{} }},\n",
            lua_quote(&e.host),
            e.port,
            e.seen_at,
            if e.typed { ", typed = true" } else { "" }
        ));
    }
    if open.is_some() {
        body.push_str("    },\n");
    }
    body.push_str("}\n");
    std::fs::write(dir.join(ENDPOINT_FILE), body)?;
    Ok(())
}

/// One IPv4 address this machine holds on a network a reader could share.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LanAddress {
    pub address: std::net::Ipv4Addr,
    /// That network's own broadcast address, as the interface reports it —
    /// which is not always `x.y.z.255`, and is the one address a datagram for
    /// "everybody here" is guaranteed to leave by the right interface.
    pub broadcast: Option<std::net::Ipv4Addr>,
}

/// Every address on an interface that is a *network*: up, able to broadcast,
/// and neither loopback nor point-to-point.
///
/// That last pair is the definition doing the work. A VPN tunnel is a
/// point-to-point interface with no broadcast address, and a reader cannot be
/// on the far end of one — so it is not a place this computer can be reached,
/// however much of the routing table it owns.
///
/// `None` when the platform cannot be asked, which is different from an empty
/// list: *no network* means stamp nothing, *cannot tell* means fall back to
/// what the routing table says.
#[cfg(unix)]
fn lan_interfaces() -> Option<Vec<LanAddress>> {
    use nix::net::if_::InterfaceFlags as F;
    let mut out = Vec::new();
    for ifa in nix::ifaddrs::getifaddrs().ok()? {
        if !ifa.flags.contains(F::IFF_UP | F::IFF_BROADCAST)
            || ifa.flags.intersects(F::IFF_LOOPBACK | F::IFF_POINTOPOINT)
        {
            continue;
        }
        let v4 = |a: &Option<nix::sys::socket::SockaddrStorage>| {
            a.as_ref()
                .and_then(|a| a.as_sockaddr_in())
                .map(nix::sys::socket::SockaddrIn::ip)
        };
        if let Some(address) = v4(&ifa.address) {
            out.push(LanAddress {
                address,
                broadcast: v4(&ifa.broadcast),
            });
        }
    }
    Some(out)
}

#[cfg(not(unix))]
fn lan_interfaces() -> Option<Vec<LanAddress>> {
    None
}

/// The address the routing table would use to reach the world.
///
/// **A UDP socket that connects and sends nothing.** `connect` on a datagram
/// socket only picks a route, so this asks the kernel *which of my addresses
/// would be the source* without a packet. The literal is TEST-NET-1 (RFC 5737),
/// which exists to be unroutable — nothing is ever sent to it, and naming a
/// real host here would put someone else's address in our source for no
/// reason.
fn routed_address() -> Option<std::net::Ipv4Addr> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("192.0.2.1:9").ok()?;
    match sock.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(v4) if !v4.is_loopback() && !v4.is_unspecified() => Some(v4),
        // Deliberately IPv4 only: the plugin's ladder derives a `/24` broadcast
        // address from a cached host by matching four dotted decimals, and
        // `socket.udp4()` is what carries every probe.
        _ => None,
    }
}

/// Which of `networks` to tell a reader about, given what the routing table
/// prefers. **Pure**, because the interesting cases are machines this suite
/// does not run on.
///
/// - The routed address, when it is on a network: the ordinary laptop, and the
///   right answer on a machine with wifi *and* ethernet.
/// - Otherwise a network address anyway — private ranges first — because the
///   routed one is then a tunnel's. This is the case that was wrong: a VPN
///   owns the default route, and a reader was told to look for this computer
///   at the far end of it.
/// - Nothing at all when there is no network, rather than the tunnel: a wrong
///   address costs the reader a timed-out rung on every push, and none costs
///   it nothing.
/// - The routed address when the interfaces could not be listed (`None`),
///   which is what this function was before it could tell the difference.
fn choose_lan_address(
    networks: Option<&[LanAddress]>,
    routed: Option<std::net::Ipv4Addr>,
) -> Option<std::net::Ipv4Addr> {
    let Some(networks) = networks else {
        return routed;
    };
    if let Some(routed) = routed
        && networks.iter().any(|n| n.address == routed)
    {
        return Some(routed);
    }
    networks
        .iter()
        .map(|n| n.address)
        // Link-local (169.254/16) is what an interface gives itself when DHCP
        // never answered; it is the last thing worth telling anybody.
        .min_by_key(|a| (!a.is_private(), a.is_link_local(), *a))
}

/// This computer's address on the network a reader would reach it over.
///
/// `None` when there is no network (a laptop with the wifi off, which is an
/// ordinary state to install a plugin in). That means *do not stamp*: a wrong
/// address is worse than none, since the reader would spend its first rung
/// probing it before it ever reached the others.
///
/// **Read by the facade and passed in, never called from `install`.** That is
/// `paired_at`'s rule one field over: ambient state read inside the function is
/// state no test can set, and this one is worse than the clock — a test would
/// stamp whatever address the machine running it happens to have, so it would
/// pass on a laptop, pass differently on a runner, and assert nothing on either.
pub fn this_lan_address() -> Option<String> {
    choose_lan_address(lan_interfaces().as_deref(), routed_address()).map(|a| a.to_string())
}

/// Where a datagram meant for *every reader nearby* should be sent.
///
/// The limited broadcast first, then each network's own. Both, because they
/// fail differently: `255.255.255.255` leaves by the default route — into a
/// VPN tunnel, when there is one, where no reader will ever hear it — while a
/// network's own broadcast address leaves by that network's interface whatever
/// the default route is. Some access points drop one and pass the other, which
/// is the reader's own third rung, seen from this side.
pub fn broadcast_addresses() -> Vec<std::net::Ipv4Addr> {
    let mut out = vec![std::net::Ipv4Addr::BROADCAST];
    for network in lan_interfaces().unwrap_or_default() {
        if let Some(b) = network.broadcast
            && !out.contains(&b)
        {
            out.push(b);
        }
    }
    out
}

/// What to call this computer in the reader's menu.
///
/// The hostname, because it is the name the *router* already knows this machine
/// by — which is the same string rung 4 of the discovery ladder resolves — so
/// one value serves the label and the lookup rather than two that can disagree.
/// `None` when the OS will not say, which is ordinary and not an error: an
/// entry with no name is drawn from its id.
pub fn this_computer() -> Option<String> {
    let name = gethostname::gethostname().to_string_lossy().into_owned();
    let name = name.trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// `n` bytes of system randomness, hex.
///
/// Used for the device id (16) and the pairing token (32). One function for
/// both because the only difference that matters is the length: neither value
/// is guessable and neither carries structure.
///
/// A failure here is the operating system declining to give us randomness,
/// which is not a condition any caller can act on differently — the one place
/// `EngineError::Other` is the honest variant rather than a lazy one.
pub fn mint_id(n: usize) -> Result<String> {
    let mut bytes = vec![0u8; n];
    getrandom::fill(&mut bytes)
        .map_err(|e| crate::error::EngineError::Other(format!("no system randomness: {e}")))?;
    use std::fmt::Write as _;
    Ok(bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    }))
}

// ---- the operations --------------------------------------------------------

/// Read what is on the reader. Never writes.
pub fn inspect(mount: &Path) -> Result<PluginStatus> {
    let dir = check_mount(mount)?;
    let mut status = PluginStatus {
        mount: mount.to_path_buf(),
        plugin_dir: dir.clone(),
        installed: false,
        installed_version: None,
        our_version: PLUGIN_VERSION,
        paired: false,
        device_id: None,
        pairings: Vec::new(),
        modified: Vec::new(),
        unrecognised: Vec::new(),
    };
    if !dir.is_dir() {
        return Ok(status);
    }
    status.installed = dir.join("main.lua").is_file();
    status.installed_version = read_installed_version(&dir)?;
    status.pairings = read_stored_pairings(&dir)?
        .iter()
        .map(StoredPairing::public)
        .collect();
    status.device_id = status.pairings.first().map(|p| p.device_id.clone());

    if let Some((_, manifest)) = read_manifest(&dir)? {
        let mut present: BTreeMap<String, PathBuf> = BTreeMap::new();
        for entry in std::fs::read_dir(&dir)?.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if UNHASHED_FILES.contains(&name.as_str()) || is_os_litter(&name) {
                continue;
            }
            present.insert(name, entry.path());
        }
        for (name, expected) in &manifest {
            // A file we installed and that is *gone* is not an obstruction —
            // reinstalling puts it back. Only a file whose bytes changed is a
            // person's work we must not overwrite.
            if let Some(path) = present.remove(name)
                && sha256_of(&path)? != *expected
            {
                status.modified.push(name.clone());
            }
        }
        status.unrecognised = present.into_keys().collect();
    }
    Ok(status)
}

/// The refusals shared by install and uninstall, checked against a status.
fn refuse_if_obstructed(status: &PluginStatus) -> Result<()> {
    if !status.modified.is_empty() {
        return Err(PluginRefusal::Modified {
            paths: status.modified.clone(),
        }
        .into());
    }
    if !status.unrecognised.is_empty() {
        return Err(PluginRefusal::Unrecognised {
            paths: status.unrecognised.clone(),
        }
        .into());
    }
    Ok(())
}

/// Install (or upgrade) the plugin, and write the pairing the device will use.
///
/// `device_id`, `token` and `paired_at` are the caller's to supply — the engine
/// facade does it, so that the same values reach `paired_devices` and
/// `pairing.lua` and there is exactly one place they are generated.
///
/// `paired_at` is **when the pairing happened**, not when this call ran. An
/// upgrade passes the stored `installed_at` back in; stamping the clock here
/// would leave the device claiming a pairing time the database disagrees with.
///
/// **Our entry is rewritten and every other computer's is kept.** This used to
/// overwrite `pairing.lua` whole, so plugging a reader into a second
/// bookmole install silently killed the first one's pairing while its
/// `paired_devices` row went on claiming it — a failure with no symptom on
/// either machine until something tried to use the token. The entry we rewrite
/// is the one carrying `device_id`; the caller resolved that against its own
/// database, which is the only place the answer exists.
pub fn install(
    mount: &Path,
    device_id: &str,
    token: &str,
    paired_at: i64,
    name: Option<&str>,
    endpoint: Option<&str>,
) -> Result<InstallReport> {
    let status = inspect(mount)?;
    refuse_if_obstructed(&status)?;
    if let Some(installed) = status.installed_version
        && installed > PLUGIN_VERSION
    {
        return Err(PluginRefusal::NewerAlreadyInstalled {
            installed,
            ours: PLUGIN_VERSION,
        }
        .into());
    }

    let dir = &status.plugin_dir;
    std::fs::create_dir_all(dir)?;

    let mut manifest = Manifest::new();
    let mut written = Vec::new();
    for (name, body) in FILES {
        let path = dir.join(name);
        std::fs::write(&path, body)?;
        manifest.insert((*name).to_string(), sha256_of(&path)?);
        written.push((*name).to_string());
    }
    write_manifest(dir, PLUGIN_VERSION, &manifest)?;

    // Read the list back off the device rather than trusting `status`: the
    // public `Pairing` deliberately drops the token, and rewriting the file
    // from it would blank every other computer's secret.
    let mut computers = read_stored_pairings(dir)?;
    let ours = StoredPairing {
        device_id: device_id.to_string(),
        token: token.to_string(),
        name: name.map(str::to_string),
        paired_at,
    };
    match computers.iter().position(|c| c.device_id == device_id) {
        // In place, so the menu's order does not shuffle on every upgrade.
        Some(i) => computers[i] = ours,
        None => computers.push(ours),
    }
    write_pairings(dir, &computers)?;
    written.push(MANIFEST_FILE.to_string());
    written.push(PAIRING_FILE.to_string());

    // **Stamp where this computer can be reached, if we can tell.**
    //
    // Every install re-stamps the endpoint on each USB connection, and that
    // is worth more than it looks, because of what was
    // measured on a Kindle: a broadcast probe's reply is dropped by the
    // device's own firewall and a *unicast* probe's reply is not. A stamped
    // endpoint makes rung 1 unicast, so the first push after a cable works on a
    // stock reader on any network, with no hostname to resolve and no rule to
    // punch. Without it the first push is a broadcast, which is the one rung
    // that needs the firewall's cooperation.
    //
    // It is a *hint and not a fact*: the device overwrites it with what it
    // actually reached, which is the file's original meaning, and a wrong guess
    // costs one timed-out rung. That is why a missing address is not an error —
    // installing with the wifi off is ordinary.
    //
    // **It goes first among our addresses and replaces none of them.** A
    // computer has a list now, and the one a cable just vouched for is the one
    // to try before any the reader learned on some other network — but those
    // stay, and so does anything the user typed in. An address already listed
    // moves to the front as it is, keeping when it was last reached.
    if let Some(host) = endpoint.filter(|h| !h.is_empty()) {
        let mut endpoints = read_endpoints(dir)?;
        let known = endpoints
            .iter()
            .position(|e| e.device_id == device_id && e.host == host)
            .map(|i| endpoints.remove(i));
        let ours = known.unwrap_or_else(|| StoredEndpoint {
            device_id: device_id.to_string(),
            host: host.to_string(),
            port: 0,
            seen_at: paired_at,
            typed: false,
        });
        let first = endpoints
            .iter()
            .position(|e| e.device_id == device_id)
            .unwrap_or(endpoints.len());
        endpoints.insert(first, ours);
        write_endpoints(dir, &endpoints)?;
        written.push(ENDPOINT_FILE.to_string());
    }
    written.sort();
    // Last, once every write has happened: on a Mac each of them left a shadow
    // file behind, and "writes only inside our directory, and only our files"
    // should be true of what a person finds there afterwards.
    sweep_os_litter(dir);

    Ok(InstallReport {
        plugin_dir: dir.clone(),
        device_id: device_id.to_string(),
        version: PLUGIN_VERSION,
        written,
        upgraded_from: status.installed_version.filter(|_| status.installed),
    })
}

/// Remove exactly what we installed, and nothing else.
///
/// Refuses rather than guessing when the directory holds something we did not
/// write, or when a file of ours has been edited. Leaving a plugin in place is
/// recoverable; deleting a person's work is not.
pub fn uninstall(mount: &Path) -> Result<UninstallReport> {
    let status = inspect(mount)?;
    let dir = status.plugin_dir.clone();
    let removed_pairings: Vec<String> = status
        .pairings
        .iter()
        .map(|p| p.device_id.clone())
        .collect();
    if !dir.is_dir() {
        return Ok(UninstallReport {
            plugin_dir: dir,
            removed: Vec::new(),
            forgot_device: status.device_id,
            removed_pairings,
        });
    }
    let Some((_, manifest)) = read_manifest(&dir)? else {
        return Err(PluginRefusal::NoManifest { path: dir }.into());
    };
    refuse_if_obstructed(&status)?;

    let mut removed = Vec::new();
    for name in manifest.keys() {
        let path = dir.join(name);
        if path.is_file() {
            std::fs::remove_file(&path)?;
            removed.push(name.clone());
        }
    }
    // `endpoint.lua` is in this list because the device wrote it and nobody
    // else will: leaving it would break "uninstall is exact" with a file whose
    // whole content is where to find this computer.
    for name in UNHASHED_FILES {
        let path = dir.join(name);
        if path.is_file() {
            std::fs::remove_file(&path)?;
            removed.push(name.to_string());
        }
    }
    // The operating system's shadows of the files just removed go with them —
    // on a Mac, deleting `main.lua` leaves `._main.lua`, and that alone would
    // keep the directory and break "uninstall is exact" on the platform most
    // likely to be holding the cable.
    sweep_os_litter(&dir);
    // `remove_dir` and not `remove_dir_all`: if anything at all is left, the
    // directory stays and so does whatever is in it. The refusals above should
    // have caught that already — this is the belt to their braces, and it is
    // the line that makes "uninstall is exact" true even if they are wrong.
    let _ = std::fs::remove_dir(&dir);
    removed.sort();

    Ok(UninstallReport {
        plugin_dir: dir,
        removed,
        forgot_device: status.device_id,
        removed_pairings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::tests::install as fake_reader;

    fn mount() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        fake_reader(tmp.path());
        tmp
    }

    fn dir_of(mount: &Path) -> PathBuf {
        plugin_dir(mount).unwrap()
    }

    /// Every file under `root` as (relative path, sha256), skipping our own
    /// directory. This is the instrument the strongest test in this module
    /// needs: "write only inside our own plugin directory" is a claim about
    /// every other byte on the volume, and only a whole-tree snapshot can check
    /// it.
    fn snapshot(root: &Path) -> BTreeMap<String, String> {
        fn walk(base: &Path, dir: &Path, skip: &Path, out: &mut BTreeMap<String, String>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path == skip {
                    continue;
                }
                if path.is_dir() {
                    walk(base, &path, skip, out);
                } else {
                    let rel = path.strip_prefix(base).unwrap().display().to_string();
                    out.insert(rel, sha256_of(&path).unwrap());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(root, root, &dir_of(root), &mut out);
        out
    }

    fn install_here(m: &Path) -> InstallReport {
        install(m, "dev-1", "tok-1", 1_700_000_000, Some("desk"), None).unwrap()
    }

    /// An install that also stamps where this computer can be reached.
    fn install_here_from(m: &Path, host: &str) -> InstallReport {
        install(m, "dev-1", "tok-1", 1_700_000_000, Some("desk"), Some(host)).unwrap()
    }

    fn refusal(e: crate::error::EngineError) -> PluginRefusal {
        match e {
            crate::error::EngineError::PluginRefused(r) => r,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn an_install_writes_our_directory_and_touches_nothing_else() {
        let tmp = mount();
        let before = snapshot(tmp.path());

        let report = install_here(tmp.path());

        assert_eq!(
            snapshot(tmp.path()),
            before,
            "an install changed something outside its own directory"
        );
        assert!(dir_of(tmp.path()).join("main.lua").is_file());
        assert_eq!(
            report.written,
            vec!["_meta.lua", "installed.lua", "main.lua", "pairing.lua"],
            "with no address to offer, no endpoint is written"
        );
        assert_eq!(report.upgraded_from, None);
    }

    /// The stamped endpoint, and why an install writes a file it does not own.
    ///
    /// Measured on a Kindle: the reply to a *broadcast* probe is dropped by the
    /// device's firewall and the reply to a *unicast* one is not. A stamped
    /// address is what makes the reader's first rung unicast, so the first push
    /// after a cable needs neither a resolvable hostname nor a firewall rule.
    /// It is a hint the device is free to overwrite with what it really
    /// reached, which is the file's original meaning.
    #[test]
    fn an_install_stamps_where_this_computer_can_be_reached() {
        let tmp = mount();
        let report = install_here_from(tmp.path(), "192.0.2.63");
        let dir = dir_of(tmp.path());

        assert!(report.written.contains(&ENDPOINT_FILE.to_string()));
        assert_eq!(
            read_endpoints(&dir).unwrap(),
            vec![StoredEndpoint {
                device_id: "dev-1".into(),
                host: "192.0.2.63".into(),
                // Not known over a cable, and not needed: every rung probes the
                // rendezvous port and the TCP port arrives in the reply.
                port: 0,
                seen_at: 1_700_000_000,
                typed: false,
            }]
        );

        // It is unhashed and unrecognised-free, or the installer it was written
        // by would refuse the next install *and* the next uninstall.
        let status = inspect(tmp.path()).unwrap();
        assert!(status.unrecognised.is_empty(), "{status:?}");
        assert!(status.modified.is_empty(), "{status:?}");
        assert!(!status.is_obstructed());
    }

    /// The two writers of `endpoint.lua` must produce the same bytes.
    ///
    /// The installer writes it over the cable and the plugin rewrites it after
    /// every successful discovery. If their formatting differed, each would
    /// undo the other's file on every connection — a pointless write to a FAT32
    /// volume every time, and a diff that never settles.
    #[test]
    fn both_writers_of_the_endpoint_cache_agree_byte_for_byte() {
        let tmp = mount();
        install_here_from(tmp.path(), "192.0.2.63");
        let ours = std::fs::read_to_string(dir_of(tmp.path()).join(ENDPOINT_FILE)).unwrap();

        let theirs = run_plugin_lua(
            r#"return RB.serialiseEndpoints({
                   ["dev-1"] = { { host = "192.0.2.63", port = 0, seen_at = 1700000000 } },
               })"#,
        )
        .unwrap();

        // `run_plugin_lua` hands back the string the plugin built; the file has
        // the trailing newline `serialiseEndpoints` ends its last line with.
        assert_eq!(ours.trim_end(), theirs.trim_end());

        // The shape that did not exist before version 3: two computers, one of
        // them known at three addresses, one of those typed in by hand and one
        // a name rather than a number. Given to both writers out of id order,
        // because sorting the computers is each writer's own job — and the
        // addresses under one computer must come out in the order they went in.
        let dir = tempfile::tempdir().unwrap();
        let e = |id: &str, host: &str, port: u16, seen_at: i64, typed: bool| StoredEndpoint {
            device_id: id.into(),
            host: host.into(),
            port,
            seen_at,
            typed,
        };
        write_endpoints(
            dir.path(),
            &[
                e("bbb", "203.0.113.9", 51001, 20, false),
                e("aaa", "desk.lan", 0, 0, true),
                e("aaa", "192.0.2.20", 51000, 10, false),
                e("aaa", "203.0.113.42", 0, 5, true),
            ],
        )
        .unwrap();
        let ours = std::fs::read_to_string(dir.path().join(ENDPOINT_FILE)).unwrap();
        let theirs = run_plugin_lua(
            r#"return RB.serialiseEndpoints({
                   bbb = { { host = "203.0.113.9", port = 51001, seen_at = 20 } },
                   aaa = {
                       { host = "desk.lan", port = 0, seen_at = 0, typed = true },
                       { host = "192.0.2.20", port = 51000, seen_at = 10 },
                       { host = "203.0.113.42", port = 0, seen_at = 5, typed = true },
                   },
               })"#,
        )
        .unwrap();
        assert_eq!(ours, theirs);

        // And each reads what the other wrote: the plugin's parser on our
        // bytes, ours on the same file.
        let back = read_endpoints(dir.path()).unwrap();
        assert_eq!(
            back.iter()
                .map(|e| format!(
                    "{}@{}{}",
                    e.device_id,
                    e.host,
                    if e.typed { "*" } else { "" }
                ))
                .collect::<Vec<_>>(),
            [
                "aaa@desk.lan*",
                "aaa@192.0.2.20",
                "aaa@203.0.113.42*",
                "bbb@203.0.113.9"
            ]
        );
        let read_by_plugin = run_plugin_lua(&format!(
            "local got = RB.parseEndpoints(load({ours:?})())
             local out = {{}}
             for _, a in ipairs(got.aaa) do
                 out[#out + 1] = a.host .. (a.typed and '*' or '')
             end
             return table.concat(out, ' ') .. ' | ' .. got.bbb[1].host"
        ))
        .unwrap();
        assert_eq!(
            read_by_plugin,
            "desk.lan* 192.0.2.20 203.0.113.42* | 203.0.113.9"
        );
    }

    /// An install's address goes first and costs the reader nothing it knew.
    ///
    /// While a computer had one address the stamp simply replaced it. With a
    /// list, replacing would throw away the office network every time the
    /// reader was plugged in at home — and, worse, an address the user typed
    /// in by hand because nothing else worked.
    #[test]
    fn an_install_puts_its_address_first_and_keeps_the_others() {
        let tmp = mount();
        install_here(tmp.path());
        let dir = dir_of(tmp.path());
        std::fs::write(
            dir.join(ENDPOINT_FILE),
            r#"return {
                ["dev-1"] = {
                    { host = "198.51.100.7", port = 51000, seen_at = 30 },
                    { host = "desk.lan", port = 0, seen_at = 0, typed = true },
                    { host = "192.0.2.63", port = 51003, seen_at = 20 },
                },
            }"#,
        )
        .unwrap();

        // An address the reader already had moves up as it is — port, time and
        // all — rather than being re-stamped with what a cable cannot know.
        install_here_from(tmp.path(), "192.0.2.63");
        let hosts = |dir: &Path| {
            read_endpoints(dir)
                .unwrap()
                .into_iter()
                .map(|e| (e.host, e.port, e.typed))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            hosts(&dir),
            [
                ("192.0.2.63".to_string(), 51003, false),
                ("198.51.100.7".to_string(), 51000, false),
                ("desk.lan".to_string(), 0, true),
            ]
        );

        // A new one is added in front, and nothing is dropped to make room.
        install_here_from(tmp.path(), "203.0.113.4");
        assert_eq!(
            hosts(&dir).iter().map(|h| h.0.as_str()).collect::<Vec<_>>(),
            ["203.0.113.4", "192.0.2.63", "198.51.100.7", "desk.lan"]
        );
    }

    #[test]
    fn install_then_uninstall_leaves_the_reader_as_it_found_it() {
        let tmp = mount();
        let before = snapshot(tmp.path());

        install_here(tmp.path());
        let report = uninstall(tmp.path()).unwrap();

        assert_eq!(snapshot(tmp.path()), before);
        assert!(
            !dir_of(tmp.path()).exists(),
            "the directory itself is part of what we put there"
        );
        assert_eq!(report.forgot_device.as_deref(), Some("dev-1"));
    }

    #[test]
    fn an_ordinary_directory_is_refused() {
        let plain = tempfile::tempdir().unwrap();
        let err = refusal(install(plain.path(), "d", "t", 0, None, None).unwrap_err());
        assert!(matches!(err, PluginRefusal::NotAKoreaderMount { .. }));
        assert_eq!(
            std::fs::read_dir(plain.path()).unwrap().count(),
            0,
            "a refusal must not create anything"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_to_a_reader_is_refused() {
        let tmp = mount();
        let link_home = tempfile::tempdir().unwrap();
        let link = link_home.path().join("KOBOeReader");
        std::os::unix::fs::symlink(tmp.path(), &link).unwrap();

        // The target really is a KOReader install: the refusal is about the
        // path we were handed, not about what is at the end of it.
        assert!(is_koreader_mount(&link));
        let err = refusal(install(&link, "d", "t", 0, None, None).unwrap_err());
        assert!(matches!(err, PluginRefusal::MountIsASymlink { .. }));
        assert!(!dir_of(tmp.path()).exists());
    }

    #[test]
    fn a_newer_plugin_is_not_overwritten() {
        let tmp = mount();
        install_here(tmp.path());
        let dir = dir_of(tmp.path());

        // A bookmole from the future got here first. Rewrite `_meta.lua`
        // *and* its manifest hash, so this is a clean newer install rather than
        // an edited file — the two refusals must not be confusable.
        let newer = format!("return {{ version = {} }}\n", PLUGIN_VERSION + 1);
        std::fs::write(dir.join("_meta.lua"), &newer).unwrap();
        let mut manifest = read_manifest(&dir).unwrap().unwrap().1;
        manifest.insert(
            "_meta.lua".into(),
            sha256_of(&dir.join("_meta.lua")).unwrap(),
        );
        write_manifest(&dir, PLUGIN_VERSION + 1, &manifest).unwrap();

        let err = refusal(install(tmp.path(), "d", "t", 0, None, None).unwrap_err());
        assert_eq!(
            err,
            PluginRefusal::NewerAlreadyInstalled {
                installed: PLUGIN_VERSION + 1,
                ours: PLUGIN_VERSION,
            }
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("_meta.lua")).unwrap(),
            newer,
            "the refusal wrote anyway"
        );
    }

    #[test]
    fn a_file_the_user_edited_is_never_overwritten_and_never_deleted() {
        let tmp = mount();
        install_here(tmp.path());
        let dir = dir_of(tmp.path());
        let theirs = "-- I fixed a bug in your plugin\n";
        std::fs::write(dir.join("main.lua"), theirs).unwrap();

        let status = inspect(tmp.path()).unwrap();
        assert_eq!(status.modified, vec!["main.lua"]);
        assert!(status.is_obstructed());

        for err in [
            install(tmp.path(), "d", "t", 0, None, None).unwrap_err(),
            uninstall(tmp.path()).unwrap_err(),
        ] {
            assert!(matches!(refusal(err), PluginRefusal::Modified { .. }));
        }
        assert_eq!(
            std::fs::read_to_string(dir.join("main.lua")).unwrap(),
            theirs
        );
    }

    #[test]
    fn something_we_did_not_write_blocks_both_operations() {
        let tmp = mount();
        install_here(tmp.path());
        let dir = dir_of(tmp.path());
        std::fs::write(dir.join("notes.txt"), "mine\n").unwrap();

        let status = inspect(tmp.path()).unwrap();
        assert_eq!(status.unrecognised, vec!["notes.txt"]);

        let err = refusal(uninstall(tmp.path()).unwrap_err());
        assert!(matches!(err, PluginRefusal::Unrecognised { .. }));
        assert!(dir.join("notes.txt").is_file());
        assert!(dir.join("main.lua").is_file(), "nothing was removed");
    }

    /// macOS leaves a shadow beside every file it writes to a FAT volume, and
    /// those are not somebody's work.
    ///
    /// The first install from a Mac onto a real Kindle left five `._*` files in
    /// our directory and every one was reported as *not ours* — so the
    /// installer had obstructed itself, and the next install and the next
    /// uninstall would both have refused on a reader nobody had touched.
    #[test]
    fn what_macos_leaves_beside_our_files_obstructs_nothing() {
        let tmp = mount();
        install_here_from(tmp.path(), "192.0.2.63");
        let dir = dir_of(tmp.path());
        let litter = [
            "._main.lua",
            ".__meta.lua",
            "._installed.lua",
            "._pairing.lua",
            "._endpoint.lua",
            ".DS_Store",
        ];
        let drop_litter = || {
            for name in litter {
                std::fs::write(dir.join(name), b"\0\x05\x16\x07 apple double").unwrap();
            }
        };

        drop_litter();
        let status = inspect(tmp.path()).unwrap();
        assert!(status.unrecognised.is_empty(), "{status:?}");
        assert!(!status.is_obstructed());

        // An install clears it away rather than leaving it for a person to
        // wonder about — including whatever this very install would have left.
        install_here_from(tmp.path(), "192.0.2.63");
        for name in litter {
            assert!(!dir.join(name).exists(), "{name} survived an install");
        }

        // And an uninstall still leaves the reader exactly as it was found.
        drop_litter();
        uninstall(tmp.path()).unwrap();
        assert!(
            !dir.exists(),
            "the shadows must not keep the directory alive"
        );
    }

    /// The exemption is for the shadow of a file **of ours** and nothing wider.
    #[test]
    fn a_shadow_of_somebody_elses_file_is_still_somebody_elses() {
        let tmp = mount();
        install_here(tmp.path());
        let dir = dir_of(tmp.path());
        for name in [
            "._notes.txt",
            "._",
            "._main.lua.bak",
            ".hidden",
            "main.lua~",
        ] {
            std::fs::write(dir.join(name), b"x").unwrap();
            let status = inspect(tmp.path()).unwrap();
            assert_eq!(status.unrecognised, vec![name.to_string()], "{name}");
            assert!(matches!(
                refusal(
                    install(tmp.path(), "dev-1", "tok-1", 1_700_000_000, None, None).unwrap_err()
                ),
                PluginRefusal::Unrecognised { .. }
            ));
            assert!(matches!(
                refusal(uninstall(tmp.path()).unwrap_err()),
                PluginRefusal::Unrecognised { .. }
            ));
            assert!(dir.join(name).exists(), "{name} must never be deleted");
            std::fs::remove_file(dir.join(name)).unwrap();
        }
    }

    #[test]
    fn a_plugin_with_no_manifest_is_not_ours_to_remove() {
        let tmp = mount();
        let dir = dir_of(tmp.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.lua"), "-- somebody else's\n").unwrap();

        let err = refusal(uninstall(tmp.path()).unwrap_err());
        assert!(matches!(err, PluginRefusal::NoManifest { .. }));
        assert!(dir.join("main.lua").is_file());
    }

    #[test]
    fn uninstalling_a_reader_that_never_had_it_is_not_an_error() {
        let tmp = mount();
        let report = uninstall(tmp.path()).unwrap();
        assert!(report.removed.is_empty());
        assert_eq!(report.forgot_device, None);
    }

    #[test]
    fn an_upgrade_reports_what_it_replaced() {
        let tmp = mount();
        install_here(tmp.path());
        let again = install(
            tmp.path(),
            "dev-1",
            "tok-1",
            1_700_000_001,
            Some("desk"),
            None,
        )
        .unwrap();
        assert_eq!(again.upgraded_from, Some(PLUGIN_VERSION));
        assert_eq!(
            inspect(tmp.path()).unwrap().device_id.as_deref(),
            Some("dev-1")
        );
    }

    /// The reason `_meta.lua` may never grow a `require("gettext")` — which
    /// every KOReader plugin's does, and which a later contributor will add by
    /// reflex. Read the shipped bytes through the same VM the installer uses.
    #[test]
    fn the_shipped_meta_is_readable_by_the_sidecar_sandbox() {
        let lua = sandboxed_lua().unwrap();
        let meta = FILES.iter().find(|(n, _)| *n == "_meta.lua").unwrap().1;
        let t = eval_table(&lua, meta, "_meta.lua").unwrap();
        assert_eq!(get_int(&t, "version"), Some(PLUGIN_VERSION));
        // Comment lines are skipped: the header of that file *explains* why
        // there is no `require` in it, so a bare `contains` fails on the
        // explanation and passes on nothing.
        let code = meta
            .lines()
            .filter(|l| !l.trim_start().starts_with("--"))
            .collect::<String>();
        assert!(
            !code.contains("require("),
            "a `require` in _meta.lua makes the installed version unreadable"
        );
    }

    /// `pairing.lua` is written by us and read by on-device Lua, so it has to
    /// be a table literal in both directions.
    #[test]
    fn the_pairing_we_write_reads_back() {
        let tmp = mount();
        install(
            tmp.path(),
            "abc123",
            "s3cret",
            1_700_000_000,
            Some("desk"),
            None,
        )
        .unwrap();
        let dir = dir_of(tmp.path());
        let src = std::fs::read_to_string(dir.join(PAIRING_FILE)).unwrap();

        let read = read_stored_pairings(&dir).unwrap();
        assert_eq!(
            read,
            vec![StoredPairing {
                device_id: "abc123".into(),
                token: "s3cret".into(),
                name: Some("desk".into()),
                paired_at: 1_700_000_000,
            }]
        );
        assert!(
            !src.contains("host") && !src.contains("port"),
            "no endpoint is written until there is a listener to name"
        );
    }

    /// The bug this stage exists to fix, end to end and on one volume.
    ///
    /// Two bookmole installs, one reader. The second used to read the
    /// file's single `device_id`, find no row for it, mint a fresh identity and
    /// **overwrite** — after which the first machine held a `paired_devices`
    /// row for a token the device no longer had, with nothing on either side
    /// looking wrong until something tried to use it.
    #[test]
    fn a_second_computer_does_not_steal_the_reader_from_the_first() {
        let tmp = mount();
        install(
            tmp.path(),
            "dev-a",
            "tok-a",
            1_700_000_000,
            Some("laptop"),
            None,
        )
        .unwrap();
        install(
            tmp.path(),
            "dev-b",
            "tok-b",
            1_700_000_100,
            Some("desktop"),
            None,
        )
        .unwrap();

        let dir = dir_of(tmp.path());
        let stored = read_stored_pairings(&dir).unwrap();
        assert_eq!(
            stored
                .iter()
                .map(|c| c.device_id.as_str())
                .collect::<Vec<_>>(),
            vec!["dev-a", "dev-b"],
        );
        assert_eq!(
            stored[0].token, "tok-a",
            "the first computer's secret survived the second computer's install"
        );
        assert_eq!(stored[0].name.as_deref(), Some("laptop"));
        assert_eq!(stored[0].paired_at, 1_700_000_000);

        // And re-installing the first rewrites *its* entry in place, so the
        // menu's order does not shuffle under the user on every upgrade.
        install(
            tmp.path(),
            "dev-a",
            "tok-a",
            1_700_000_000,
            Some("laptop2"),
            None,
        )
        .unwrap();
        let again = read_stored_pairings(&dir).unwrap();
        assert_eq!(
            again
                .iter()
                .map(|c| c.device_id.as_str())
                .collect::<Vec<_>>(),
            vec!["dev-a", "dev-b"],
        );
        assert_eq!(again[0].name.as_deref(), Some("laptop2"));
        assert_eq!(again[1].token, "tok-b");
    }

    /// The version-1 flat file is still on every reader paired before the list
    /// existed, so it is read as the one-entry list it always meant — and an
    /// install onto one *upgrades the shape* rather than adding a second entry
    /// for the computer that is already there.
    #[test]
    fn the_version_one_flat_file_is_read_as_one_computer() {
        let tmp = mount();
        install_here(tmp.path());
        let dir = dir_of(tmp.path());
        std::fs::write(
            dir.join(PAIRING_FILE),
            "-- Written by bookmole over USB. Do not edit.\n\
             return {\n\
             \x20   device_id = \"old-id\",\n\
             \x20   token     = \"old-token\",\n\
             \x20   paired_at = 1690000000,\n\
             }\n",
        )
        .unwrap();

        let status = inspect(tmp.path()).unwrap();
        assert_eq!(status.device_id.as_deref(), Some("old-id"));
        assert_eq!(status.pairings.len(), 1);
        assert_eq!(status.pairings[0].name, None, "version 1 wrote no name");
        assert_eq!(status.pairings[0].paired_at, Some(1_690_000_000));

        install(
            tmp.path(),
            "old-id",
            "old-token",
            1_690_000_000,
            Some("desk"),
            None,
        )
        .unwrap();
        let stored = read_stored_pairings(&dir).unwrap();
        assert_eq!(stored.len(), 1, "the same computer, in the list shape");
        assert_eq!(stored[0].token, "old-token");
        assert_eq!(stored[0].paired_at, 1_690_000_000);
    }

    /// Half a credential proves nothing, so it is dropped rather than kept —
    /// otherwise an installer can decide it has found its own entry and write a
    /// token beside a name that is not its own.
    #[test]
    fn an_entry_missing_its_id_or_its_token_is_not_a_computer() {
        let tmp = mount();
        install_here(tmp.path());
        let dir = dir_of(tmp.path());
        std::fs::write(
            dir.join(PAIRING_FILE),
            "return { computers = {\n\
             \x20 { device_id = \"a\", token = \"t\" },\n\
             \x20 { device_id = \"b\" },\n\
             \x20 { token = \"t\" },\n\
             \x20 { device_id = \"\", token = \"t\" },\n\
             \x20 \"not a table\",\n\
             } }\n",
        )
        .unwrap();
        let stored = read_stored_pairings(&dir).unwrap();
        assert_eq!(
            stored
                .iter()
                .map(|c| c.device_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a"],
        );
    }

    /// A hostname is not our string. Until this stage every value written into
    /// `pairing.lua` was hex, so raw interpolation was safe by accident; a name
    /// carrying a quote would write a file that does not parse, which the
    /// reader reports as *not paired* and the installer reads back as a fresh
    /// device.
    #[test]
    fn a_name_that_could_break_the_file_is_escaped() {
        let tmp = mount();
        let hostile = "o\"liver's\\box\nnewline";
        install(
            tmp.path(),
            "dev-1",
            "tok-1",
            1_700_000_000,
            Some(hostile),
            None,
        )
        .unwrap();
        let stored = read_stored_pairings(&dir_of(tmp.path())).unwrap();
        assert_eq!(stored[0].name.as_deref(), Some(hostile));
    }

    /// The collision the endpoint file caused: a cache file beside the plugin used
    /// to land in `unrecognised`, which `refuse_if_obstructed` turns into a
    /// refusal of **install and uninstall both** — a plugin bricking its own
    /// installer the first time it wrote down where to find us.
    #[test]
    fn the_endpoint_the_device_writes_blocks_neither_operation() {
        let tmp = mount();
        install_here(tmp.path());
        let dir = dir_of(tmp.path());
        let learned = "return { [\"dev-1\"] = { host = \"192.0.2.20\", port = 51861 } }\n";
        std::fs::write(dir.join(ENDPOINT_FILE), learned).unwrap();

        let status = inspect(tmp.path()).unwrap();
        assert!(status.unrecognised.is_empty(), "{status:?}");
        assert!(!status.is_obstructed());

        // An install with no address to offer leaves it exactly alone: it is
        // the device's learned state, and an upgrade is not a reason to forget
        // where we live.
        install_here(tmp.path());
        assert_eq!(
            std::fs::read_to_string(dir.join(ENDPOINT_FILE)).unwrap(),
            learned
        );

        // An install *with* one rewrites our own entry and leaves every other
        // computer's untouched — `pairing.lua`'s rule, for the same reason: two
        // computers share this file and only one of them is holding the cable.
        install(
            tmp.path(),
            "dev-2",
            "tok-2",
            1_700_000_500,
            Some("laptop"),
            Some("192.0.2.63"),
        )
        .unwrap();
        let after = read_endpoints(&dir).unwrap();
        assert_eq!(
            after,
            vec![
                StoredEndpoint {
                    device_id: "dev-1".into(),
                    host: "192.0.2.20".into(),
                    port: 51861,
                    seen_at: 0,
                    typed: false,
                },
                StoredEndpoint {
                    device_id: "dev-2".into(),
                    host: "192.0.2.63".into(),
                    port: 0,
                    seen_at: 1_700_000_500,
                    typed: false,
                },
            ]
        );

        // An uninstall takes it, because "uninstall is exact" cannot leave
        // behind a file whose whole content is where to find this computer.
        let report = uninstall(tmp.path()).unwrap();
        assert!(report.removed.contains(&ENDPOINT_FILE.to_string()));
        assert!(!dir.exists());
    }

    /// The pure function the on-device menu runs, executed against exactly the
    /// bytes the installer writes.
    ///
    /// `main.lua` is 90 lines that no test has ever run: the only Lua a test
    /// parsed was `_meta.lua`, and that test protects the version-reading trick
    /// rather than the plugin's behaviour. Defensible for a menu entry, and not
    /// defensible for the selection logic a push verb will branch on. `require`
    /// is stubbed with a table that answers every index and every call with
    /// itself, which is enough to get the module's return value back without
    /// KOReader present.
    fn run_plugin_lua(script: &str) -> mlua::Result<String> {
        let lua = mlua::Lua::new();
        let stub = lua.create_table()?;
        let meta = lua.create_table()?;
        let s = stub.clone();
        meta.set(
            "__index",
            lua.create_function(move |_, (_t, _k): (mlua::Table, mlua::Value)| Ok(s.clone()))?,
        )?;
        let s = stub.clone();
        meta.set(
            "__call",
            lua.create_function(move |_, _: mlua::MultiValue| Ok(s.clone()))?,
        )?;
        stub.set_metatable(Some(meta));

        // Two modules answer for real, and the reason is the bug this harness
        // did not catch for a release: `gettext` and `ffi/util.template` are
        // what every user-visible string in the plugin passes through, so a
        // stub that swallows them turns a menu label into an opaque table and
        // makes "the menu built" unassertable. Answering them properly is what
        // lets a test read the labels a person would see.
        let gettext = lua.create_function(|_, s: String| Ok(s))?;
        let ffi_util = lua.create_table()?;
        ffi_util.set(
            "template",
            lua.create_function(|_, (fmt, args): (String, mlua::Variadic<String>)| {
                let mut out = fmt;
                for (i, a) in args.iter().enumerate() {
                    out = out.replace(&format!("%{}", i + 1), a);
                }
                Ok(out)
            })?,
        )?;

        let s = stub.clone();
        lua.globals().set(
            "require",
            lua.create_function(move |lua, name: String| {
                // A test may stand a module in by name. The device half of a
                // wifi pairing is glue around a socket and a JSON codec, and
                // the only way to run that glue here is to hand it fakes of
                // both — everything else still falls through to the stub.
                if let Ok(mlua::Value::Table(fakes)) =
                    lua.globals().get::<mlua::Value>("__rb_modules")
                    && let Ok(fake) = fakes.get::<mlua::Value>(name.as_str())
                    && !fake.is_nil()
                {
                    return Ok(fake);
                }
                match name.as_str() {
                    "gettext" => lua.globals().get::<mlua::Value>("__rb_gettext"),
                    "ffi/util" => lua.globals().get::<mlua::Value>("__rb_ffi_util"),
                    "ui/widget/container/widgetcontainer" => {
                        lua.globals().get::<mlua::Value>("__rb_widget")
                    }
                    "ffi/sha2" => lua.globals().get::<mlua::Value>("__rb_sha2"),
                    _ => Ok(mlua::Value::Table(s.clone())),
                }
            })?,
        )?;
        lua.globals().set("__rb_gettext", gettext)?;
        lua.globals().set("__rb_ffi_util", ffi_util)?;

        // A fourth, since version 3: `ffi/sha2`, answered by **the desktop's
        // own HMAC**. The pairing exchange is two implementations of one
        // protocol, and the only honest test of that is the reader's half
        // signing with the same function the computer verifies with. That the
        // device's library computes this same HMAC is pinned separately, from
        // the device's own output, in `wireless.rs`.
        let sha2 = lua.create_table()?;
        sha2.set("sha256", lua.create_function(|_, s: String| Ok(s))?)?;
        sha2.set(
            "hmac",
            lua.create_function(|_, (_hash, key, msg): (mlua::Value, String, String)| {
                Ok(crate::wireless::mac(&key, &msg))
            })?,
        )?;
        lua.globals().set("__rb_sha2", sha2)?;
        lua.globals().set("__rb_modules", lua.create_table()?)?;

        // A third module answers for real, and for a subtler reason. The plugin
        // is `WidgetContainer:extend{...}`, so under the swallow-everything stub
        // the *module itself* is that stub — and a stub whose `__index` answers
        // every key means `self.window_server` is never nil, so `windowOpen()`
        // is always true and the menu under test is not the menu on the device.
        // Six lines of real `extend`/`new` make the plugin a plain table again,
        // where an unset field reads as nil the way it does on a reader.
        let widget: mlua::Value = lua
            .load(
                r#"local W = {}
                   function W:extend(t)
                       t = t or {}
                       setmetatable(t, { __index = self })
                       return t
                   end
                   function W:new(o)
                       o = o or {}
                       setmetatable(o, { __index = self })
                       if o.init then o:init() end
                       return o
                   end
                   return W"#,
            )
            .eval()?;
        lua.globals().set("__rb_widget", widget)?;

        let main = FILES.iter().find(|(n, _)| *n == "main.lua").unwrap().1;
        let module: mlua::Table = lua.load(main).set_name("main.lua").eval()?;
        lua.globals().set("RB", module)?;
        // Stringified *inside* the VM's lifetime: an `mlua::Value` handed back
        // outlives the `Lua` it points into and panics with "Lua instance is
        // destroyed" on the first read of it.
        lua.load(script).eval::<mlua::Value>().map(|v| match v {
            mlua::Value::String(s) => s.to_string_lossy().to_string(),
            mlua::Value::Integer(i) => i.to_string(),
            other => format!("{other:?}"),
        })
    }

    #[test]
    fn every_shipped_lua_file_compiles() {
        // The floor of the Lua gate: a syntax error in a file we `include_str!`
        // is a compile-time constant that bricks a reader at runtime, and
        // nothing else in this repo would have noticed.
        let lua = mlua::Lua::new();
        for (name, body) in FILES {
            lua.load(*body)
                .set_name(*name)
                .into_function()
                .unwrap_or_else(|e| panic!("{name} does not compile: {e}"));
        }
    }

    /// The menu a paired reader actually draws, built from a real `pairing.lua`.
    ///
    /// **"Every shipped file compiles" is not "the entry appears", and the gap
    /// between them is one release wide.** One release shipped a `main.lua` whose menu
    /// loop was `for _, entry in ipairs(self.pairings)` — `_` is gettext, bound
    /// at the top of the module, and the next line calls it. Every gate this
    /// repo had was green: it compiles under mlua, it compiles under LuaJIT,
    /// `normalisePairings` and `ladder` both pass. On the device the menu
    /// vanished from Tools entirely, because the throw happens while KOReader
    /// is *building* the menu and a paired reader is the only case that runs
    /// the loop at all. So the gate has to instantiate the plugin and ask it
    /// for its menu, which is what this does.
    #[test]
    fn the_menu_builds_on_a_paired_reader() {
        let tmp = mount();
        install(
            tmp.path(),
            "dev-a",
            "tok-a",
            1_700_000_000,
            Some("laptop"),
            None,
        )
        .unwrap();
        let dir = dir_of(tmp.path());

        // `self.path` and `self.ui` are what `pluginloader.lua` supplies. Then
        // the two calls KOReader makes, in its order: `init` (which reads the
        // pairing file off the "device") and `addToMainMenu`.
        let labels = run_plugin_lua(&format!(
            r#"RB.path = {path:?}
               RB.ui = require("anything")
               RB:init()
               local items = {{}}
               RB:addToMainMenu(items)
               local entry = items.bookmole
               local out = {{ entry.text .. "/" .. entry.sorting_hint }}
               for _, it in ipairs(entry.sub_item_table_func()) do
                   out[#out + 1] = it.text or it.text_func()
                   if it.sub_item_table_func then
                       for _, computer in ipairs(it.sub_item_table_func()) do
                           out[#out + 1] = "> " .. computer.text
                           for _, line in ipairs(computer.sub_item_table_func()) do
                               out[#out + 1] = ">> " .. line.text
                           end
                       end
                   end
               end
               return table.concat(out, " | ")"#,
            path = dir.to_string_lossy()
        ))
        .unwrap();

        assert_eq!(
            labels,
            concat!(
                "bookmole/tools | Send highlights to laptop",
                " | Let a computer fetch highlights",
                // One computer, opened: no address is known yet, so the only
                // lines are the two things that can be done about it.
                " | Linked computers | > laptop | >> Add an address | >> Unlink laptop",
                " | Link a computer over wifi | How sending and fetching work"
            ),
            "the menu a paired reader draws"
        );
    }

    /// The same menu on a reader that is installed but not paired.
    ///
    /// The other side of the loop above: with no computers there is nothing to
    /// push to and no window worth opening, so the entry is present and carries
    /// the one item that can still say something useful.
    #[test]
    fn the_menu_builds_on_an_unpaired_reader() {
        let tmp = mount();
        let dir = dir_of(tmp.path());
        std::fs::create_dir_all(&dir).unwrap();
        let labels = run_plugin_lua(&format!(
            r#"RB.path = {path:?}
               RB.ui = require("anything")
               RB:init()
               local items = {{}}
               RB:addToMainMenu(items)
               local out = {{}}
               for _, it in ipairs(items.bookmole.sub_item_table_func()) do
                   out[#out + 1] = it.text or it.text_func()
               end
               return table.concat(out, " | ")"#,
            path = dir.to_string_lossy()
        ))
        .unwrap();
        // Since version 3 this is not a dead end: a reader with the plugin and
        // no computer can be given one from here, without a cable.
        assert_eq!(
            labels,
            "Link a computer over wifi | How sending and fetching work"
        );
    }

    /// The exact firewall rules the plugin runs on a Kindle.
    ///
    /// A Kindle's INPUT policy is DROP with a conntrack accept per interface,
    /// which passes the answer to a *unicast* probe and drops the answer to a
    /// *broadcast* one — the ladder's fallback rung, and the one a reader that
    /// has never pushed depends on. Measured on the device: same second, same
    /// desktop, unicast answered and broadcast timed out.
    ///
    /// None of that runs in CI, so what is asserted is the string. It is worth
    /// asserting because it is a *command*: a nil port would interpolate as
    /// `--dport nil` on somebody's reader, and an add and a delete that differ
    /// by one character leave a rule behind for ever.
    #[test]
    fn the_firewall_rules_are_the_ones_koreaders_own_plugins_use() {
        let rules = run_plugin_lua(
            r#"return table.concat({
                   RB.firewallRule("A", "INPUT", "udp", 61862),
                   RB.firewallRule("D", "OUTPUT", "tcp", 8080),
                   tostring(RB.firewallRule("A", "INPUT", "udp", nil)),
                   tostring(RB.firewallRule("X", "INPUT", "udp", 1)),
                   tostring(RB.firewallRule("A", "FORWARD", "udp", 1)),
                   tostring(RB.firewallRule("A", "INPUT", "icmp", 1)),
                   tostring(RB.firewallRule("A", "INPUT", "udp", 70000)),
               }, "\n")"#,
        )
        .unwrap();

        assert_eq!(
            rules.lines().collect::<Vec<_>>(),
            vec![
                // Byte-for-byte `SSH.koplugin` and `httpinspector.koplugin`.
                "iptables -A INPUT -p udp --dport 61862 -m conntrack --ctstate NEW,ESTABLISHED -j ACCEPT",
                "iptables -D OUTPUT -p tcp --sport 8080 -m conntrack --ctstate ESTABLISHED -j ACCEPT",
                // Everything a caller could get wrong builds no command at all.
                "nil",
                "nil",
                "nil",
                "nil",
                "nil",
            ]
        );
    }

    #[test]
    fn the_plugin_reads_every_pairing_shape_the_installer_can_write() {
        let tmp = mount();
        install(
            tmp.path(),
            "dev-a",
            "tok-a",
            1_700_000_000,
            Some("laptop"),
            None,
        )
        .unwrap();
        install(tmp.path(), "dev-b", "tok-b", 1_700_000_100, None, None).unwrap();
        let list = std::fs::read_to_string(dir_of(tmp.path()).join(PAIRING_FILE)).unwrap();

        // The list shape, straight off the device: both computers, in order,
        // and the nameless one drawn from its id exactly as the menu draws it.
        let names = run_plugin_lua(&format!(
            "local raw = load({list:?})()
             local out = {{}}
             for _, e in ipairs(RB.normalisePairings(raw)) do
                 table.insert(out, RB.computerName(e))
             end
             return table.concat(out, ',')"
        ))
        .unwrap();
        assert_eq!(names, "laptop,dev-b");

        // The version-1 flat shape is one computer, and a table that names nobody
        // is zero — never a nil the caller has to test for.
        for (src, want) in [
            (r#"return { device_id = "x", token = "t" }"#, 1),
            (r#"return { device_id = "x" }"#, 0),
            (r#"return { computers = {} }"#, 0),
            (r#"return { }"#, 0),
            (r#"return "nonsense""#, 0),
        ] {
            let n =
                run_plugin_lua(&format!("return #RB.normalisePairings(load({src:?})())")).unwrap();
            assert_eq!(n, want.to_string(), "{src}");
        }
    }

    /// The discovery ladder's **ordering and fallback**, run as the device runs
    /// it. This is the part with bugs in it, and a ladder that can only be
    /// exercised by carrying a laptop between subnets is a ladder with no
    /// tests — `watch.rs`'s sentence about `notify`, one subsystem over.
    #[test]
    fn the_ladder_tries_the_cheap_rungs_first_and_degrades() {
        let rungs = |cached: &str, name: &str| {
            run_plugin_lua(&format!(
                "local out = {{}}
                 for _, r in ipairs(RB.ladder({cached}, {name})) do
                     table.insert(out, r.kind .. ':' .. r.host)
                 end
                 return table.concat(out, ' ')"
            ))
            .unwrap()
        };

        // Everything known: the cached address, then the two broadcasts — the
        // global one and the cached /24's, for the APs that filter one and pass
        // the other — then the name, bare and with each suffix.
        assert_eq!(
            rungs(r#"{ { host = "192.0.2.20", port = 51000 } }"#, r#""desk""#),
            "cached:192.0.2.20 broadcast:255.255.255.255 broadcast:192.0.2.255 \
             dns:desk dns:desk.lan dns:desk.home.arpa"
        );

        // A first run has no cache, so the ladder starts at the broadcast — and
        // there is no /24 to derive a directed broadcast from.
        assert_eq!(
            rungs("nil", r#""desk""#),
            "broadcast:255.255.255.255 dns:desk dns:desk.lan dns:desk.home.arpa"
        );

        // An entry written before there were names (the version-1 flat file) still
        // gets the rungs that do not need one, rather than no ladder at all.
        assert_eq!(
            rungs(r#"{ { host = "203.0.113.5" } }"#, "nil"),
            "cached:203.0.113.5 broadcast:255.255.255.255 broadcast:203.0.113.255"
        );

        // An address that is a *name* has no subnet to broadcast into, and
        // `sendto` cannot take one — so it is a `dns` rung, which is the kind
        // `discover` resolves first. It used to be filed as `cached` and sent
        // as it stood, which is a probe that never leaves the device.
        assert_eq!(
            rungs(r#"{ { host = "desk.lan" } }"#, "nil"),
            "dns:desk.lan broadcast:255.255.255.255"
        );

        // A hostname that is really an address would make the DNS rungs a
        // second copy of rung 1 — and `203.0.113.5.lan` resolves nowhere.
        assert_eq!(
            rungs("nil", r#""203.0.113.5""#),
            "broadcast:255.255.255.255"
        );

        // **Several addresses**, which is what version 3 is for: a laptop that
        // lives on two networks. Every known address is asked directly before
        // anything is shouted, in the order the file holds them; then one
        // global broadcast, then each network's own. Nothing is asked twice —
        // two addresses on one /24 share a directed broadcast, and a typed
        // name that is also the computer's name is one DNS rung, not two.
        assert_eq!(
            rungs(
                r#"{ { host = "198.51.100.7" }, { host = "desk", typed = true },
                     { host = "192.0.2.20" }, { host = "192.0.2.63" } }"#,
                r#""desk""#
            ),
            "cached:198.51.100.7 dns:desk cached:192.0.2.20 cached:192.0.2.63 \
             broadcast:255.255.255.255 broadcast:198.51.100.255 broadcast:192.0.2.255 \
             dns:desk.lan dns:desk.home.arpa"
        );

        // Garbage in the list is skipped, never thrown on: this runs on a
        // device, off a file the device wrote, with nobody to read a traceback.
        assert_eq!(
            rungs(r#"{ "nonsense", { host = "" }, { port = 1 } }"#, "nil"),
            "broadcast:255.255.255.255"
        );
    }

    /// `endpoint.lua` round-trips through the plugin's own writer and reader,
    /// and the installer's parser is not involved — this file is the device's.
    #[test]
    fn the_endpoint_cache_round_trips_and_is_stable() {
        let script = "local written = RB.serialiseEndpoints({\n\
             \x20 [\"bbb\"] = { { host = \"203.0.113.9\", port = 51001, seen_at = 20 } },\n\
             \x20 [\"aaa\"] = { { host = \"192.0.2.20\", port = 51000, seen_at = 10 },\n\
             \x20             { host = \"desk.lan\", typed = true } },\n\
             })\n\
             local back = RB.parseEndpoints(load(written)())\n\
             local again = RB.serialiseEndpoints(back)\n\
             return (written == again and 'stable ' or 'UNSTABLE ')\n\
                 .. back.aaa[1].host .. ':' .. back.aaa[1].port\n\
                 .. ' ' .. back.aaa[2].host .. (back.aaa[2].typed and '*' or '')\n\
                 .. ' ' .. back.bbb[1].host .. ':' .. back.bbb[1].port";
        assert_eq!(
            run_plugin_lua(script).unwrap(),
            "stable 192.0.2.20:51000 desk.lan* 203.0.113.9:51001"
        );

        // Version 2's file, one flat address per computer, is a one-address
        // list — and it is rewritten in the new shape the first time anything
        // is learned, with what it held still in it.
        assert_eq!(
            run_plugin_lua(
                r#"local old = RB.parseEndpoints({ ["dev-1"] = { host = "192.0.2.20", port = 51000, seen_at = 10 } })
                   return #old["dev-1"] .. " " .. old["dev-1"][1].host .. ":" .. old["dev-1"][1].port"#
            )
            .unwrap(),
            "1 192.0.2.20:51000"
        );

        // Absent, unreadable and half-written are one answer, and the answer is
        // a table — never a nil the caller has to test for.
        for bad in [
            "nil",
            r#""not a table""#,
            r#"{ ["a"] = { port = 1 } }"#,
            r#"{ ["a"] = { host = "" } }"#,
            r#"{ ["a"] = { { host = "" }, "junk" } }"#,
            r#"{ [1] = { host = "203.0.113.1" } }"#,
        ] {
            let n = run_plugin_lua(&format!(
                "local n = 0
                 for _ in pairs(RB.parseEndpoints({bad})) do n = n + 1 end
                 return n"
            ))
            .unwrap();
            assert_eq!(n, "0", "{bad}");
        }
    }

    /// What the reader does to one computer's list of addresses.
    ///
    /// Three pure functions, and the rule they share is what the list is for:
    /// **the order is the order to try, a learned address can be forgotten to
    /// make room, and one a person typed never is.**
    #[test]
    fn a_computer_keeps_several_addresses_and_never_drops_a_typed_one() {
        let hosts = |script: &str| {
            run_plugin_lua(&format!(
                "local list = {script}
                 local out = {{}}
                 for _, a in ipairs(list) do
                     out[#out + 1] = a.host .. (a.typed and '*' or '') .. '@' .. (a.seen_at or 0)
                 end
                 return table.concat(out, ' ')"
            ))
            .unwrap()
        };

        // The first address a computer answers at.
        assert_eq!(
            hosts(r#"RB.rememberAddress(nil, "192.0.2.20", 51000, 100)"#),
            "192.0.2.20@100"
        );
        // A new network: it goes first, and home is still known.
        assert_eq!(
            hosts(
                r#"RB.rememberAddress({ { host = "192.0.2.20", seen_at = 100 } }, "198.51.100.7", 51000, 200)"#
            ),
            "198.51.100.7@200 192.0.2.20@100"
        );
        // Home again: it moves back to the front rather than being listed twice.
        assert_eq!(
            hosts(
                r#"RB.rememberAddress({ { host = "198.51.100.7", seen_at = 200 }, { host = "192.0.2.20", seen_at = 100 } }, "192.0.2.20", 51000, 300)"#
            ),
            "192.0.2.20@300 198.51.100.7@200"
        );
        // Three learned addresses is the limit, and the oldest is what goes —
        // while both typed ones stay, wherever in the list they were, and a
        // typed address that answers is still a typed address.
        assert_eq!(
            hosts(
                r#"RB.rememberAddress({
                       { host = "a.lan", typed = true },
                       { host = "203.0.113.1", seen_at = 4 },
                       { host = "203.0.113.2", seen_at = 3 },
                       { host = "203.0.113.3", seen_at = 2 },
                       { host = "b.lan", typed = true, seen_at = 1 },
                   }, "203.0.113.9", 51000, 500)"#
            ),
            "203.0.113.9@500 a.lan*@0 203.0.113.1@4 203.0.113.2@3 b.lan*@1"
        );
        assert_eq!(
            hosts(
                r#"RB.rememberAddress({ { host = "203.0.113.1", seen_at = 4 }, { host = "b.lan", typed = true } }, "b.lan", 51000, 600)"#
            ),
            "b.lan*@600 203.0.113.1@4"
        );

        // Typing one in puts it first — the person has just said where the
        // computer is — and typing one the reader already learned marks it
        // kept, without forgetting when it last worked.
        assert_eq!(
            hosts(r#"RB.addTypedAddress({ { host = "203.0.113.1", seen_at = 4 } }, "desk.lan")"#),
            "desk.lan*@0 203.0.113.1@4"
        );
        assert_eq!(
            hosts(
                r#"RB.addTypedAddress({ { host = "203.0.113.2", seen_at = 9 }, { host = "203.0.113.1", seen_at = 4 } }, "203.0.113.1")"#
            ),
            "203.0.113.1*@4 203.0.113.2@9"
        );
        assert_eq!(
            hosts(
                r#"RB.removeAddress({ { host = "203.0.113.2", seen_at = 9 }, { host = "desk.lan", typed = true } }, "desk.lan")"#
            ),
            "203.0.113.2@9"
        );

        // What may be typed. It is written into a Lua file and handed to a
        // resolver, so it is held to the characters an address has.
        for (typed, want) in [
            ("192.0.2.20", "192.0.2.20"),
            ("  desk.lan \n", "desk.lan"),
            ("my-laptop", "my-laptop"),
            ("", "nil"),
            ("   ", "nil"),
            ("desk lan", "nil"),
            ("-desk", "nil"),
            ("desk\"; os.exit()", "nil"),
            ("http://desk", "nil"),
        ] {
            assert_eq!(
                run_plugin_lua(&format!("return tostring(RB.validAddress({typed:?}))")).unwrap(),
                want,
                "{typed:?}"
            );
        }
    }

    /// `pairing.lua` has two writers since version 3, and they agree on every
    /// byte.
    ///
    /// The installer wrote this file alone until a reader could be linked over
    /// wifi. Now the plugin rewrites it too — to add a computer, to re-key one,
    /// to unlink one — and the installer reads the result back over the next
    /// cable and rewrites its own entry. A name is the dangerous field: it is
    /// a hostname, which is not our string, and each side has its own escaper.
    #[test]
    fn both_writers_of_the_pairing_file_agree_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        let awkward = "o'brien's \"mac\" \\ book\n\ttwo";
        write_pairings(
            dir.path(),
            &[
                StoredPairing {
                    device_id: "dev-a".into(),
                    token: "tok-a".into(),
                    name: Some("laptop".into()),
                    paired_at: 1_700_000_000,
                },
                StoredPairing {
                    device_id: "dev-b".into(),
                    token: "tok-b".into(),
                    name: None,
                    paired_at: 1_700_000_100,
                },
                StoredPairing {
                    device_id: "dev-c".into(),
                    token: "tok-c".into(),
                    name: Some(awkward.into()),
                    paired_at: 0,
                },
            ],
        )
        .unwrap();
        let ours = std::fs::read_to_string(dir.path().join(PAIRING_FILE)).unwrap();

        let theirs = run_plugin_lua(&format!(
            r#"return RB.serialisePairings({{
                   {{ device_id = "dev-a", token = "tok-a", name = "laptop", paired_at = 1700000000 }},
                   {{ device_id = "dev-b", token = "tok-b", paired_at = 1700000100 }},
                   {{ device_id = "dev-c", token = "tok-c", name = {awkward:?}, paired_at = 0 }},
               }})"#
        ))
        .unwrap();
        assert_eq!(ours, theirs);

        // And the installer reads the plugin's file back to exactly the list
        // that went in — awkward name and all.
        std::fs::write(dir.path().join(PAIRING_FILE), &theirs).unwrap();
        let back = read_stored_pairings(dir.path()).unwrap();
        assert_eq!(back.len(), 3);
        assert_eq!(back[2].name.as_deref(), Some(awkward));
        assert_eq!(back[1].name, None);

        // Linking again replaces an entry where it stands; unlinking removes
        // one and leaves the rest in order. Other computers' secrets are never
        // touched by either, which is the installer's own rule.
        let edited = run_plugin_lua(
            r#"local list = {
                   { device_id = "a", token = "ta" },
                   { device_id = "b", token = "tb" },
                   { device_id = "c", token = "tc" },
               }
               list = RB.upsertPairing(list, { device_id = "b", token = "NEW" })
               list = RB.upsertPairing(list, { device_id = "d", token = "td" })
               list = RB.removePairing(list, "a")
               local out = {}
               for _, e in ipairs(list) do out[#out + 1] = e.device_id .. "=" .. e.token end
               return table.concat(out, " ")"#,
        )
        .unwrap();
        assert_eq!(edited, "b=NEW c=tc d=td");
    }

    /// The code the reader shows is one the computer accepts, however it is
    /// typed back.
    #[test]
    fn a_code_the_reader_shows_is_a_code_the_computer_accepts() {
        use crate::wireless::{PAIRING_CODE_LEN, normalise_code};
        // Every byte value, so every symbol of the alphabet is produced.
        for start in [0u8, 16, 100, 240] {
            let bytes: String = (0..16)
                .map(|i| format!("\\{}", start.wrapping_add(i * 7)))
                .collect();
            let shown = run_plugin_lua(&format!(
                "return RB.formatCode(RB.codeFromBytes(\"{bytes}\"))"
            ))
            .unwrap();
            assert_eq!(shown.len(), PAIRING_CODE_LEN + 3, "{shown}");
            let code = normalise_code(&shown).unwrap_or_else(|| panic!("{shown} was refused"));
            assert_eq!(code, shown.replace('-', ""));
            // Typed in lower case with spaces instead of hyphens: the same code.
            assert_eq!(
                normalise_code(&shown.to_lowercase().replace('-', " ")).as_deref(),
                Some(code.as_str())
            );
        }
        // Too few random bytes is no code at all, never a short one.
        assert_eq!(
            run_plugin_lua(r#"return tostring(RB.codeFromBytes("too short"))"#).unwrap(),
            "nil"
        );
        assert_eq!(
            run_plugin_lua(r#"return RB.hexFromBytes("\0\1\254\255")"#).unwrap(),
            "0001feff"
        );
    }

    /// Run the reader's half of a wifi pairing against the computer's half.
    ///
    /// Returns what the reader would send and store, as
    /// `nonce|device_id|reader|plugin|mac|token|name|paired_at|proof`, or `silent`.
    fn reader_answers(code: &str, pairings: &str, pair: &crate::wireless::Pair) -> String {
        let known = pair
            .known
            .iter()
            .map(|k| format!("{k:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        run_plugin_lua(&format!(
            r#"local sha = require("ffi/sha2")
               local state = {{ code = {code:?}, pairings = {pairings}, plugin = 3, reader = "Kindle" }}
               local msg = {{ v = {v}, kind = {kind:?}, nonce = {nonce:?}, at = {at},
                             name = {name:?}, known = {{ {known} }}, mac = {mac:?} }}
               local reply, entry = RB.answerPair(state, msg, RB.hmac,
                   function() return "reader-nonce", "00112233445566778899aabbccddeeff" end)
               if not reply then return "silent" end
               -- Asked twice, it says the same thing and stores nothing new.
               local again, second = RB.answerPair(state, msg, RB.hmac,
                   function() return "DIFFERENT", "ffffffffffffffffffffffffffffffff" end)
               assert(again == reply and second == nil, "a repeated request must get the same answer")
               -- A different request under the same code gets nothing: one code, one computer.
               local other = {{}}
               for k, v in pairs(msg) do other[k] = v end
               other.nonce = "another-nonce"
               other.mac = RB.hmac(state.code, RB.pairChallenge(other.nonce, other.at, other.known, other.name))
               assert(RB.answerPair(state, other, RB.hmac, function() return "x", "y" end) == nil,
                   "a used code must not link a second computer")
               return table.concat({{ reply.nonce, reply.device_id, reply.reader, reply.plugin,
                   reply.mac, entry.token, tostring(entry.name), entry.paired_at,
                   tostring(reply.proof) }}, "|")"#,
            v = pair.v,
            kind = pair.kind,
            nonce = pair.nonce,
            at = pair.at,
            name = pair.name,
            mac = pair.mac,
        ))
        .unwrap()
    }

    fn a_pair(code: &str, known: &[&str], name: &str) -> crate::wireless::Pair {
        use crate::wireless::{PROTOCOL_VERSION, mac, pair_challenge};
        let known: Vec<String> = known.iter().map(|s| s.to_string()).collect();
        crate::wireless::Pair {
            v: PROTOCOL_VERSION,
            kind: "pair".into(),
            nonce: "desk-nonce".into(),
            at: 1_700_000_000,
            name: name.into(),
            mac: mac(
                code,
                &pair_challenge("desk-nonce", 1_700_000_000, &known, name),
            ),
            known,
        }
    }

    /// **The two halves of a wifi pairing, run against each other.**
    ///
    /// The exchange is implemented twice — `wireless.rs` for the computer, this
    /// plugin's `answerPair` for the reader — and nothing but a test stands
    /// between them and a pairing that both sides report as made while holding
    /// different tokens. That failure has no symptom until the first push,
    /// which then fails as *could not reach bookmole* on a network where it
    /// plainly can. So the reader's actual Lua answers the computer's actual
    /// request here, and the computer's own functions check the answer.
    #[test]
    fn the_readers_half_of_a_wifi_pairing_agrees_with_the_computers() {
        use crate::wireless::{mac, paired_challenge, token_challenge, verify};
        let code = "0123456789ABCDEF";

        // A computer this reader has never met. The name is one a hostname
        // could really be, colon and all, since it sits inside a signed string.
        let pair = a_pair(code, &["aaaa", "bbbb"], "ann's mac:2");
        let said = reader_answers(code, "{}", &pair);
        let f: Vec<&str> = said.split('|').collect();
        let (nonce, device_id, reader, plugin, their_mac, token, name, paired_at) =
            (f[0], f[1], f[2], f[3], f[4], f[5], f[6], f[7]);
        assert_eq!(device_id, "00112233445566778899aabbccddeeff", "a fresh id");
        assert_eq!((reader, plugin), ("Kindle", "3"));
        assert!(
            verify(
                code,
                &paired_challenge("desk-nonce", nonce, device_id, 3, reader),
                their_mac
            ),
            "the computer must be able to verify the reader's answer"
        );
        assert_eq!(
            token,
            mac(code, &token_challenge("desk-nonce", nonce, device_id)),
            "both sides must derive the same token"
        );
        assert_eq!(token.len(), 64, "the shape every other token has");
        assert_eq!(name, "ann's mac:2");
        assert_eq!(f[8], "nil", "a new reader has no old token to prove");
        assert_eq!(
            paired_at, "1700000000",
            "the computer's clock, not the reader's"
        );

        // A computer this reader already knows: it names an id the reader
        // holds, and gets that id back — with a new token, and with the date
        // the relationship began rather than today's.
        let pair = a_pair(code, &["aaaa", "dev-old"], "desk");
        let said = reader_answers(
            code,
            r#"{ { device_id = "other", token = "t0", paired_at = 5 },
                 { device_id = "dev-old", token = "t1", paired_at = 1600000000 } }"#,
            &pair,
        );
        let f: Vec<&str> = said.split('|').collect();
        assert_eq!(f[1], "dev-old", "recognised, not duplicated");
        assert_ne!(f[5], "t1", "re-keyed");
        assert_eq!(
            f[5],
            mac(code, &token_challenge("desk-nonce", f[0], "dev-old"))
        );
        assert_eq!(f[7], "1600000000");
        // And it proves it is that reader, with the token it held *before* —
        // which is what the computer checks against its own row, and the only
        // thing standing between a code and somebody else's place.
        assert!(
            verify(
                "t1",
                &crate::wireless::rekey_challenge("desk-nonce", f[0], "dev-old"),
                f[8]
            ),
            "a reader answering as one the computer knows must prove the old token"
        );

        // Everything that is not a correctly signed request is met with
        // silence — a wrong code first, since that is the one a person causes.
        assert_eq!(reader_answers("FEDCBA9876543210", "{}", &pair), "silent");
        let mut tampered = a_pair(code, &["aaaa"], "desk");
        tampered.name = "evil".into();
        assert_eq!(reader_answers(code, "{}", &tampered), "silent");
        let mut tampered = a_pair(code, &["aaaa"], "desk");
        tampered.known.push("dev-old".into());
        assert_eq!(
            reader_answers(
                code,
                r#"{ { device_id = "dev-old", token = "t1" } }"#,
                &tampered
            ),
            "silent",
            "the list of ids is signed: adding one must not take over an entry"
        );
        let mut wrong_kind = a_pair(code, &[], "desk");
        wrong_kind.kind = "hello".into();
        assert_eq!(reader_answers(code, "{}", &wrong_kind), "silent");
        let mut wrong_version = a_pair(code, &[], "desk");
        wrong_version.v = 2;
        assert_eq!(reader_answers(code, "{}", &wrong_version), "silent");
    }

    /// The challenge strings the plugin builds must be byte-identical to the
    /// ones the listener signs — they are the only thing keeping the two
    /// implementations of the same protocol honest, and they are built by
    /// `string.format` on one side and `format!` on the other.
    #[test]
    fn the_challenges_the_plugin_builds_match_the_listeners() {
        use crate::wireless::{
            body_challenge, here_challenge, open_challenge, pair_challenge, paired_challenge,
            token_challenge,
        };
        let built = run_plugin_lua(
            "return string.format('here:%d:%s:%d', 1, 'n1', 51000) .. '|' ..
                    string.format('open:%d:%s', 1, 'n1') .. '|' ..
                    string.format('body:%d:%s:%s', 1, 'n1', 'deadbeef') .. '|' ..
                    RB.pairChallenge('n1', 1700000000, { 'aa', 'bb' }, 'desk') .. '|' ..
                    RB.pairChallenge('n1', 1700000000, {}, '') .. '|' ..
                    RB.pairedChallenge('n1', 'n2', 'dev', 3, 'Kindle') .. '|' ..
                    RB.tokenChallenge('n1', 'n2', 'dev') .. '|' ..
                    RB.rekeyChallenge('n1', 'n2', 'dev')",
        )
        .unwrap();
        assert_eq!(
            built,
            format!(
                "{}|{}|{}|{}|{}|{}|{}|{}",
                here_challenge("n1", 51000),
                open_challenge("n1"),
                body_challenge("n1", "deadbeef"),
                pair_challenge("n1", 1_700_000_000, &["aa".into(), "bb".into()], "desk"),
                pair_challenge("n1", 1_700_000_000, &[], ""),
                paired_challenge("n1", "n2", "dev", 3, "Kindle"),
                token_challenge("n1", "n2", "dev"),
                crate::wireless::rekey_challenge("n1", "n2", "dev")
            )
        );
    }

    /// **The device half of a wifi pairing, run end to end against a real
    /// plugin directory** — everything `answerPair` is wrapped in.
    ///
    /// The pure function is tested above. What is left is glue, and glue is
    /// where a link goes wrong without anything looking wrong: the entry has to
    /// reach `pairing.lua` *before* the reply is sent, the address has to reach
    /// `endpoint.lua`, the repeat of a request must not write twice, and none
    /// of it may leave a file behind that the installer does not recognise —
    /// since that would make the reader refuse its next install *and* its next
    /// uninstall. No reader runs in CI, so the socket and the JSON codec are
    /// stood in and the rest is the plugin's own code on a real directory.
    #[test]
    fn a_code_typed_on_a_computer_links_it_on_the_reader() {
        use crate::wireless::{mac, paired_challenge, token_challenge, verify};
        let tmp = mount();
        install(
            tmp.path(),
            "dev-a",
            "tok-a",
            1_600_000_000,
            Some("laptop"),
            None,
        )
        .unwrap();
        let dir = dir_of(tmp.path());

        // `/dev/urandom` answers sevens, so the code on the "screen" is known:
        // sixteen 7s. The same bytes mint the nonce and the id.
        let code = "7777777777777777";
        let minted = "07".repeat(16);
        let pair = a_pair(code, &["zzzz"], "desk");
        let wrong = a_pair("0000000000000000", &["zzzz"], "desk");
        let as_lua = |p: &crate::wireless::Pair| {
            format!(
                "{{ v = {}, kind = {:?}, nonce = {:?}, at = {}, name = {:?}, known = {{ \"zzzz\" }}, mac = {:?} }}",
                p.v, p.kind, p.nonce, p.at, p.name, p.mac
            )
        };

        let said = run_plugin_lua(&format!(
            r#"local real_open = io.open
               io.open = function(path, mode)
                   if path == "/dev/urandom" then
                       return {{ read = function(_, n) return string.rep("\7", n) end,
                                close = function() end }}
                   end
                   return real_open(path, mode)
               end
               local inbox = {{ GOOD = {good}, WRONG = {wrong} }}
               local queue = {{ "WRONG", "GOOD", "GOOD" }}
               local sent, closed = {{}}, false
               __rb_modules.json = {{
                   decode = function(s) return inbox[s] end,
                   encode = function(t) return t end,
               }}
               __rb_modules.socket = {{ udp4 = function() return {{
                   setoption = function() return true end,
                   setsockname = function() return true end,
                   settimeout = function() end,
                   setpeername = function() return nil end,
                   receivefrom = function()
                       local next = table.remove(queue, 1)
                       if next then return next, "192.0.2.63", 40000 end
                   end,
                   sendto = function(_, payload, host, port)
                       sent[#sent + 1] = {{ payload = payload, to = host .. ":" .. port }}
                       return true
                   end,
                   close = function() closed = true end,
               }} end }}

               RB.path = {path:?}
               RB.ui = require("anything")
               RB:init()
               RB:openPairing()
               assert(RB:pairingOpen(), "a code is showing")

               -- A wrong code: silence, and nothing written.
               RB.pairing.responder:waitEvent()
               assert(#sent == 0 and #RB.pairings == 1, "a wrong code changes nothing")
               -- The right one, and then the computer asking again.
               RB.pairing.responder:waitEvent()
               RB.pairing.responder:waitEvent()
               assert(#sent == 2 and sent[1].payload == sent[2].payload,
                   "a repeated request gets the same answer")
               -- Nothing more arrives; polling an empty socket is not an error.
               RB.pairing.responder:waitEvent()

               RB:closePairing("test")
               assert(closed and not RB:pairingOpen(), "the code is withdrawn")
               RB:closePairing("twice")

               local labels = {{}}
               for _, it in ipairs(RB:buildMenu()) do
                   labels[#labels + 1] = it.text or it.text_func()
               end
               local r = sent[1].payload
               return table.concat({{ r.kind, r.nonce, r.device_id, r.reader, r.plugin, r.mac,
                   sent[1].to, #RB.pairings, labels[1], labels[2] }}, "|")"#,
            good = as_lua(&pair),
            wrong = as_lua(&wrong),
            path = dir.to_string_lossy()
        ))
        .unwrap();
        let f: Vec<&str> = said.split('|').collect();
        assert_eq!(f[0], "paired");
        assert_eq!((f[1], f[2]), (minted.as_str(), minted.as_str()));
        // The version comes from the `_meta.lua` that is actually installed.
        assert_eq!(f[4], PLUGIN_VERSION.to_string());
        assert!(verify(
            code,
            &paired_challenge("desk-nonce", f[1], f[2], PLUGIN_VERSION, f[3]),
            f[5]
        ));
        assert_eq!(f[6], "192.0.2.63:40000", "answered where it was asked from");
        assert_eq!(f[7], "2");
        assert_eq!(
            (f[8], f[9]),
            ("Send highlights to laptop", "Send highlights to desk"),
            "the new computer is in the menu without a restart"
        );

        // On the "device": the cable's computer untouched, and the new one
        // beside it holding the token both sides derived.
        let stored = read_stored_pairings(&dir).unwrap();
        assert_eq!(stored.len(), 2);
        assert_eq!(
            (stored[0].device_id.as_str(), stored[0].token.as_str()),
            ("dev-a", "tok-a")
        );
        assert_eq!(stored[1].device_id, minted);
        assert_eq!(stored[1].name.as_deref(), Some("desk"));
        assert_eq!(stored[1].paired_at, 1_700_000_000);
        assert_eq!(
            stored[1].token,
            mac(code, &token_challenge("desk-nonce", &minted, &minted))
        );
        // Where the request came from is where the computer is.
        let endpoints = read_endpoints(&dir).unwrap();
        assert_eq!(endpoints.len(), 1);
        assert_eq!(
            (endpoints[0].device_id.as_str(), endpoints[0].host.as_str()),
            (minted.as_str(), "192.0.2.63")
        );

        // **And the installer still recognises its own directory.** A stray
        // file here would refuse the next install and the next uninstall.
        let status = inspect(tmp.path()).unwrap();
        assert!(!status.is_obstructed(), "{status:?}");
        assert_eq!(status.pairings.len(), 2);
        // A cable afterwards rewrites our entry and keeps the wifi one whole.
        install(
            tmp.path(),
            "dev-a",
            "tok-a",
            1_600_000_000,
            Some("laptop"),
            None,
        )
        .unwrap();
        assert_eq!(read_stored_pairings(&dir).unwrap(), stored);
        let report = uninstall(tmp.path()).unwrap();
        assert_eq!(report.removed_pairings.len(), 2);
        assert!(!dir.exists(), "uninstall is still exact");
    }

    /// Which address a reader is told to look for this computer at.
    ///
    /// The second case is the one that was wrong on a real machine: with a VPN
    /// up, the routing table's answer is the tunnel's address, and a Kindle on
    /// `192.0.2.0/24` was told to probe `198.51.100.2`.
    #[test]
    fn a_reader_is_told_the_networks_address_and_never_the_tunnels() {
        let ip = |s: &str| s.parse::<std::net::Ipv4Addr>().unwrap();
        let net = |a: &str| LanAddress {
            address: ip(a),
            broadcast: None,
        };
        let pick = |nets: Option<&[LanAddress]>, routed: Option<&str>| {
            choose_lan_address(nets, routed.map(ip)).map(|a| a.to_string())
        };

        // The ordinary laptop: one network, and the route agrees.
        let wifi = [net("192.0.2.222")];
        assert_eq!(
            pick(Some(&wifi), Some("192.0.2.222")).as_deref(),
            Some("192.0.2.222")
        );
        // A VPN owns the default route. The tunnel is not in the list of
        // networks at all — it is point-to-point — so the route's answer is
        // recognised as not-a-network and the wifi address is used.
        assert_eq!(
            pick(Some(&wifi), Some("198.51.100.2")).as_deref(),
            Some("192.0.2.222")
        );
        // Wifi and ethernet both: the one the machine actually prefers.
        let both = [net("192.0.2.222"), net("192.0.2.40")];
        assert_eq!(
            pick(Some(&both), Some("192.0.2.40")).as_deref(),
            Some("192.0.2.40")
        );
        // No route at all, and a choice to make: a private address before a
        // public one, and a self-assigned one only when it is all there is.
        let mixed = [net("169.254.7.7"), net("203.0.113.9"), net("10.0.0.5")];
        assert_eq!(pick(Some(&mixed), None).as_deref(), Some("10.0.0.5"));
        assert_eq!(
            pick(Some(&[net("169.254.7.7")]), None).as_deref(),
            Some("169.254.7.7")
        );
        // Only a tunnel: nothing, rather than an address no reader can reach.
        assert_eq!(pick(Some(&[]), Some("198.51.100.2")), None);
        // The interfaces could not be listed: what the routing table says,
        // which is all this function used to know.
        assert_eq!(
            pick(None, Some("198.51.100.2")).as_deref(),
            Some("198.51.100.2")
        );
        assert_eq!(pick(None, None), None);
    }

    /// The real interfaces of whatever machine this runs on. Nothing can be
    /// asserted about *which* addresses it has — only that what comes back is
    /// the shape a caller relies on, and that asking does not fail.
    #[test]
    fn the_broadcast_list_always_starts_with_the_limited_broadcast() {
        let all = broadcast_addresses();
        assert_eq!(all[0], std::net::Ipv4Addr::BROADCAST);
        let mut unique = all.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), all.len(), "no address is named twice");
        for network in lan_interfaces().unwrap_or_default() {
            assert!(!network.address.is_loopback());
        }
    }

    #[test]
    fn minted_ids_are_the_length_asked_for_and_not_each_other() {
        let a = mint_id(16).unwrap();
        let b = mint_id(16).unwrap();
        assert_eq!(a.len(), 32);
        assert_eq!(mint_id(32).unwrap().len(), 64);
        assert_ne!(a, b);
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            /// Install then uninstall is the identity on the rest of the
            /// volume, whatever else is on it. The example-based test above
            /// fixes one tree; this one says the property does not depend on
            /// which.
            #[test]
            fn install_and_uninstall_restore_any_tree(
                extras in proptest::collection::vec("[a-z]{1,8}", 0..6)
            ) {
                let tmp = mount();
                for (i, name) in extras.iter().enumerate() {
                    let d = tmp.path().join("koreader").join("plugins").join(format!("{name}.koplugin"));
                    std::fs::create_dir_all(&d).unwrap();
                    std::fs::write(d.join("main.lua"), format!("-- plugin {i}\n")).unwrap();
                }
                let before = snapshot(tmp.path());
                install_here(tmp.path());
                // The device learns an endpoint between the two, which is the
                // ordinary case once a reader has pushed — and it is the file
                // that used to make `uninstall` refuse, so the property has to
                // be asserted with one present rather than beside it.
                std::fs::write(
                    dir_of(tmp.path()).join(ENDPOINT_FILE),
                    "return { [\"dev-1\"] = { host = \"203.0.113.2\", port = 1 } }\n",
                ).unwrap();
                uninstall(tmp.path()).unwrap();
                prop_assert_eq!(snapshot(tmp.path()), before);
            }
        }
    }
}
