//! Default library location: `--data-dir`, then `BOOKMOLE_DATA_DIR`, then the platform data
//! directory. No cwd fallback; the result is always absolute, since a relative root yields
//! relative cover paths.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

/// One constant: renaming it means moving users' libraries, and the README and `install.sh` spell
/// it too.
pub const LIBRARY_FOLDER_NAME: &str = "bookmole";

pub const DATA_DIR_ENV: &str = "BOOKMOLE_DATA_DIR";

/// A value, not a `cfg!`, so a test can ask for another host's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Unix,
    Windows,
}

impl Platform {
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

pub fn resolve(explicit: Option<&Path>) -> Result<PathBuf, DataDirError> {
    resolve_from(
        explicit,
        |name| std::env::var_os(name),
        Platform::current(),
        |p| std::path::absolute(p),
    )
}

/// Pure: `absolute` makes a user-given relative path absolute. The platform default is not run
/// through it, or a Windows default asked for on a Unix host would be glued onto a Unix cwd.
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
            // XDG: a relative value is invalid and ignored; empty counts as unset.
            if let Some(xdg) = non_empty(env("XDG_DATA_HOME")).filter(starts_with_slash) {
                return Ok(PathBuf::from(xdg).join(LIBRARY_FOLDER_NAME));
            }
            let home = unix_home(env)?;
            Ok(home.join(".local").join("share").join(LIBRARY_FOLDER_NAME))
        }
        Platform::Windows => {
            let appdata =
                non_empty(env("APPDATA")).ok_or(DataDirError::NoHome { var: "APPDATA" })?;
            // Joined as text with `\`: `Path::join` uses the host's separator, but this branch must give the
            // Windows answer anywhere.
            let mut s = appdata;
            if !s.to_string_lossy().ends_with(['\\', '/']) {
                s.push("\\");
            }
            s.push(LIBRARY_FOLDER_NAME);
            Ok(PathBuf::from(s))
        }
    }
}

/// A relative `HOME` would make the default cwd-relative, so it counts as no home.
fn unix_home(env: &impl Fn(&str) -> Option<OsString>) -> Result<PathBuf, DataDirError> {
    non_empty(env("HOME"))
        .filter(starts_with_slash)
        .map(PathBuf::from)
        .ok_or(DataDirError::NoHome { var: "HOME" })
}

fn non_empty(v: Option<OsString>) -> Option<OsString> {
    v.filter(|s| !s.is_empty())
}

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
