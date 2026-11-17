//! Whether the library's database sits inside a cloud-synced folder.
//!
//! A live SQLite library in a synced folder gets damaged: the sync client
//! copies `app.db` and its `-wal`/`-shm` siblings at different moments, so the
//! copy on the other side (and, after a round trip, the copy here) is three
//! files from three instants. Every frontend says so once per launch and opens
//! the library anyway; nothing here refuses anything.
//!
//! **This module is the fact, never the sentence.** [`cloud_folder_from`]
//! answers *which kind of synced location, rooted where*, as a [`CloudFolder`];
//! the wording is each frontend's. Derived facts live in the engine so every
//! frontend agrees on them; phrasing does not, so each can say it its own way.
//!
//! ## What is judged: the database's folder, not the vault
//!
//! The vault is plain markdown and is safe in a synced folder — it may stay
//! there. What is at risk is the folder holding `app.db` and its siblings, so
//! [`LibraryLocation::of`] judges the parent of the file named by
//! `EngineConfig::db_url`. A config whose vault lives in iCloud Drive and whose
//! database does not produces nothing, and that case is tested by name.
//!
//! ## The roots, and how sure each one is
//!
//! Each root below is marked **certain** (the client's documented or
//! long-standing fixed location, or a variable the client itself sets) or
//! **conventional** (the client's default folder, which a user may move; a
//! moved folder is simply not recognised, which is the quiet failure).
//!
//! macOS (`$HOME`):
//! - `~/Library/Mobile Documents` — iCloud Drive, `com~apple~CloudDocs` and
//!   every app's iCloud container under it. **Certain.**
//! - `~/Library/CloudStorage/<X>` — the File Provider location every modern
//!   sync client uses (Dropbox, Google Drive, OneDrive, Proton Drive, Box and
//!   others). **Certain** that anything under it is synced; which client is
//!   read off `<X>`'s prefix (`Dropbox`, `GoogleDrive-…`, `OneDrive-…`,
//!   `ProtonDrive-…`, `Box-…`), which is **conventional**, and an unknown
//!   prefix is [`CloudService::CloudStorage`] rather than no answer.
//! - `~/Dropbox`, `~/Google Drive`, `~/OneDrive` — the clients' folders from
//!   before File Provider. **Conventional.**
//! - `/Volumes/GoogleDrive`, `/Volumes/GoogleDrive-<id>` — Google Drive File
//!   Stream's old mount. **Conventional.**
//! - **Not covered: `~/Desktop` and `~/Documents`.** iCloud's "Desktop &
//!   Documents Folders" syncs both, and the path alone cannot say whether it is
//!   on. The signals that could (Finder's preference keys, a File Provider
//!   extended attribute) are undocumented and would need a plist reader or a
//!   subprocess; a guess either way is wrong for somebody, and warning on every
//!   `~/Documents` library would be a warning nobody believes. Left out.
//!
//! Windows (`%USERPROFILE%`):
//! - `%OneDrive%`, `%OneDriveConsumer%`, `%OneDriveCommercial%` — set by the
//!   OneDrive client to its own folders. **Certain** when set.
//! - `\Dropbox`, `\OneDrive`, `\iCloudDrive` — the clients' default folders.
//!   **Conventional.**
//! - `\Google Drive` (Backup and Sync's old folder) and `\Proton Drive`.
//!   **Conventional.**
//! - `<drive>:\My Drive` and `<drive>:\Shared drives` — Google Drive for
//!   desktop's virtual drive. **Conventional**: a folder by that exact name at
//!   a drive's root is not something else in practice.
//!
//! Linux and other Unix (`$HOME`): `~/Dropbox` (Dropbox's own client,
//! **certain** default) and `~/OneDrive` (the `onedrive` client's default,
//! **conventional**). Nothing else: Google Drive and Proton Drive ship no
//! Linux client whose folder is a fixed path, and GNOME's online-accounts
//! mounts are not a place anybody puts a library.
//!
//! ## How a path is compared
//!
//! On components, after both sides are absolute, lexically (`.` dropped, `..`
//! popped), and case-insensitively on macOS and Windows, whose default file
//! systems are. A sibling whose name merely *starts with* a root's name
//! (`~/Dropbox2`) is not under it. The pure function touches no file system;
//! [`cloud_folder`], the thin wrapper, additionally canonicalizes both the path
//! and the home so a symlinked data dir is seen through, and tolerates either
//! canonicalization failing.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::data_dir::Platform;

/// Which kind of synced location matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CloudService {
    ICloudDrive,
    Dropbox,
    GoogleDrive,
    OneDrive,
    ProtonDrive,
    Box,
    /// Under macOS's `~/Library/CloudStorage` with a folder prefix this module
    /// does not name: some File Provider client, synced all the same.
    CloudStorage,
}

/// The fact: the path is inside `root`, which `service` syncs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudFolder {
    pub service: CloudService,
    /// The synced root the path is inside, spelled as the *path* spells it.
    pub root: PathBuf,
}

/// Where the library's database is, and whether that is inside a synced
/// folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryLocation {
    /// The folder holding the database file and its `-wal`/`-shm` siblings.
    /// `None` for an in-memory database, which is in no folder at all.
    pub database_dir: Option<PathBuf>,
    pub cloud_folder: Option<CloudFolder>,
}

impl LibraryLocation {
    /// Judge the folder of the database `db_url` names with `detect`.
    ///
    /// [`crate::Engine::library_location`] passes [`cloud_folder`]; a test
    /// passes [`cloud_folder_from`] with its own environment, so the
    /// composition the engine runs is the one the tests run.
    pub fn of(db_url: &str, detect: impl Fn(&Path) -> Option<CloudFolder>) -> LibraryLocation {
        let database_dir = database_dir(db_url);
        let cloud_folder = database_dir.as_deref().and_then(detect);
        LibraryLocation {
            database_dir,
            cloud_folder,
        }
    }
}

/// The folder holding the database a sqlx SQLite URL names, or `None` for an
/// in-memory one. Relative stays relative; the caller makes it absolute.
pub fn database_dir(db_url: &str) -> Option<PathBuf> {
    let rest = db_url
        .strip_prefix("sqlite://")
        .or_else(|| db_url.strip_prefix("sqlite:"))
        .unwrap_or(db_url);
    let file = rest.split('?').next().unwrap_or("");
    if file.is_empty() || file.starts_with(":memory:") {
        return None;
    }
    match Path::new(file).parent() {
        Some(p) if !p.as_os_str().is_empty() => Some(p.to_path_buf()),
        _ => Some(PathBuf::from(".")),
    }
}

/// [`cloud_folder_from`] with the real environment and platform, and a second
/// look through symlinks: if the path as given matches nothing, the canonical
/// path is judged against the canonical home. Either canonicalization failing
/// just means that look is skipped.
pub fn cloud_folder(path: &Path) -> Option<CloudFolder> {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    cloud_folder_through(
        &abs,
        |name| std::env::var_os(name),
        Platform::current(),
        |p| std::fs::canonicalize(p).ok(),
    )
}

/// The wrapper's two looks, with the canonicalizer passed in so a test can run
/// it against a temp tree and its own home.
fn cloud_folder_through(
    path: &Path,
    env: impl Fn(&str) -> Option<OsString>,
    platform: Platform,
    canonical: impl Fn(&Path) -> Option<PathBuf>,
) -> Option<CloudFolder> {
    if let Some(found) = cloud_folder_from(path, &env, platform) {
        return Some(found);
    }
    let canon = canonical(path)?;
    let home_var = home_var(platform);
    let canon_home = env(home_var)
        .and_then(|h| canonical(Path::new(&h)))
        .map(PathBuf::into_os_string);
    cloud_folder_from(
        &canon,
        |name| match (&canon_home, name == home_var) {
            (Some(h), true) => Some(h.clone()),
            _ => env(name),
        },
        platform,
    )
}

/// The rule itself, with everything it reads passed in.
///
/// `path` must be absolute for `platform` (a relative one is no answer);
/// `env` looks up `HOME` (macOS, Unix) or `USERPROFILE` and the OneDrive
/// variables (Windows). Pure: no file system, no process state.
pub fn cloud_folder_from(
    path: &Path,
    env: impl Fn(&str) -> Option<OsString>,
    platform: Platform,
) -> Option<CloudFolder> {
    let fold = platform != Platform::Unix;
    let comps = components(&path.to_string_lossy(), platform)?;
    let home = env(home_var(platform))
        .filter(|h| !h.is_empty())
        .and_then(|h| components(&h.to_string_lossy(), platform));

    let mut roots: Vec<(CloudService, Vec<String>)> = Vec::new();
    let under_home = |tail: &[&str]| -> Option<Vec<String>> {
        let mut r = home.clone()?;
        r.extend(tail.iter().map(|s| s.to_string()));
        Some(r)
    };
    match platform {
        Platform::MacOs => {
            for (service, tail) in [
                (
                    CloudService::ICloudDrive,
                    &["Library", "Mobile Documents"][..],
                ),
                (CloudService::Dropbox, &["Dropbox"][..]),
                (CloudService::GoogleDrive, &["Google Drive"][..]),
                (CloudService::OneDrive, &["OneDrive"][..]),
            ] {
                roots.extend(under_home(tail).map(|r| (service, r)));
            }
            // `~/Library/CloudStorage/<X>`: the provider folder is the root,
            // so the container itself is not a match and anything below a
            // provider is.
            if let Some(container) = under_home(&["Library", "CloudStorage"])
                && let Some(provider) = comps.get(container.len())
                && starts_with(&comps, &container, fold)
            {
                let mut r = container;
                r.push(provider.clone());
                roots.push((provider_service(provider), r));
            }
            // Google Drive File Stream's old mount, `/Volumes/GoogleDrive[-id]`.
            if comps.len() >= 2
                && same(&comps[0], "Volumes", fold)
                && has_prefix_name(&comps[1], "GoogleDrive")
            {
                roots.push((CloudService::GoogleDrive, comps[..2].to_vec()));
            }
        }
        Platform::Unix => {
            for (service, tail) in [
                (CloudService::Dropbox, "Dropbox"),
                (CloudService::OneDrive, "OneDrive"),
            ] {
                roots.extend(under_home(&[tail]).map(|r| (service, r)));
            }
        }
        Platform::Windows => {
            for var in ["OneDrive", "OneDriveConsumer", "OneDriveCommercial"] {
                if let Some(r) = env(var)
                    .filter(|v| !v.is_empty())
                    .and_then(|v| components(&v.to_string_lossy(), platform))
                {
                    roots.push((CloudService::OneDrive, r));
                }
            }
            for (service, tail) in [
                (CloudService::Dropbox, "Dropbox"),
                (CloudService::OneDrive, "OneDrive"),
                (CloudService::GoogleDrive, "Google Drive"),
                (CloudService::ICloudDrive, "iCloudDrive"),
                (CloudService::ProtonDrive, "Proton Drive"),
            ] {
                roots.extend(under_home(&[tail]).map(|r| (service, r)));
            }
            // Google Drive for desktop's virtual drive: `G:\My Drive\…`.
            if comps.len() >= 2
                && is_drive(&comps[0])
                && (same(&comps[1], "My Drive", fold) || same(&comps[1], "Shared drives", fold))
            {
                roots.push((CloudService::GoogleDrive, comps[..2].to_vec()));
            }
        }
    }

    roots
        .into_iter()
        // A root of one component would be a whole drive or `/`; nothing
        // above syncs that, and a bad variable must not make everything match.
        .filter(|(_, r)| r.len() >= 2)
        .find(|(_, r)| starts_with(&comps, r, fold))
        .map(|(service, r)| CloudFolder {
            service,
            root: spell(&comps[..r.len()], platform),
        })
}

/// The variable a platform's home is read from.
fn home_var(platform: Platform) -> &'static str {
    match platform {
        Platform::Windows => "USERPROFILE",
        Platform::MacOs | Platform::Unix => "HOME",
    }
}

/// Which client a `~/Library/CloudStorage` folder belongs to, by the prefix
/// each client names its folder with.
fn provider_service(name: &str) -> CloudService {
    for (prefix, service) in [
        ("Dropbox", CloudService::Dropbox),
        ("GoogleDrive", CloudService::GoogleDrive),
        ("OneDrive", CloudService::OneDrive),
        ("ProtonDrive", CloudService::ProtonDrive),
        ("Box", CloudService::Box),
    ] {
        if has_prefix_name(name, prefix) {
            return service;
        }
    }
    CloudService::CloudStorage
}

/// `name` is `prefix` or `prefix-…`, ignoring case. `Boxcryptor` is not Box.
fn has_prefix_name(name: &str, prefix: &str) -> bool {
    let (n, p) = (name.to_lowercase(), prefix.to_lowercase());
    n == p || n.strip_prefix(&p).is_some_and(|rest| rest.starts_with('-'))
}

fn same(a: &str, b: &str, fold: bool) -> bool {
    if fold {
        a.to_lowercase() == b.to_lowercase()
    } else {
        a == b
    }
}

fn starts_with(path: &[String], root: &[String], fold: bool) -> bool {
    path.len() >= root.len() && path.iter().zip(root).all(|(a, b)| same(a, b, fold))
}

fn is_drive(c: &str) -> bool {
    let b = c.as_bytes();
    b.len() == 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

/// An absolute path's components, lexically normalized, or `None` when the
/// text is not absolute on `platform`. Judged on the text, so the answer does
/// not depend on the host the test runs on.
///
/// Unix and macOS: the components after the leading `/`. Windows: a drive
/// (`C:`) or a UNC head (`\\server`) first, then the rest, split on either
/// slash; a verbatim `\\?\` prefix (what `canonicalize` returns there) is
/// dropped first.
fn components(text: &str, platform: Platform) -> Option<Vec<String>> {
    if platform != Platform::Windows {
        return normalize(Vec::new(), text.strip_prefix('/')?, platform);
    }
    let t = text
        .strip_prefix(r"\\?\UNC\")
        .map(|r| format!(r"\\{r}"))
        .or_else(|| text.strip_prefix(r"\\?\").map(str::to_string))
        .unwrap_or_else(|| text.to_string());
    if let Some(unc) = t.strip_prefix(r"\\").or_else(|| t.strip_prefix("//")) {
        let mut parts = unc.splitn(2, ['\\', '/']);
        let server = parts.next().filter(|s| !s.is_empty())?;
        let rest = parts.next().unwrap_or("");
        return normalize(vec![format!(r"\\{server}")], rest, platform);
    }
    let drive = t.get(..2).filter(|d| is_drive(d))?;
    let rest = t[2..].strip_prefix(['\\', '/'])?;
    normalize(vec![drive.to_string()], rest, platform)
}

fn normalize(mut out: Vec<String>, rest: &str, platform: Platform) -> Option<Vec<String>> {
    let floor = out.len();
    let seps: &[char] = match platform {
        Platform::Windows => &['\\', '/'],
        Platform::MacOs | Platform::Unix => &['/'],
    };
    for c in rest.split(seps) {
        match c {
            "" | "." => {}
            ".." => {
                if out.len() > floor {
                    out.pop();
                }
            }
            c => out.push(c.to_string()),
        }
    }
    Some(out)
}

/// Components back into a path, in the platform's own spelling.
fn spell(comps: &[String], platform: Platform) -> PathBuf {
    match platform {
        Platform::MacOs | Platform::Unix => PathBuf::from(format!("/{}", comps.join("/"))),
        Platform::Windows => PathBuf::from(comps.join("\\")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EngineConfig;
    use std::collections::HashMap;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + use<> {
        let map: HashMap<String, OsString> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), OsString::from(v)))
            .collect();
        move |name| map.get(name).cloned()
    }

    fn on(path: &str, pairs: &[(&str, &str)], platform: Platform) -> Option<CloudFolder> {
        cloud_folder_from(Path::new(path), env_of(pairs), platform)
    }

    fn found(service: CloudService, root: &str) -> Option<CloudFolder> {
        Some(CloudFolder {
            service,
            root: PathBuf::from(root),
        })
    }

    const MAC_HOME: [(&str, &str); 1] = [("HOME", "/Users/u")];

    #[test]
    fn macos_roots() {
        use CloudService::*;
        let cases: &[(&str, Option<CloudFolder>)] = &[
            (
                "/Users/u/Library/Mobile Documents/com~apple~CloudDocs/rb",
                found(ICloudDrive, "/Users/u/Library/Mobile Documents"),
            ),
            (
                "/Users/u/Library/Mobile Documents/iCloud~md~obsidian/Documents",
                found(ICloudDrive, "/Users/u/Library/Mobile Documents"),
            ),
            (
                "/Users/u/Library/CloudStorage/Dropbox/rb",
                found(Dropbox, "/Users/u/Library/CloudStorage/Dropbox"),
            ),
            (
                "/Users/u/Library/CloudStorage/Dropbox-Team/rb",
                found(Dropbox, "/Users/u/Library/CloudStorage/Dropbox-Team"),
            ),
            (
                "/Users/u/Library/CloudStorage/GoogleDrive-u@example.com/My Drive/rb",
                found(
                    GoogleDrive,
                    "/Users/u/Library/CloudStorage/GoogleDrive-u@example.com",
                ),
            ),
            (
                "/Users/u/Library/CloudStorage/OneDrive-Personal/rb",
                found(OneDrive, "/Users/u/Library/CloudStorage/OneDrive-Personal"),
            ),
            (
                "/Users/u/Library/CloudStorage/ProtonDrive-u@proton.me-folder/rb",
                found(
                    ProtonDrive,
                    "/Users/u/Library/CloudStorage/ProtonDrive-u@proton.me-folder",
                ),
            ),
            (
                "/Users/u/Library/CloudStorage/Box-Box/rb",
                found(Box, "/Users/u/Library/CloudStorage/Box-Box"),
            ),
            (
                "/Users/u/Library/CloudStorage/pCloud/rb",
                found(CloudStorage, "/Users/u/Library/CloudStorage/pCloud"),
            ),
            (
                "/Users/u/Library/CloudStorage/Boxcryptor/rb",
                found(CloudStorage, "/Users/u/Library/CloudStorage/Boxcryptor"),
            ),
            ("/Users/u/Dropbox/rb", found(Dropbox, "/Users/u/Dropbox")),
            (
                "/Users/u/Google Drive/rb",
                found(GoogleDrive, "/Users/u/Google Drive"),
            ),
            ("/Users/u/OneDrive", found(OneDrive, "/Users/u/OneDrive")),
            (
                "/Volumes/GoogleDrive/My Drive/rb",
                found(GoogleDrive, "/Volumes/GoogleDrive"),
            ),
            (
                "/Volumes/GoogleDrive-1234/My Drive/rb",
                found(GoogleDrive, "/Volumes/GoogleDrive-1234"),
            ),
            // The default, and the folders that cannot be told from a path.
            ("/Users/u/Library/Application Support/bookmole", None),
            ("/Users/u/Documents/bookmole", None),
            ("/Users/u/Desktop/bookmole", None),
            // The container itself is not a provider's folder.
            ("/Users/u/Library/CloudStorage", None),
            // Someone else's home.
            ("/Users/v/Dropbox/rb", None),
            ("/Volumes/External/rb", None),
        ];
        for (path, want) in cases {
            assert_eq!(&on(path, &MAC_HOME, Platform::MacOs), want, "{path}");
        }
    }

    #[test]
    fn macos_and_windows_compare_without_case_and_unix_does_not() {
        assert_eq!(
            on("/users/U/dropbox/rb", &MAC_HOME, Platform::MacOs),
            found(CloudService::Dropbox, "/users/U/dropbox"),
            "the root is spelled as the path spells it"
        );
        assert_eq!(
            on(
                "/Users/u/library/mobile documents/x",
                &MAC_HOME,
                Platform::MacOs
            )
            .map(|f| f.service),
            Some(CloudService::ICloudDrive)
        );
        assert_eq!(
            on(
                r"c:\users\u\DROPBOX\rb",
                &[("USERPROFILE", r"C:\Users\u")],
                Platform::Windows
            )
            .map(|f| f.service),
            Some(CloudService::Dropbox)
        );
        assert_eq!(
            on("/home/u/dropbox/rb", &[("HOME", "/home/u")], Platform::Unix),
            None
        );
    }

    #[test]
    fn a_sibling_that_only_starts_with_a_root_name_is_not_inside_it() {
        for path in [
            "/Users/u/Dropbox2/rb",
            "/Users/u/Dropbox-old/rb",
            "/Users/u/Library/Mobile Documents2/rb",
            "/Users/u/OneDriveBackup",
            "/Volumes/GoogleDriveBackup/rb",
        ] {
            assert_eq!(on(path, &MAC_HOME, Platform::MacOs), None, "{path}");
        }
    }

    #[test]
    fn dots_are_resolved_before_comparing() {
        assert_eq!(
            on(
                "/Users/u/Documents/../Dropbox/./rb",
                &MAC_HOME,
                Platform::MacOs
            )
            .map(|f| f.service),
            Some(CloudService::Dropbox)
        );
        assert_eq!(
            on("/Users/u/Dropbox/../rb", &MAC_HOME, Platform::MacOs),
            None
        );
    }

    #[test]
    fn windows_roots() {
        use CloudService::*;
        let env = [
            ("USERPROFILE", r"C:\Users\u"),
            ("OneDriveCommercial", r"D:\Work\OneDrive - Org"),
        ];
        let cases: &[(&str, Option<CloudFolder>)] = &[
            (
                r"C:\Users\u\Dropbox\rb",
                found(Dropbox, r"C:\Users\u\Dropbox"),
            ),
            (
                r"C:\Users\u\OneDrive\Documents\rb",
                found(OneDrive, r"C:\Users\u\OneDrive"),
            ),
            (
                r"D:\Work\OneDrive - Org\rb",
                found(OneDrive, r"D:\Work\OneDrive - Org"),
            ),
            (
                r"C:\Users\u\Google Drive\rb",
                found(GoogleDrive, r"C:\Users\u\Google Drive"),
            ),
            (
                r"C:\Users\u\iCloudDrive\rb",
                found(ICloudDrive, r"C:\Users\u\iCloudDrive"),
            ),
            (
                r"C:\Users\u\Proton Drive\rb",
                found(ProtonDrive, r"C:\Users\u\Proton Drive"),
            ),
            (r"G:\My Drive\rb", found(GoogleDrive, r"G:\My Drive")),
            (
                r"G:\Shared drives\Team\rb",
                found(GoogleDrive, r"G:\Shared drives"),
            ),
            // What `canonicalize` hands back on Windows.
            (
                r"\\?\C:\Users\u\Dropbox\rb",
                found(Dropbox, r"C:\Users\u\Dropbox"),
            ),
            (
                "C:/Users/u/Dropbox/rb",
                found(Dropbox, r"C:\Users\u\Dropbox"),
            ),
            (r"C:\Users\u\AppData\Roaming\bookmole", None),
            (r"C:\Users\u\Documents\bookmole", None),
            (r"D:\Work\rb", None),
            (r"G:\Other\rb", None),
            // Not absolute on Windows.
            (r"Users\u\Dropbox\rb", None),
            ("/Users/u/Dropbox/rb", None),
        ];
        for (path, want) in cases {
            assert_eq!(&on(path, &env, Platform::Windows), want, "{path}");
        }
    }

    #[test]
    fn unix_roots_are_few() {
        let env = [("HOME", "/home/u")];
        assert_eq!(
            on("/home/u/Dropbox/rb", &env, Platform::Unix),
            found(CloudService::Dropbox, "/home/u/Dropbox")
        );
        assert_eq!(
            on("/home/u/OneDrive/rb", &env, Platform::Unix),
            found(CloudService::OneDrive, "/home/u/OneDrive")
        );
        for path in [
            "/home/u/.local/share/bookmole",
            "/home/u/Google Drive/rb",
            "/home/u/Library/Mobile Documents/rb",
            "/home/u/Library/CloudStorage/Dropbox/rb",
        ] {
            assert_eq!(on(path, &env, Platform::Unix), None, "{path}");
        }
    }

    #[test]
    fn no_home_means_only_the_rules_that_need_none() {
        assert_eq!(on("/Users/u/Dropbox/rb", &[], Platform::MacOs), None);
        assert_eq!(
            on("/Users/u/Dropbox/rb", &[("HOME", "")], Platform::MacOs),
            None
        );
        assert_eq!(
            on("/Volumes/GoogleDrive/rb", &[], Platform::MacOs).map(|f| f.service),
            Some(CloudService::GoogleDrive)
        );
        assert_eq!(on(r"C:\Users\u\Dropbox", &[], Platform::Windows), None);
    }

    #[test]
    fn a_bad_variable_cannot_make_everything_match() {
        // A OneDrive variable pointing at a whole drive, and a home of `/`.
        assert_eq!(
            on(r"C:\anything", &[("OneDrive", r"C:\")], Platform::Windows),
            None
        );
        assert_eq!(on("/Dropbox/x", &[("HOME", "/")], Platform::MacOs), None);
        assert_eq!(on("/x", &[("HOME", "relative")], Platform::MacOs), None);
    }

    #[test]
    fn a_relative_path_is_no_answer() {
        assert_eq!(on("Dropbox/rb", &MAC_HOME, Platform::MacOs), None);
    }

    #[test]
    fn the_database_dir_comes_from_the_url() {
        assert_eq!(
            database_dir("sqlite:///tmp/rb/database/app.db"),
            Some(PathBuf::from("/tmp/rb/database"))
        );
        assert_eq!(
            database_dir("sqlite:///tmp/rb/database/app.db?mode=rwc"),
            Some(PathBuf::from("/tmp/rb/database"))
        );
        assert_eq!(
            database_dir("sqlite://./database/app.db"),
            Some(PathBuf::from("./database"))
        );
        assert_eq!(database_dir("sqlite://app.db"), Some(PathBuf::from(".")));
        assert_eq!(database_dir("sqlite::memory:"), None);
        assert_eq!(database_dir("sqlite://:memory:"), None);
    }

    fn icloud(home: &str) -> String {
        format!("{home}/Library/Mobile Documents/com~apple~CloudDocs")
    }

    #[test]
    fn the_vault_alone_in_a_synced_folder_is_nothing() {
        let env = env_of(&MAC_HOME);
        let mut config = EngineConfig::rooted_at("/Users/u/Library/Application Support/rb");
        config.vault_dir = PathBuf::from(format!("{}/vault", icloud("/Users/u")));
        let got = LibraryLocation::of(&config.db_url, |p| {
            cloud_folder_from(p, &env, Platform::MacOs)
        });
        assert_eq!(got.cloud_folder, None);
        assert_eq!(
            got.database_dir,
            Some(PathBuf::from(
                "/Users/u/Library/Application Support/rb/database"
            ))
        );
    }

    #[test]
    fn the_library_in_a_synced_folder_is_the_fact() {
        let env = env_of(&MAC_HOME);
        let config = EngineConfig::rooted_at(format!("{}/rb", icloud("/Users/u")));
        let got = LibraryLocation::of(&config.db_url, |p| {
            cloud_folder_from(p, &env, Platform::MacOs)
        });
        assert_eq!(
            got.cloud_folder,
            found(
                CloudService::ICloudDrive,
                "/Users/u/Library/Mobile Documents"
            )
        );
    }

    #[test]
    fn an_in_memory_library_is_in_no_folder() {
        let got = LibraryLocation::of("sqlite::memory:", |_| {
            panic!("nothing to judge for an in-memory database")
        });
        assert_eq!(
            got,
            LibraryLocation {
                database_dir: None,
                cloud_folder: None
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_wrapper_sees_through_a_symlink_into_a_synced_folder() {
        // A temp home whose iCloud folder holds the library, and a link to it
        // from outside the home. The link's own path is nothing; its target
        // is inside iCloud Drive. The real home is never read: `HOME` is this
        // test's own.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let lib = home.join("Library/Mobile Documents/com~apple~CloudDocs/rb");
        std::fs::create_dir_all(&lib).unwrap();
        let link = tmp.path().join("rb-link");
        std::os::unix::fs::symlink(&lib, &link).unwrap();
        let home_s = home.to_string_lossy().into_owned();
        let env = env_of(&[("HOME", home_s.as_str())]);
        let canon = |p: &Path| std::fs::canonicalize(p).ok();

        assert_eq!(cloud_folder_from(&link, &env, Platform::MacOs), None);
        let got = cloud_folder_through(&link, &env, Platform::MacOs, canon);
        assert_eq!(got.map(|f| f.service), Some(CloudService::ICloudDrive));

        // A plain folder stays nothing through both looks, and a path that
        // cannot be canonicalized is the first look's answer, not an error.
        let plain = tmp.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert_eq!(
            cloud_folder_through(&plain, &env, Platform::MacOs, canon),
            None
        );
        assert_eq!(
            cloud_folder_through(&tmp.path().join("absent"), &env, Platform::MacOs, canon),
            None
        );
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        fn name() -> impl Strategy<Value = String> {
            "[A-Za-z][A-Za-z0-9 ._-]{0,10}".prop_filter("not a dot name", |s| {
                s != "." && s != ".." && !s.ends_with(' ')
            })
        }

        /// (platform, env, a root that platform syncs, the service it is).
        fn rooted()
        -> impl Strategy<Value = (Platform, Vec<(&'static str, String)>, String, CloudService)>
        {
            prop_oneof![
                Just((
                    Platform::MacOs,
                    vec![("HOME", "/Users/u".to_string())],
                    "/Users/u/Library/Mobile Documents".to_string(),
                    CloudService::ICloudDrive
                )),
                Just((
                    Platform::MacOs,
                    vec![("HOME", "/Users/u".to_string())],
                    "/Users/u/Library/CloudStorage/OneDrive-Personal".to_string(),
                    CloudService::OneDrive
                )),
                Just((
                    Platform::MacOs,
                    vec![("HOME", "/Users/u".to_string())],
                    "/Users/u/Dropbox".to_string(),
                    CloudService::Dropbox
                )),
                Just((
                    Platform::Unix,
                    vec![("HOME", "/home/u".to_string())],
                    "/home/u/Dropbox".to_string(),
                    CloudService::Dropbox
                )),
                Just((
                    Platform::Windows,
                    vec![("USERPROFILE", r"C:\Users\u".to_string())],
                    r"C:\Users\u\Proton Drive".to_string(),
                    CloudService::ProtonDrive
                )),
                Just((
                    Platform::Windows,
                    vec![
                        ("USERPROFILE", r"C:\Users\u".to_string()),
                        ("OneDrive", r"E:\Sync\OneDrive".to_string())
                    ],
                    r"E:\Sync\OneDrive".to_string(),
                    CloudService::OneDrive
                )),
            ]
        }

        fn sep(p: Platform) -> &'static str {
            if p == Platform::Windows { "\\" } else { "/" }
        }

        fn judge(
            path: &str,
            env: &[(&'static str, String)],
            platform: Platform,
        ) -> Option<CloudFolder> {
            let pairs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
            on(path, &pairs, platform)
        }

        proptest! {
            /// Any path at or under a synced root is inside it, with that
            /// root and that service.
            #[test]
            fn anything_under_a_root_matches(
                (platform, env, root, service) in rooted(),
                tail in proptest::collection::vec(name(), 0..4),
            ) {
                let mut path = root.clone();
                for t in &tail {
                    path.push_str(sep(platform));
                    path.push_str(t);
                }
                let got = judge(&path, &env, platform);
                prop_assert_eq!(got, Some(CloudFolder { service, root: PathBuf::from(&root) }));
            }

            /// A sibling whose last component merely starts with the root's
            /// last component is never inside it.
            #[test]
            fn a_longer_sibling_name_never_matches(
                (platform, env, root, service) in rooted(),
                suffix in "[A-Za-z0-9 ._]{1,6}",
                tail in proptest::collection::vec(name(), 0..3),
            ) {
                // Under `~/Library/CloudStorage` *every* folder is a client's,
                // so a longer name there is another synced folder, correctly.
                prop_assume!(!root.contains("CloudStorage"));
                let _ = service;
                let mut path = format!("{root}{suffix}");
                for t in &tail {
                    path.push_str(sep(platform));
                    path.push_str(t);
                }
                prop_assert_eq!(judge(&path, &env, platform), None, "{}", path);
            }
        }
    }
}
