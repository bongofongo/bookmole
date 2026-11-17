//! Where the library lives when nobody says.
//!
//! Every frontend used to resolve its data root as `--data-dir`, then
//! `BOOKMOLE_DATA_DIR`, then **the current directory** — so every
//! directory a binary was launched from was a separate library, and a macOS
//! `.app` started from Finder (whose cwd is `/`) could not open one at all.
//! This module is the one answer every frontend shares:
//!
//! 1. an explicit path (the `--data-dir` flag),
//! 2. `BOOKMOLE_DATA_DIR`, when it is set and not empty,
//! 3. the platform's per-user data directory joined with
//!    [`LIBRARY_FOLDER_NAME`]: `$HOME/Library/Application Support/<name>` on
//!    macOS; `$XDG_DATA_HOME/<name>` on Linux and other Unix when that is set,
//!    absolute and not empty, else `$HOME/.local/share/<name>`;
//!    `%APPDATA%\<name>` on Windows.
//!
//! There is **no fourth step**. When no home can be found the answer is
//! [`DataDirError::NoHome`], never a quiet fall back to the cwd — that fall
//! back is the bug this module exists to remove.
//!
//! The result is always absolute. A relative flag or variable is made absolute
//! against the cwd at the moment it is resolved, because `cover_path` is stored
//! as `images_dir.join(name)`: a relative root yields relative cover paths, and
//! a webview-based frontend has no working directory to resolve one against.
//!
//! [`resolve_from`] is the whole rule as a pure function — the environment, the
//! platform and the absolutizer are all arguments — so the tests cover macOS,
//! Linux and Windows on any host without touching the real environment or the
//! real home. [`resolve`] is the thin wrapper a `main` calls.
//!
//! Creating the directory is not this module's job: [`crate::Engine::open`]
//! already `create_dir_all`s everything under the root, which creates the root.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

/// The folder the library lives in under the platform data directory.
///
/// The same name on every platform, so every frontend opens one folder and a
/// terminal user types it without quoting. Once people have
/// libraries here, renaming it means moving them, so it is one constant: no
/// code may spell the folder name out, and the files that do (the README and
/// `install.sh`) are the list to update with it.
pub const LIBRARY_FOLDER_NAME: &str = "bookmole";

/// The environment variable that overrides the default location.
pub const DATA_DIR_ENV: &str = "BOOKMOLE_DATA_DIR";

/// Which platform's convention the default follows. A value, not a `cfg!`, so
/// a test on one host can ask for another host's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    /// Linux and every other Unix: the XDG base-directory convention.
    Unix,
    Windows,
}

impl Platform {
    /// The platform this binary was built for.
    pub fn current() -> Platform {
        if cfg!(target_os = "macos") {
            Platform::MacOs
        } else if cfg!(windows) {
            Platform::Windows
        } else {
            Platform::Unix
        }
    }
}

/// Why there is no library location.
#[derive(Debug, thiserror::Error)]
pub enum DataDirError {
    /// The platform default needs a home and the environment names none.
    #[error(
        "there is no default library location: {var} is not set. \
         Pass --data-dir or set BOOKMOLE_DATA_DIR."
    )]
    NoHome { var: &'static str },
    /// A relative `--data-dir` or `BOOKMOLE_DATA_DIR` could not be made
    /// absolute — in practice, the current directory has been deleted.
    #[error("could not make the library location {} absolute: {source}", .path.display())]
    NotAbsolute {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// The library location for this process: the real environment, the real
/// platform, the real cwd. `explicit` is the `--data-dir` flag, if the
/// frontend has one.
pub fn resolve(explicit: Option<&Path>) -> Result<PathBuf, DataDirError> {
    resolve_from(
        explicit,
        |name| std::env::var_os(name),
        Platform::current(),
        |p| std::path::absolute(p),
    )
}

/// The rule itself, with everything it reads passed in.
///
/// - `env` looks up an environment variable (`None` when unset).
/// - `platform` picks the default's convention.
/// - `absolute` makes a relative **user-given** path absolute — the real
///   wrapper passes [`std::path::absolute`]. The platform default is not run
///   through it: it is built from variables that are absolute on their own
///   platform by construction (`HOME` and `XDG_DATA_HOME` are checked for a
///   leading `/`), and a Windows default asked for on a Unix host would
///   otherwise be glued onto a Unix cwd.
pub fn resolve_from(
    explicit: Option<&Path>,
    env: impl Fn(&str) -> Option<OsString>,
    platform: Platform,
    absolute: impl Fn(&Path) -> io::Result<PathBuf>,
) -> Result<PathBuf, DataDirError> {
    let given = explicit
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .or_else(|| non_empty(env(DATA_DIR_ENV)).map(PathBuf::from));
    if let Some(path) = given {
        if path.is_absolute() {
            return Ok(path);
        }
        return absolute(&path).map_err(|source| DataDirError::NotAbsolute { path, source });
    }
    platform_default(&env, platform)
}

/// Step 3 alone: the per-user data directory joined with the folder name.
fn platform_default(
    env: &impl Fn(&str) -> Option<OsString>,
    platform: Platform,
) -> Result<PathBuf, DataDirError> {
    match platform {
        Platform::MacOs => {
            let home = unix_home(env)?;
            Ok(home
                .join("Library")
                .join("Application Support")
                .join(LIBRARY_FOLDER_NAME))
        }
        Platform::Unix => {
            // The XDG spec: a relative value is invalid and must be ignored,
            // and an empty one counts as unset.
            if let Some(xdg) = non_empty(env("XDG_DATA_HOME")).filter(starts_with_slash) {
                return Ok(PathBuf::from(xdg).join(LIBRARY_FOLDER_NAME));
            }
            let home = unix_home(env)?;
            Ok(home.join(".local").join("share").join(LIBRARY_FOLDER_NAME))
        }
        Platform::Windows => {
            let appdata =
                non_empty(env("APPDATA")).ok_or(DataDirError::NoHome { var: "APPDATA" })?;
            // Joined as text with `\`: `Path::join` uses the *host's*
            // separator, and this branch must give the Windows answer on any
            // host. On Windows itself the two are the same path.
            let mut s = appdata;
            if !s.to_string_lossy().ends_with(['\\', '/']) {
                s.push("\\");
            }
            s.push(LIBRARY_FOLDER_NAME);
            Ok(PathBuf::from(s))
        }
    }
}

/// `$HOME`, when it is set, not empty and absolute. A relative `HOME` would
/// make the default relative to the cwd — the old bug by another door — so it
/// counts as no home at all.
fn unix_home(env: &impl Fn(&str) -> Option<OsString>) -> Result<PathBuf, DataDirError> {
    non_empty(env("HOME"))
        .filter(starts_with_slash)
        .map(PathBuf::from)
        .ok_or(DataDirError::NoHome { var: "HOME" })
}

fn non_empty(v: Option<OsString>) -> Option<OsString> {
    v.filter(|s| !s.is_empty())
}

/// Absolute in the Unix sense, judged on the text so the answer does not
/// depend on the host the test runs on.
fn starts_with_slash(v: &OsString) -> bool {
    v.to_string_lossy().starts_with('/')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const CWD: &str = "/work/here";

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let map: HashMap<String, OsString> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), OsString::from(v)))
            .collect();
        move |name| map.get(name).cloned()
    }

    /// The real absolutizer's behaviour against a fixed cwd, so no test reads
    /// the process's own.
    fn fake_abs(p: &Path) -> io::Result<PathBuf> {
        Ok(Path::new(CWD).join(p))
    }

    fn resolve_on(
        explicit: Option<&str>,
        pairs: &[(&str, &str)],
        platform: Platform,
    ) -> Result<PathBuf, DataDirError> {
        resolve_from(explicit.map(Path::new), env_of(pairs), platform, fake_abs)
    }

    const ALL: [Platform; 3] = [Platform::MacOs, Platform::Unix, Platform::Windows];

    #[test]
    fn the_flag_beats_the_variable_beats_the_default() {
        let env = [
            (DATA_DIR_ENV, "/from/env"),
            ("HOME", "/home/u"),
            ("APPDATA", r"C:\Users\u\AppData\Roaming"),
        ];
        for p in ALL {
            assert_eq!(
                resolve_on(Some("/from/flag"), &env, p).unwrap(),
                PathBuf::from("/from/flag"),
                "{p:?}"
            );
            assert_eq!(
                resolve_on(None, &env, p).unwrap(),
                PathBuf::from("/from/env"),
                "{p:?}"
            );
        }
    }

    #[test]
    fn an_empty_variable_counts_as_unset() {
        let env = [(DATA_DIR_ENV, ""), ("HOME", "/Users/u")];
        assert_eq!(
            resolve_on(None, &env, Platform::MacOs).unwrap(),
            PathBuf::from("/Users/u/Library/Application Support/bookmole")
        );
    }

    #[test]
    fn an_empty_flag_counts_as_absent() {
        let env = [(DATA_DIR_ENV, "/from/env")];
        assert_eq!(
            resolve_on(Some(""), &env, Platform::Unix).unwrap(),
            PathBuf::from("/from/env")
        );
    }

    #[test]
    fn macos_defaults_to_application_support() {
        let env = [("HOME", "/Users/u"), ("XDG_DATA_HOME", "/xdg")];
        assert_eq!(
            resolve_on(None, &env, Platform::MacOs).unwrap(),
            PathBuf::from("/Users/u/Library/Application Support/bookmole"),
            "macOS ignores XDG_DATA_HOME: it is not the platform's convention"
        );
    }

    #[test]
    fn unix_defaults_to_xdg_data_home_when_it_is_usable() {
        let env = [("HOME", "/home/u"), ("XDG_DATA_HOME", "/data/xdg")];
        assert_eq!(
            resolve_on(None, &env, Platform::Unix).unwrap(),
            PathBuf::from("/data/xdg/bookmole")
        );
    }

    #[test]
    fn unix_falls_back_to_local_share() {
        let env = [("HOME", "/home/u")];
        assert_eq!(
            resolve_on(None, &env, Platform::Unix).unwrap(),
            PathBuf::from("/home/u/.local/share/bookmole")
        );
    }

    #[test]
    fn a_relative_or_empty_xdg_data_home_is_ignored() {
        for xdg in ["", "relative/xdg", "./xdg"] {
            let env = [("HOME", "/home/u"), ("XDG_DATA_HOME", xdg)];
            assert_eq!(
                resolve_on(None, &env, Platform::Unix).unwrap(),
                PathBuf::from("/home/u/.local/share/bookmole"),
                "XDG_DATA_HOME={xdg:?}"
            );
        }
    }

    #[test]
    fn windows_defaults_to_appdata() {
        let env = [("APPDATA", r"C:\Users\u\AppData\Roaming"), ("HOME", "/h")];
        assert_eq!(
            resolve_on(None, &env, Platform::Windows).unwrap(),
            PathBuf::from(r"C:\Users\u\AppData\Roaming\bookmole")
        );
        let env = [("APPDATA", r"C:\Roaming\")];
        assert_eq!(
            resolve_on(None, &env, Platform::Windows).unwrap(),
            PathBuf::from(r"C:\Roaming\bookmole"),
            "no doubled separator"
        );
    }

    #[test]
    fn no_home_is_a_typed_error_and_never_the_cwd() {
        for (platform, env, var) in [
            (Platform::MacOs, vec![], "HOME"),
            (Platform::MacOs, vec![("HOME", "")], "HOME"),
            (Platform::MacOs, vec![("HOME", "relative")], "HOME"),
            (Platform::Unix, vec![("XDG_DATA_HOME", "rel")], "HOME"),
            (Platform::Unix, vec![("HOME", "")], "HOME"),
            (Platform::Windows, vec![("HOME", "/home/u")], "APPDATA"),
            (Platform::Windows, vec![("APPDATA", "")], "APPDATA"),
        ] {
            match resolve_on(None, &env, platform) {
                Err(DataDirError::NoHome { var: got }) => assert_eq!(got, var, "{platform:?}"),
                other => panic!("{platform:?} {env:?}: expected NoHome, got {other:?}"),
            }
        }
    }

    #[test]
    fn the_no_home_message_says_how_to_choose_one() {
        let e = resolve_on(None, &[], Platform::MacOs).unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("HOME"), "{msg}");
        assert!(msg.contains("--data-dir"), "{msg}");
        assert!(msg.contains(DATA_DIR_ENV), "{msg}");
    }

    #[test]
    fn relative_inputs_come_back_absolute() {
        assert_eq!(
            resolve_on(Some("lib"), &[], Platform::MacOs).unwrap(),
            PathBuf::from("/work/here/lib")
        );
        assert_eq!(
            resolve_on(None, &[(DATA_DIR_ENV, "./dev-data")], Platform::Unix).unwrap(),
            PathBuf::from("/work/here/./dev-data")
        );
    }

    #[test]
    fn an_absolutizer_failure_is_reported_with_the_path() {
        let e = resolve_from(Some(Path::new("lib")), env_of(&[]), Platform::Unix, |_| {
            Err(io::Error::new(io::ErrorKind::NotFound, "cwd is gone"))
        })
        .unwrap_err();
        assert!(
            matches!(&e, DataDirError::NotAbsolute { path, .. } if path == Path::new("lib")),
            "{e:?}"
        );
    }

    #[test]
    fn the_real_wrapper_gives_an_absolute_path_for_an_explicit_one() {
        // Explicit, so neither the real environment nor the real home is read.
        let got = resolve(Some(Path::new("some/relative/lib"))).unwrap();
        assert!(got.is_absolute(), "{}", got.display());
        assert!(got.ends_with("some/relative/lib"));
    }

    #[test]
    fn the_folder_name_is_spelled_once() {
        assert_eq!(LIBRARY_FOLDER_NAME, "bookmole");
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        fn platform() -> impl Strategy<Value = Platform> {
            prop_oneof![
                Just(Platform::MacOs),
                Just(Platform::Unix),
                Just(Platform::Windows)
            ]
        }

        /// Values an environment variable might hold, empty and relative
        /// included.
        fn value() -> impl Strategy<Value = Option<String>> {
            proptest::option::of(prop_oneof![
                Just(String::new()),
                "[a-z]{1,8}(/[a-z]{1,8}){0,2}",
                "/[a-z]{1,8}(/[a-z]{1,8}){0,2}",
            ])
        }

        fn env_from(
            data: Option<String>,
            home: Option<String>,
            xdg: Option<String>,
            appdata: Option<String>,
        ) -> Vec<(&'static str, String)> {
            [
                (DATA_DIR_ENV, data),
                ("HOME", home),
                ("XDG_DATA_HOME", xdg),
                ("APPDATA", appdata),
            ]
            .into_iter()
            .filter_map(|(k, v)| v.map(|v| (k, v)))
            .collect()
        }

        fn run(
            explicit: Option<&str>,
            env: &[(&'static str, String)],
            platform: Platform,
        ) -> Result<PathBuf, DataDirError> {
            let pairs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
            resolve_on(explicit, &pairs, platform)
        }

        proptest! {
            /// A non-empty `--data-dir` wins whatever the environment holds.
            #[test]
            fn an_explicit_path_always_wins(
                flag in "/?[a-z]{1,8}(/[a-z]{1,8}){0,2}",
                data in value(), home in value(), xdg in value(), appdata in value(),
                platform in platform(),
            ) {
                let env = env_from(data, home, xdg, appdata);
                let got = run(Some(&flag), &env, platform).unwrap();
                prop_assert_eq!(got, fake_abs(Path::new(&flag)).unwrap());
            }

            /// On macOS and Unix, whatever the environment, the answer is an
            /// absolute path or the typed error — never something relative to
            /// wherever the binary was started.
            #[test]
            fn a_unix_answer_is_absolute_or_an_error(
                data in value(), home in value(), xdg in value(),
                platform in prop_oneof![Just(Platform::MacOs), Just(Platform::Unix)],
            ) {
                let env = env_from(data, home, xdg, None);
                match run(None, &env, platform) {
                    Ok(p) => prop_assert!(p.to_string_lossy().starts_with('/'), "{}", p.display()),
                    Err(DataDirError::NoHome { .. }) => {}
                    Err(e) => prop_assert!(false, "unexpected {e:?}"),
                }
            }
        }
    }
}
