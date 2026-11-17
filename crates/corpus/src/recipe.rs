//! The recipe runner: applies `corpus/recipes/<name>.toml` to a data directory
//! through the real `bookmole` binary (argv, never a shell string), writes
//! `build.json` and the semantic lock, and compares that lock with
//! `corpus/recipes/expect/<name>.json`. The invariant suite that reads the
//! result lives in `crates/cli/tests/recipes.rs`, which compiles **this file**
//! in with `#[path]` — so it uses nothing but `std`, `serde` and `toml`, the
//! three things both crates already link. That is why JSON is written and read
//! by hand below rather than through `serde_json`.
//!
//! The one exception to the crate's rule that it never depends on the engine
//! (a generator that reused the engine's parsing would bake its bugs into the
//! goldens):
//! the runner *runs* the built CLI binary, because the binary is the thing under
//! test. It links nothing from `bookmole`, and it counts what was built by
//! asking the `sqlite3` CLI, never the engine's own queries.
//!
//! `corpus/recipes/README.md` is the format; the semantic lock below and the
//! invariants in `crates/cli/tests/recipes.rs` are the contract.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use serde::Deserialize;

/// Bumped when the shape of `build.json` or of the semantic lock changes.
pub const RUNNER_VERSION: u32 = 1;

pub struct Options {
    pub update_expect: bool,
}

/// A recipe file, as written.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(rename = "step", default)]
    pub steps: Vec<Step>,
    /// Strict expected-failures: hostile cases whose observed behaviour
    /// contradicts their declared class, reported as engine bugs. A listed
    /// case that *stops* diverging fails the build too, so the entry is
    /// removed (and the case promoted) when the fix lands — never left to rot.
    #[serde(rename = "known", default)]
    pub known: Vec<Known>,
    /// Where the recipe was read from; its expectations live beside it, so
    /// the private overlay's never land in the committed tree.
    #[serde(skip)]
    pub source: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Known {
    pub case: String,
    pub why: String,
}

impl Recipe {
    pub fn is_known(&self, case: Option<&str>) -> bool {
        case.is_some_and(|c| self.known.iter().any(|k| k.case == c))
    }

    /// Invocations that broke the recipe: unexpected outcomes on cases not
    /// listed as known, and known cases that no longer diverge.
    pub fn violations<'a>(&self, runs: &'a [Invocation]) -> Vec<(&'a Invocation, &'static str)> {
        runs.iter()
            .filter_map(
                |r| match (r.as_expected(), self.is_known(r.case.as_deref())) {
                    (false, false) => Some((r, "did not do what the recipe declared")),
                    (true, true) => Some((
                        r,
                        "listed under [[known]] but now behaves as declared: promote it",
                    )),
                    _ => None,
                },
            )
            .collect()
    }
}

/// One `[[step]]`. Which path key a kind reads is fixed by [`Step::input`].
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub kind: String,
    /// `epubs`: the directory whose `*.epub` are imported, in name order.
    pub dir: Option<String>,
    /// `epubs`: import only the first N (by file name).
    pub limit: Option<usize>,
    /// `calibre-import`: the calibre library.
    pub library: Option<String>,
    /// `ko-sync`: a reader's root (a directory of `.sdr` sidecars).
    pub root: Option<String>,
    /// `ko-stats`: a reader mount holding `statistics.sqlite3` or a
    /// `statistics.sql` the runner materialises.
    pub mount: Option<String>,
    /// `goodreads-import`: the export CSV.
    pub csv: Option<String>,
    /// `verbs`: a `verbs.jsonl` file.
    pub file: Option<String>,
    /// The step is expected to exit non-zero (a refusal that is the point).
    #[serde(default)]
    pub expect_failure: bool,
    /// Re-applied by the idempotency invariant. Defaults to true for import
    /// kinds; a recipe sets it false only with a `reapply_why`.
    pub reapply: Option<bool>,
    /// Why a step is excluded from re-application — required with `reapply = false`.
    pub reapply_why: Option<String>,
}

pub const KINDS: &[&str] = &[
    "epubs",
    "calibre-import",
    "ko-sync",
    "ko-stats",
    "goodreads-import",
    "verbs",
    "refill",
    "sort-keys",
    "covers",
];

/// The kinds whose second application must change nothing.
const IMPORT_KINDS: &[&str] = &[
    "epubs",
    "calibre-import",
    "ko-sync",
    "ko-stats",
    "goodreads-import",
];

impl Step {
    pub fn reapplies(&self) -> bool {
        self.reapply
            .unwrap_or_else(|| IMPORT_KINDS.contains(&self.kind.as_str()))
    }

    fn input(&self) -> Option<&str> {
        match self.kind.as_str() {
            "epubs" => self.dir.as_deref(),
            "calibre-import" => self.library.as_deref(),
            "ko-sync" => self.root.as_deref(),
            "ko-stats" => self.mount.as_deref(),
            "goodreads-import" => self.csv.as_deref(),
            "verbs" => self.file.as_deref(),
            _ => None,
        }
    }
}

/// One `verbs.jsonl` line, in the shape `gen-reader` documents (`reader.rs`).
#[derive(Debug, Clone, PartialEq)]
pub struct Verb {
    pub argv: Vec<String>,
    pub expect_ok: bool,
    pub case: Option<String>,
    pub why: String,
}

/// One process the runner started.
#[derive(Debug, Clone)]
pub struct Invocation {
    pub step: usize,
    pub kind: String,
    pub argv: Vec<String>,
    pub expect_failure: bool,
    pub case: Option<String>,
    pub exit: Option<i32>,
    pub millis: u128,
    pub stdout: String,
    pub stderr: String,
}

impl Invocation {
    /// Did this process do what the recipe said it would?
    pub fn as_expected(&self) -> bool {
        match self.exit {
            // 101 is a Rust panic: never an acceptable refusal.
            Some(0) => !self.expect_failure,
            Some(101) | None => false,
            Some(_) => self.expect_failure,
        }
    }
}

// ---- locating things ---------------------------------------------------

/// The repository root: recipe paths are relative to it.
pub fn repo_root() -> PathBuf {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    // Both crates that compile this file sit at `crates/<name>`.
    here.join("../..")
        .canonicalize()
        .unwrap_or_else(|_| here.join("../.."))
}

/// A name under `corpus/recipes/` or `personal_data/recipes/`, or a path.
pub fn locate(recipe: &str) -> io::Result<PathBuf> {
    let root = repo_root();
    let direct = PathBuf::from(recipe);
    let candidates = [
        if direct.is_absolute() {
            direct.clone()
        } else {
            root.join(&direct)
        },
        root.join("corpus/recipes").join(format!("{recipe}.toml")),
        root.join("personal_data/recipes")
            .join(format!("{recipe}.toml")),
    ];
    candidates
        .into_iter()
        .find(|p| p.is_file())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("no recipe '{recipe}'")))
}

pub fn load(recipe: &str) -> io::Result<(Recipe, PathBuf)> {
    let path = locate(recipe)?;
    let text = std::fs::read_to_string(&path)?;
    let parsed: Recipe =
        toml::from_str(&text).map_err(|e| invalid(format!("{}: {e}", path.display())))?;
    validate(&parsed)?;
    Ok((parsed, path))
}

pub fn validate(r: &Recipe) -> io::Result<()> {
    for (i, s) in r.steps.iter().enumerate() {
        if !KINDS.contains(&s.kind.as_str()) {
            return Err(invalid(format!(
                "{}: step {i}: unknown kind '{}' (one of {})",
                r.name,
                s.kind,
                KINDS.join(", ")
            )));
        }
        let needs_input = !matches!(s.kind.as_str(), "refill" | "sort-keys" | "covers");
        if needs_input && s.input().is_none() {
            return Err(invalid(format!(
                "{}: step {i} ({}) names no input path",
                r.name, s.kind
            )));
        }
        if s.reapply == Some(false) && s.reapply_why.is_none() {
            return Err(invalid(format!(
                "{}: step {i} sets reapply = false without reapply_why",
                r.name
            )));
        }
    }
    Ok(())
}

/// The missing inputs, if any — a recipe whose layer is not generated.
pub fn missing_inputs(r: &Recipe) -> Vec<String> {
    let root = repo_root();
    r.steps
        .iter()
        .filter_map(Step::input)
        .filter(|p| !root.join(p).exists())
        .map(str::to_owned)
        .collect()
}

/// The programs a step needs the binary to find, beside the binary itself.
///
/// Only `calibre-import` has one: the engine reads a calibre library through
/// calibre's own `calibredb`. Where that is absent the CLI says so and exits 0
/// — an absent optional tool is reported, never a failure — so the step
/// imports nothing, every invocation is "as expected", and the library built
/// is **not the one the recipe's expectations describe**: `everything` comes
/// out 48 books short, with every title and author calibre would have
/// supplied left as the epub spells it. CI's ubuntu runner has no calibre,
/// which is how the semantic lock went red there (2026-11-08) while passing
/// on a Mac that has it.
const STEP_TOOLS: &[(&str, &str)] = &[("calibre-import", "calibredb")];

/// Where the engine looks for calibre after `PATH`. A copy of
/// `bookmole::calibre::KNOWN_DIRS`, not a reference to it: this file links
/// nothing from the engine.
const TOOL_DIRS: &[&str] = &[
    "/Applications/calibre.app/Contents/MacOS",
    "/opt/calibre",
    "/usr/lib/calibre",
];

/// The tools `r`'s steps need that this machine does not have — a recipe
/// that will build, but not into the library its expectations were taken
/// from. Each entry names the tool and the step that needs it.
pub fn missing_tools(r: &Recipe) -> Vec<String> {
    missing_tools_with(r, |tool| find_tool(tool).is_some())
}

fn missing_tools_with(r: &Recipe, found: impl Fn(&str) -> bool) -> Vec<String> {
    STEP_TOOLS
        .iter()
        .filter(|(kind, tool)| r.steps.iter().any(|s| s.kind == *kind) && !found(tool))
        .map(|(kind, tool)| format!("{tool} (for the {kind} step)"))
        .collect()
}

/// `PATH`, then [`TOOL_DIRS`], as the engine searches: an executable file,
/// not merely a path that exists.
fn find_tool(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(TOOL_DIRS.iter().map(PathBuf::from))
        .map(|dir| dir.join(name))
        .find(|p| is_executable(p))
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

/// The workspace's debug `bookmole`, unless told otherwise.
pub fn default_bin() -> PathBuf {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join("target"));
    target.join("debug/bookmole")
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

// ---- running ------------------------------------------------------------

/// Run every step of `recipe` (or only the re-applicable ones) on `data_dir`.
pub fn run(
    recipe: &Recipe,
    data_dir: &Path,
    bin: &Path,
    reapply_only: bool,
) -> io::Result<Vec<Invocation>> {
    std::fs::create_dir_all(data_dir)?;
    let root = repo_root();
    let mut out = Vec::new();
    for (i, step) in recipe.steps.iter().enumerate() {
        if reapply_only && !step.reapplies() {
            continue;
        }
        let input = step.input().map(|p| root.join(p));
        if let Some(p) = &input
            && !p.exists()
        {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "{}: step {i} ({}): {} is not generated — see corpus/recipes/README.md",
                    recipe.name,
                    step.kind,
                    p.display()
                ),
            ));
        }
        for (argv, case, expect_failure) in plan(step, i, input.as_deref(), data_dir)? {
            out.push(exec(
                bin,
                data_dir,
                i,
                &step.kind,
                argv,
                case,
                expect_failure,
            ));
        }
    }
    Ok(out)
}

/// One planned process: argv, the hostile case it carries, whether it must fail.
type Planned = (Vec<String>, Option<String>, bool);

/// The argv lists one step expands to (without the binary and `--data-dir`).
fn plan(
    step: &Step,
    index: usize,
    input: Option<&Path>,
    data_dir: &Path,
) -> io::Result<Vec<Planned>> {
    let s = |p: &Path| p.display().to_string();
    let one = |argv: Vec<String>| vec![(argv, None, step.expect_failure)];
    Ok(match step.kind.as_str() {
        "epubs" => {
            let dir = input.expect("validated");
            let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "epub"))
                .collect();
            files.sort();
            if let Some(n) = step.limit {
                files.truncate(n);
            }
            files
                .iter()
                .map(|f| (vec!["epub".to_owned(), s(f)], None, step.expect_failure))
                .collect()
        }
        "calibre-import" => one(vec![
            "calibre".into(),
            "import".into(),
            "--library".into(),
            s(input.expect("validated")),
        ]),
        "ko-sync" => one(vec![
            "ko".into(),
            "sync".into(),
            s(input.expect("validated")),
            "--all".into(),
        ]),
        "ko-stats" => {
            let mount = materialise_mount(input.expect("validated"), data_dir, index)?;
            one(vec!["ko".into(), "stats".into(), s(&mount)])
        }
        "goodreads-import" => one(vec![
            "goodreads".into(),
            "import".into(),
            s(input.expect("validated")),
        ]),
        "verbs" => read_verbs(input.expect("validated"))?
            .into_iter()
            .map(|v| (v.argv, v.case, !v.expect_ok || step.expect_failure))
            .collect(),
        "refill" => one(vec!["activity".into(), "--refill".into()]),
        "sort-keys" => one(vec!["sort-keys".into()]),
        "covers" => one(vec!["covers".into()]),
        other => return Err(invalid(format!("unknown kind {other}"))),
    })
}

fn exec(
    bin: &Path,
    data_dir: &Path,
    step: usize,
    kind: &str,
    argv: Vec<String>,
    case: Option<String>,
    expect_failure: bool,
) -> Invocation {
    let home = data_dir.join(".home");
    let _ = std::fs::create_dir_all(&home);
    let started = Instant::now();
    // argv, never a shell: `--data-dir` first, the recipe's words after it,
    // user text already behind `--` where the verb file put it.
    let result = Command::new(bin)
        .arg("--data-dir")
        .arg(data_dir)
        .args(&argv)
        // Recipe inputs and verb-file paths are repo-relative, so the child
        // runs from the repo root whatever directory the runner (or `cargo
        // test`, whose cwd is the crate) was started in.
        .current_dir(repo_root())
        .stdin(Stdio::null())
        // The user's config, API keys and editor never reach a corpus build.
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("EDITOR", "true")
        .env("VISUAL", "true")
        .env_remove("BOOKMOLE_DATA_DIR")
        .env_remove("GOOGLE_BOOKS_API_KEY")
        .env_remove("RUST_LOG")
        .output();
    let millis = started.elapsed().as_millis();
    match result {
        Ok(o) => Invocation {
            step,
            kind: kind.to_owned(),
            argv,
            expect_failure,
            case,
            exit: o.status.code(),
            millis,
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        },
        Err(e) => Invocation {
            step,
            kind: kind.to_owned(),
            argv,
            expect_failure,
            case,
            exit: None,
            millis,
            stdout: String::new(),
            stderr: format!("could not start {}: {e}", bin.display()),
        },
    }
}

/// A reader mount for `ko stats`: copied under the data dir, every `X.sql`
/// turned into `X.sqlite3` by the `sqlite3` CLI, and the KOReader install
/// markers `koreader_dir` looks for created if the layer did not ship them.
/// A mount with no `koreader/` directory is read as the settings directory
/// itself (the shape of `tests/fixtures/koreader/statistics/`).
fn materialise_mount(src: &Path, data_dir: &Path, index: usize) -> io::Result<PathBuf> {
    let mount = data_dir.join(".mounts").join(format!("step-{index}"));
    if mount.exists() {
        std::fs::remove_dir_all(&mount)?;
    }
    let ko = mount.join("koreader");
    if src.join("koreader").is_dir() {
        copy_tree(src, &mount)?;
    } else {
        copy_tree(src, &ko.join("settings"))?;
    }
    for d in ["frontend", "plugins", "settings"] {
        std::fs::create_dir_all(ko.join(d))?;
    }
    if !ko.join("reader.lua").exists() {
        std::fs::write(ko.join("reader.lua"), "-- corpus mount marker\n")?;
    }
    let settings = ko.join("settings");
    for e in std::fs::read_dir(&settings)? {
        let p = e?.path();
        if p.extension().is_some_and(|x| x == "sql") {
            let db = p.with_extension("sqlite3");
            let _ = std::fs::remove_file(&db);
            let sql = std::fs::File::open(&p)?;
            let st = Command::new("sqlite3").arg(&db).stdin(sql).status()?;
            if !st.success() {
                return Err(invalid(format!("sqlite3 failed on {}", p.display())));
            }
        }
    }
    Ok(mount)
}

fn copy_tree(src: &Path, dst: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let to = dst.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_tree(&e.path(), &to)?;
        } else {
            std::fs::copy(e.path(), &to)?;
        }
    }
    Ok(())
}

pub fn read_verbs(path: &Path) -> io::Result<Vec<Verb>> {
    let text = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let bad = |m: &str| invalid(format!("{}:{}: {m}", path.display(), n + 1));
        let v = json::parse(line).map_err(|e| bad(&e))?;
        let argv = v
            .get("argv")
            .and_then(json::Value::as_array)
            .ok_or_else(|| bad("no argv array"))?
            .iter()
            .map(|a| {
                a.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| bad("argv holds a non-string"))
            })
            .collect::<io::Result<Vec<_>>>()?;
        let expect_ok = match v.get("expect").and_then(json::Value::as_str) {
            Some("ok") => true,
            Some("fail") => false,
            _ => return Err(bad("expect must be \"ok\" or \"fail\"")),
        };
        out.push(Verb {
            argv,
            expect_ok,
            case: v
                .get("case")
                .and_then(json::Value::as_str)
                .map(str::to_owned),
            why: v
                .get("why")
                .and_then(json::Value::as_str)
                .unwrap_or("")
                .to_owned(),
        });
    }
    Ok(out)
}

/// Counts a re-applied step reported as changed: every `<n> new|updated|
/// changed|created|added` with `n > 0`, as `(n, word)`. A second application
/// of an import must report none. Read by the invariant suite, not by `main`.
#[allow(dead_code)]
pub fn changes_reported(stdout: &str) -> Vec<(u64, String)> {
    let words: Vec<&str> = stdout.split_whitespace().collect();
    words
        .windows(2)
        .filter_map(|w| {
            let n: u64 = w[0]
                .trim_matches(|c: char| !c.is_ascii_digit())
                .parse()
                .ok()?;
            let word = w[1].trim_matches(|c: char| !c.is_alphabetic());
            (n > 0 && ["new", "updated", "changed", "created", "added"].contains(&word))
                .then(|| (n, word.to_owned()))
        })
        .collect()
}

// ---- the semantic lock ----------------------------------------------------

/// Run one query through the `sqlite3` CLI; every column comes back hex so a
/// title with a newline, a tab or a NUL cannot break the parse.
pub fn query(db: &Path, sql: &str) -> io::Result<Vec<Vec<String>>> {
    let o = Command::new("sqlite3")
        .arg("-readonly")
        // Another reader (a `find`, a frontend's dump) may hold the file a moment.
        .arg("-cmd")
        .arg(".timeout 10000")
        .arg("-noheader")
        .arg("-list")
        .arg("-separator")
        .arg("|")
        .arg(db)
        .arg(sql)
        .output()?;
    if !o.status.success() {
        return Err(invalid(format!(
            "sqlite3: {sql}: {}",
            String::from_utf8_lossy(&o.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(|l| l.split('|').map(unhex).collect())
        .collect())
}

fn unhex(s: &str) -> String {
    let bytes: Vec<u8> = (0..s.len() / 2)
        .filter_map(|i| u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok())
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn count(db: &Path, sql: &str) -> io::Result<i64> {
    let rows = query(db, &format!("SELECT hex(({sql}))"))?;
    Ok(rows
        .first()
        .and_then(|r| r.first())
        .and_then(|v| v.parse().ok())
        .unwrap_or(-1))
}

pub fn db_path(data_dir: &Path) -> PathBuf {
    data_dir.join("database/app.db")
}

/// The semantic lock of a built library: what a build must
/// reproduce, and nothing the clock decides. Deterministic text, so the
/// comparison is a string comparison and the diff is a line diff.
pub fn semantic_lock(data_dir: &Path, recipe: &Recipe, runs: &[Invocation]) -> io::Result<String> {
    let db = db_path(data_dir);
    let mut counts = BTreeMap::new();
    for (k, sql) in [
        ("books", "SELECT count(*) FROM books"),
        ("highlights", "SELECT count(*) FROM highlights"),
        (
            "highlights_with_reading",
            "SELECT count(*) FROM highlights WHERE reading_id IS NOT NULL",
        ),
        ("notes", "SELECT count(*) FROM notes"),
        ("notes_fts", "SELECT count(*) FROM notes_fts"),
        ("note_links", "SELECT count(*) FROM note_links"),
        ("flashcards", "SELECT count(*) FROM flashcards"),
        ("readings", "SELECT count(*) FROM readings"),
        (
            "books_with_cover",
            "SELECT count(*) FROM books WHERE cover_path IS NOT NULL",
        ),
        (
            "user_provenance",
            "SELECT count(*) FROM field_provenance WHERE source = 'user'",
        ),
    ] {
        counts.insert(k.to_owned(), count(&db, sql)?);
    }
    let by_status = query(
        &db,
        "SELECT hex(status), hex(count(*)) FROM readings GROUP BY status ORDER BY status",
    )?;
    let titles = query(&db, "SELECT hex(title) FROM books ORDER BY title, id")?;

    let mut s = String::from("{\n");
    let _ = writeln!(s, "  \"runner_version\": {RUNNER_VERSION},");
    let _ = writeln!(s, "  \"recipe\": {},", json::quote(&recipe.name));
    s.push_str("  \"counts\": {\n");
    let body: Vec<String> = counts
        .iter()
        .map(|(k, v)| format!("    {}: {v}", json::quote(k)))
        .collect();
    s.push_str(&body.join(",\n"));
    s.push_str("\n  },\n  \"readings_by_status\": {\n");
    let body: Vec<String> = by_status
        .iter()
        .filter(|r| r.len() == 2)
        .map(|r| format!("    {}: {}", json::quote(&r[0]), r[1]))
        .collect();
    s.push_str(&body.join(",\n"));
    s.push_str("\n  },\n  \"titles\": [\n");
    let body: Vec<String> = titles
        .iter()
        .map(|r| format!("    {}", json::quote(r.first().map_or("", String::as_str))))
        .collect();
    s.push_str(&body.join(",\n"));
    s.push_str("\n  ],\n  \"hostile\": [\n");
    let body: Vec<String> = hostile_outcomes(runs)
        .iter()
        .map(|(case, step, exit, ok)| {
            format!(
                "    {{\"case\": {}, \"step\": {step}, \"exit\": {exit}, \"as_expected\": {ok}, \"known_divergence\": {}}}",
                json::quote(case),
                recipe.is_known(Some(case))
            )
        })
        .collect();
    s.push_str(&body.join(",\n"));
    s.push_str("\n  ]\n}\n");
    Ok(s)
}

/// Per salted case: which step carried it, how the binary exited, and whether
/// that is what the verb line declared.
pub fn hostile_outcomes(runs: &[Invocation]) -> Vec<(String, usize, i64, bool)> {
    runs.iter()
        .filter_map(|r| {
            let case = r.case.clone()?;
            Some((case, r.step, r.exit.map_or(-1, i64::from), r.as_expected()))
        })
        .collect()
}

/// `build.json`: per-invocation exit codes, durations, stdout tails.
pub fn build_report(recipe: &Recipe, runs: &[Invocation]) -> String {
    let mut s = String::from("{\n");
    let _ = writeln!(s, "  \"runner_version\": {RUNNER_VERSION},");
    let _ = writeln!(s, "  \"recipe\": {},", json::quote(&recipe.name));
    let _ = writeln!(s, "  \"violations\": {},", recipe.violations(runs).len());
    s.push_str("  \"runs\": [\n");
    let body: Vec<String> = runs
        .iter()
        .map(|r| {
            let argv: Vec<String> = r.argv.iter().map(|a| json::quote(a)).collect();
            format!(
                "    {{\"step\": {}, \"kind\": {}, \"argv\": [{}], \"exit\": {}, \"expect_failure\": {}, \"as_expected\": {}, \"case\": {}, \"millis\": {}, \"stdout_tail\": {}, \"stderr_tail\": {}}}",
                r.step,
                json::quote(&r.kind),
                argv.join(", "),
                r.exit.map_or("null".to_owned(), |c| c.to_string()),
                r.expect_failure,
                r.as_expected(),
                r.case.as_deref().map_or("null".to_owned(), json::quote),
                r.millis,
                json::quote(&tail(&r.stdout, 8)),
                json::quote(&tail(&r.stderr, 8)),
            )
        })
        .collect();
    s.push_str(&body.join(",\n"));
    s.push_str("\n  ]\n}\n");
    s
}

fn tail(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// `<recipe dir>/expect/<name>.json` — `corpus/recipes/expect/` for the
/// committed recipes, `personal_data/recipes/expect/` for the private overlay.
pub fn expect_path(recipe: &Recipe) -> PathBuf {
    recipe_dir(recipe)
        .join("expect")
        .join(format!("{}.json", recipe.name))
}

/// `<recipe dir>/dump/<name>.txt`, the golden for a frontend's text dump of
/// the built library. Read by the invariant suite, not by `main`.
#[allow(dead_code)]
pub fn dump_path(recipe: &Recipe) -> PathBuf {
    recipe_dir(recipe)
        .join("dump")
        .join(format!("{}.txt", recipe.name))
}

fn recipe_dir(recipe: &Recipe) -> PathBuf {
    recipe
        .source
        .parent()
        .map_or_else(|| repo_root().join("corpus/recipes"), Path::to_path_buf)
}

/// A line diff good enough to read: lines only in `want`, then only in `got`.
pub fn diff(want: &str, got: &str) -> String {
    let w: Vec<&str> = want.lines().collect();
    let g: Vec<&str> = got.lines().collect();
    let mut s = String::new();
    for l in w.iter().filter(|l| !g.contains(l)) {
        let _ = writeln!(s, "- {l}");
    }
    for l in g.iter().filter(|l| !w.contains(l)) {
        let _ = writeln!(s, "+ {l}");
    }
    if s.is_empty() && want != got {
        s.push_str("(same lines, different order or count)\n");
    }
    s
}

/// `build-recipe`: build, write `build.json` + `lock.json`, compare or update.
pub fn build(
    recipe: &str,
    data_dir: &Path,
    bin: Option<&Path>,
    opts: &Options,
) -> io::Result<String> {
    let (recipe, path) = load(recipe)?;
    let missing = missing_inputs(&recipe);
    if !missing.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "{}: layer(s) not generated: {} (`make corpus-layers`)",
                recipe.name,
                missing.join(", ")
            ),
        ));
    }
    let data_dir = std::path::absolute(data_dir)?;
    let bin = bin.map_or_else(default_bin, Path::to_path_buf);
    if !bin.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "{} not built — `cargo build -p bookmole-cli`",
                bin.display()
            ),
        ));
    }
    if db_path(&data_dir).exists() {
        return Err(invalid(format!(
            "{} already holds a library; build-recipe starts from an empty data dir",
            data_dir.display()
        )));
    }
    let runs = run(&recipe, &data_dir, &bin, false)?;
    std::fs::write(data_dir.join("build.json"), build_report(&recipe, &runs))?;
    let lock = semantic_lock(&data_dir, &recipe, &runs)?;
    std::fs::write(data_dir.join("lock.json"), &lock)?;

    let mut report = format!(
        "recipe {} ({}): {}\n  -> {}: {} invocations",
        recipe.name,
        path.display(),
        recipe.description,
        data_dir.display(),
        runs.len()
    );
    let unexpected = recipe.violations(&runs);
    for (r, what) in &unexpected {
        let _ = write!(
            report,
            "\n  UNEXPECTED step {} {} {:?}: exit {:?}: {what}\n    {}",
            r.step,
            r.kind,
            r.argv,
            r.exit,
            tail(&r.stderr, 3).replace('\n', "\n    ")
        );
    }
    for k in &recipe.known {
        let _ = write!(report, "\n  known divergence {}: {}", k.case, k.why);
    }
    let expect = expect_path(&recipe);
    let tools = missing_tools(&recipe);
    if !tools.is_empty() {
        // Neither compared nor written: an expectation recorded here would
        // describe a library the recipe does not build anywhere else.
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "{report}\nnot installed: {} — the library was built without it, so its lock \
                 is not {}'s and is neither compared with nor written to {}",
                tools.join(", "),
                recipe.name,
                expect.display()
            ),
        ));
    }
    if opts.update_expect {
        std::fs::create_dir_all(expect.parent().expect("has a parent"))?;
        std::fs::write(&expect, &lock)?;
        let _ = write!(report, "\nwrote {}", expect.display());
    } else {
        let want = std::fs::read_to_string(&expect).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "{}: {e} (first run? pass --update-expect)",
                    expect.display()
                ),
            )
        })?;
        if want != lock {
            return Err(invalid(format!(
                "{report}\nsemantic lock differs from {}:\n{}",
                expect.display(),
                diff(&want, &lock)
            )));
        }
        let _ = write!(report, "\nsemantic lock matches {}", expect.display());
    }
    if !unexpected.is_empty() {
        return Err(invalid(format!(
            "{report}\n{} invocation(s) broke the recipe's declarations",
            unexpected.len()
        )));
    }
    Ok(report)
}

// ---- JSON, by hand --------------------------------------------------------

/// Just enough JSON for `verbs.jsonl` lines and for writing locks. Hand-rolled
/// because the invariant suite compiles this file into a crate that links no
/// `serde_json`.
pub mod json {
    use std::collections::BTreeMap;

    #[derive(Debug, Clone, PartialEq)]
    pub enum Value {
        Null,
        Bool(bool),
        Num(f64),
        Str(String),
        Arr(Vec<Value>),
        Obj(BTreeMap<String, Value>),
    }

    impl Value {
        pub fn get(&self, k: &str) -> Option<&Value> {
            match self {
                Value::Obj(m) => m.get(k),
                _ => None,
            }
        }
        pub fn as_str(&self) -> Option<&str> {
            match self {
                Value::Str(s) => Some(s),
                _ => None,
            }
        }
        pub fn as_array(&self) -> Option<&Vec<Value>> {
            match self {
                Value::Arr(a) => Some(a),
                _ => None,
            }
        }
    }

    pub fn quote(s: &str) -> String {
        let mut o = String::with_capacity(s.len() + 2);
        o.push('"');
        for c in s.chars() {
            match c {
                '"' => o.push_str("\\\""),
                '\\' => o.push_str("\\\\"),
                '\n' => o.push_str("\\n"),
                '\r' => o.push_str("\\r"),
                '\t' => o.push_str("\\t"),
                c if (c as u32) < 0x20 || c == '\u{7f}' => {
                    o.push_str(&format!("\\u{:04x}", c as u32));
                }
                c => o.push(c),
            }
        }
        o.push('"');
        o
    }

    pub fn parse(s: &str) -> Result<Value, String> {
        let mut p = P {
            b: s.as_bytes(),
            i: 0,
            s,
        };
        let v = p.value()?;
        p.ws();
        if p.i != p.b.len() {
            return Err(format!("trailing input at byte {}", p.i));
        }
        Ok(v)
    }

    struct P<'a> {
        b: &'a [u8],
        i: usize,
        s: &'a str,
    }

    impl P<'_> {
        fn ws(&mut self) {
            while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
                self.i += 1;
            }
        }
        fn eat(&mut self, c: u8) -> Result<(), String> {
            self.ws();
            if self.b.get(self.i) == Some(&c) {
                self.i += 1;
                Ok(())
            } else {
                Err(format!("expected '{}' at byte {}", c as char, self.i))
            }
        }
        fn value(&mut self) -> Result<Value, String> {
            self.ws();
            match self.b.get(self.i) {
                Some(b'{') => {
                    self.i += 1;
                    let mut m = BTreeMap::new();
                    self.ws();
                    if self.b.get(self.i) == Some(&b'}') {
                        self.i += 1;
                        return Ok(Value::Obj(m));
                    }
                    loop {
                        self.ws();
                        let k = self.string()?;
                        self.eat(b':')?;
                        let v = self.value()?;
                        m.insert(k, v);
                        self.ws();
                        match self.b.get(self.i) {
                            Some(b',') => self.i += 1,
                            Some(b'}') => {
                                self.i += 1;
                                return Ok(Value::Obj(m));
                            }
                            _ => return Err(format!("bad object at byte {}", self.i)),
                        }
                    }
                }
                Some(b'[') => {
                    self.i += 1;
                    let mut a = Vec::new();
                    self.ws();
                    if self.b.get(self.i) == Some(&b']') {
                        self.i += 1;
                        return Ok(Value::Arr(a));
                    }
                    loop {
                        a.push(self.value()?);
                        self.ws();
                        match self.b.get(self.i) {
                            Some(b',') => self.i += 1,
                            Some(b']') => {
                                self.i += 1;
                                return Ok(Value::Arr(a));
                            }
                            _ => return Err(format!("bad array at byte {}", self.i)),
                        }
                    }
                }
                Some(b'"') => Ok(Value::Str(self.string()?)),
                Some(b't') => self.word("true", Value::Bool(true)),
                Some(b'f') => self.word("false", Value::Bool(false)),
                Some(b'n') => self.word("null", Value::Null),
                Some(_) => {
                    let start = self.i;
                    while self.i < self.b.len()
                        && matches!(
                            self.b[self.i],
                            b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'
                        )
                    {
                        self.i += 1;
                    }
                    self.s[start..self.i]
                        .parse()
                        .map(Value::Num)
                        .map_err(|_| format!("bad number at byte {start}"))
                }
                None => Err("unexpected end".into()),
            }
        }
        fn word(&mut self, w: &str, v: Value) -> Result<Value, String> {
            if self.s[self.i..].starts_with(w) {
                self.i += w.len();
                Ok(v)
            } else {
                Err(format!("bad literal at byte {}", self.i))
            }
        }
        fn hex4(&mut self) -> Result<u32, String> {
            let h = self.s.get(self.i..self.i + 4).ok_or("short \\u escape")?;
            self.i += 4;
            u32::from_str_radix(h, 16).map_err(|_| "bad \\u escape".to_owned())
        }
        fn string(&mut self) -> Result<String, String> {
            if self.b.get(self.i) != Some(&b'"') {
                return Err(format!("expected string at byte {}", self.i));
            }
            self.i += 1;
            let mut o = String::new();
            loop {
                let rest = &self.s[self.i..];
                let c = rest.chars().next().ok_or("unterminated string")?;
                self.i += c.len_utf8();
                match c {
                    '"' => return Ok(o),
                    '\\' => {
                        let e = self.b.get(self.i).copied().ok_or("bad escape")?;
                        self.i += 1;
                        match e {
                            b'"' => o.push('"'),
                            b'\\' => o.push('\\'),
                            b'/' => o.push('/'),
                            b'b' => o.push('\u{8}'),
                            b'f' => o.push('\u{c}'),
                            b'n' => o.push('\n'),
                            b'r' => o.push('\r'),
                            b't' => o.push('\t'),
                            b'u' => {
                                let hi = self.hex4()?;
                                let cp = if (0xD800..0xDC00).contains(&hi)
                                    && self.s[self.i..].starts_with("\\u")
                                {
                                    self.i += 2;
                                    let lo = self.hex4()?;
                                    0x10000
                                        + ((hi - 0xD800) << 10)
                                        + (lo.wrapping_sub(0xDC00) & 0x3FF)
                                } else {
                                    hi
                                };
                                o.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                            }
                            _ => return Err("bad escape".into()),
                        }
                    }
                    c => o.push(c),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The corpus's salting rule, held for every committed verbs file: at most
    /// one hostile value per row (one `case` field, by construction) and at
    /// most 5% of rows salted.
    #[test]
    fn committed_verbs_files_salt_at_most_five_percent() {
        let dir = repo_root().join("corpus/recipes/fixtures");
        let mut seen = 0;
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let p = e.path();
            if p.extension().is_none_or(|x| x != "jsonl") {
                continue;
            }
            let verbs = read_verbs(&p).unwrap();
            let salted = verbs.iter().filter(|v| v.case.is_some()).count();
            assert!(
                salted * 20 <= verbs.len(),
                "{}: {salted} of {} rows salted — over the 5% salting cap",
                p.display(),
                verbs.len()
            );
            seen += 1;
        }
        assert!(seen > 0, "no verbs fixture under {}", dir.display());
    }

    /// The shape that went red on CI's ubuntu runner (2026-11-08): a recipe
    /// with a `calibre-import` step on a machine with no `calibredb` builds,
    /// exits 0 everywhere, and is not the library its goldens describe. It
    /// must be reported as a missing tool, and only when that step is there.
    #[test]
    fn a_calibre_step_without_calibredb_is_a_missing_tool() {
        let parse = |s: &str| -> Recipe { toml::from_str(s).unwrap() };
        let calibre = parse(
            "name = \"c\"\n[[step]]\nkind = \"calibre-import\"\nlibrary = \"lib\"\n\
             [[step]]\nkind = \"refill\"\n",
        );
        let none = |_: &str| false;
        assert_eq!(
            missing_tools_with(&calibre, none),
            vec!["calibredb (for the calibre-import step)".to_owned()]
        );
        assert!(missing_tools_with(&calibre, |t| t == "calibredb").is_empty());
        let plain = parse("name = \"p\"\n[[step]]\nkind = \"refill\"\n");
        assert!(missing_tools_with(&plain, none).is_empty());

        // The committed recipe whose two goldens were the ones that failed.
        let (everything, _) = load("everything").unwrap();
        assert_eq!(missing_tools_with(&everything, none).len(), 1);
    }

    /// A directory called `calibredb` on `PATH` is not calibre — the engine's
    /// rule, kept here so the two cannot disagree about "absent".
    #[test]
    fn a_directory_is_not_a_tool() {
        let dir = std::env::temp_dir().join(format!("rb-tool-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("calibredb")).unwrap();
        assert!(!is_executable(&dir.join("calibredb")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn json_round_trips_hostile_strings() {
        for s in [
            "plain",
            "quote \" and \\ back",
            "nul\u{0}ctl\u{7}\u{1b}[31m",
            "crlf\r\nlone\r",
            "emoji 👨‍👩‍👧‍👦 bidi \u{202e}x",
            "--data-dir=/tmp/x",
        ] {
            let q = json::quote(s);
            assert_eq!(
                json::parse(&q).unwrap(),
                json::Value::Str(s.to_owned()),
                "{s:?}"
            );
        }
        assert_eq!(
            json::parse("\"\\ud83d\\ude00\"").unwrap(),
            json::Value::Str("😀".into())
        );
    }

    #[test]
    fn verb_lines_parse_in_lane_c_shape() {
        let dir = std::env::temp_dir().join(format!("rb-verbs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("v.jsonl");
        std::fs::write(
            &f,
            "{\"argv\": [\"note\", \"--\", \"x\"], \"expect\": \"ok\", \"case\": null, \"why\": \"w\"}\n\n\
             {\"argv\": [\"progress\", \"Alice\", \"-3\"], \"expect\": \"fail\", \"case\": \"page-negative\", \"why\": \"\"}\n",
        )
        .unwrap();
        let v = read_verbs(&f).unwrap();
        assert_eq!(v.len(), 2);
        assert!(v[0].expect_ok && v[0].case.is_none());
        assert_eq!(v[1].case.as_deref(), Some("page-negative"));
        assert!(!v[1].expect_ok);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn change_counts_are_found_in_import_output() {
        let out = "Alice: 40 new, 0 updated from the device, 0 already known\n\
                   40 rows read, 0 books created, 36 matched\n  0 new, 3 changed";
        assert_eq!(
            changes_reported(out),
            vec![(40, "new".to_owned()), (3, "changed".to_owned())]
        );
        assert!(changes_reported("nothing to sync — everything is already here.").is_empty());
    }

    #[test]
    fn a_panic_is_never_an_expected_failure() {
        let mut r = Invocation {
            step: 0,
            kind: "verbs".into(),
            argv: vec![],
            expect_failure: true,
            case: None,
            exit: Some(101),
            millis: 0,
            stdout: String::new(),
            stderr: String::new(),
        };
        assert!(!r.as_expected());
        r.exit = Some(2);
        assert!(r.as_expected());
        r.exit = Some(0);
        assert!(!r.as_expected());
    }

    #[test]
    fn every_committed_recipe_parses_and_validates() {
        let dir = repo_root().join("corpus/recipes");
        let mut n = 0;
        for e in std::fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            if p.extension().is_some_and(|x| x == "toml") {
                let name = p.file_stem().unwrap().to_str().unwrap().to_owned();
                let (r, _) = load(&name).unwrap_or_else(|e| panic!("{name}: {e}"));
                assert_eq!(r.name, name, "a recipe's name is its file name");
                n += 1;
            }
        }
        assert!(n >= 6, "the six recipes of the plan, found {n}");
    }
}
