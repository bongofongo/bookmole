//! The recipe invariant suite: one test per invariant a built recipe must
//! satisfy, against a library built through the real binary (recipes and their
//! format are described in `corpus/recipes/README.md`).
//!
//! The recipe runner itself is compiled in from `crates/corpus/src/recipe.rs`
//! (`#[path]` below), so the suite and `cargo run -p corpus -- build-recipe`
//! are one implementation and cannot drift. That file links only `std`,
//! `serde` and `toml`, all of which this crate already has.
//!
//! The library is built **once** per test binary (a `OnceLock`), and the
//! re-application for the idempotency invariant happens inside that same
//! initialiser, so no test ever reads a library another test is mutating.
//! Under nextest every test is its own process and a `OnceLock` is per
//! process, so the built library and what building it produced are also
//! **kept on disk between processes** (`cache_dir`), keyed on the binary,
//! the recipe file and every input layer: the first test builds, the rest
//! read. Before that, CI's ubuntu runner built the recipe eleven times in a
//! row at 60–100 s each (the `calibredb` group runs one at a time), which
//! overran the job's 30 minutes on 2026-11-09.
//!
//! Which recipe: `BOOKMOLE_RECIPE`, default `everything`. Every test skips
//! loudly with `SKIPPED:` when the recipe's layers are not generated, and fails
//! instead under `BOOKMOLE_REQUIRE_FIXTURES=1` (the engine standards'
//! rule).

#[path = "../../corpus/src/recipe.rs"]
#[allow(dead_code)]
mod recipe;
mod sandbox;

use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use recipe::{Invocation, Recipe, json};
use sandbox::BIN;
use serde::{Deserialize, Serialize};

struct Built {
    recipe: Recipe,
    dir: PathBuf,
    runs: Vec<Invocation>,
    lock_first: String,
    lock_second: String,
    reapplied: Vec<Invocation>,
    inputs_before: BTreeMap<String, (u64, u128)>,
    inputs_after: BTreeMap<String, (u64, u128)>,
}

fn require() -> bool {
    std::env::var_os("BOOKMOLE_REQUIRE_FIXTURES").is_some_and(|v| v == "1")
}

fn recipe_name() -> String {
    std::env::var("BOOKMOLE_RECIPE").unwrap_or_else(|_| "everything".to_owned())
}

/// The shared build, or why there is none.
fn built() -> Option<&'static Built> {
    static BUILT: OnceLock<Result<Built, String>> = OnceLock::new();
    match BUILT.get_or_init(build) {
        Ok(b) => Some(b),
        Err(why) => {
            assert!(
                !require(),
                "BOOKMOLE_REQUIRE_FIXTURES=1 and the recipe cannot be built: {why}"
            );
            eprintln!("SKIPPED: recipe invariants: {why}");
            None
        }
    }
}

fn build() -> Result<Built, String> {
    let name = recipe_name();
    if !recipe::repo_root().join("corpus/generated").is_dir() {
        return Err("corpus/generated is absent — `make corpus-layers`".into());
    }
    let (recipe, recipe_path) = recipe::load(&name).map_err(|e| e.to_string())?;
    let missing = recipe::missing_inputs(&recipe);
    if !missing.is_empty() {
        return Err(format!("{name}: not generated: {}", missing.join(", ")));
    }
    let inputs_before = digest_inputs(&recipe);
    let cache = cache_dir(&name, &recipe_path, &inputs_before);
    let dir = cache.join("data");
    if let Some(b) = Manifest::read(&cache, &recipe, &dir) {
        return Ok(b);
    }
    // Another key's library is a build this machine's state no longer
    // describes; one tree per recipe is enough to keep.
    if let Some(parent) = cache.parent()
        && let Ok(rd) = std::fs::read_dir(parent)
    {
        for e in rd.flatten() {
            let n = e.file_name();
            if n.to_string_lossy().starts_with(&format!("{name}-")) && e.path() != cache {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    let building = cache.with_extension(format!("building-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&building);
    let dir_building = building.join("data");
    let bin = Path::new(BIN);
    let runs = recipe::run(&recipe, &dir_building, bin, false).map_err(|e| e.to_string())?;
    let lock_first =
        recipe::semantic_lock(&dir_building, &recipe, &runs).map_err(|e| e.to_string())?;
    let reapplied = recipe::run(&recipe, &dir_building, bin, true).map_err(|e| e.to_string())?;
    // The second lock is computed against the *first* run's invocations, so
    // its hostile list is the same by construction and only the counts and
    // titles — what the re-application could have changed — are compared.
    let lock_second =
        recipe::semantic_lock(&dir_building, &recipe, &runs).map_err(|e| e.to_string())?;
    let inputs_after = digest_inputs(&recipe);
    let manifest = Manifest {
        runs: runs.iter().map(Inv::from).collect(),
        lock_first,
        lock_second,
        reapplied: reapplied.iter().map(Inv::from).collect(),
        inputs_before: Input::list(&inputs_before),
        inputs_after: Input::list(&inputs_after),
    };
    let text = toml::to_string(&manifest).map_err(|e| format!("manifest: {e}"))?;
    std::fs::write(building.join("built.toml"), text).map_err(|e| e.to_string())?;
    // Whole or absent: the manifest is written before the rename, and a
    // process that lost the race to the rename reads the winner's tree.
    if std::fs::rename(&building, &cache).is_err() {
        let _ = std::fs::remove_dir_all(&building);
    }
    Manifest::read(&cache, &recipe, &dir).ok_or_else(|| {
        format!(
            "{}: built, but the manifest does not read back",
            cache.display()
        )
    })
}

/// Where the built library lives between test processes:
/// `<temp>/rb-recipe-cache/<recipe>-<key>/`, the key a hash of everything the
/// build reads — the binary (size and mtime, so a rebuild is a new key), the
/// recipe file's bytes and every input layer's files (size and mtime).
fn cache_dir(name: &str, recipe_path: &Path, inputs: &BTreeMap<String, (u64, u128)>) -> PathBuf {
    let mut h = DefaultHasher::new();
    BIN.hash(&mut h);
    let stamp = |p: &Path| -> (u64, u128) {
        let Ok(m) = std::fs::metadata(p) else {
            return (0, 0);
        };
        let mtime = m
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos());
        (m.len(), mtime)
    };
    stamp(Path::new(BIN)).hash(&mut h);
    std::fs::read(recipe_path).unwrap_or_default().hash(&mut h);
    recipe_path.hash(&mut h);
    for (p, (len, mtime)) in inputs {
        (p, len, mtime).hash(&mut h);
    }
    std::env::temp_dir()
        .join("rb-recipe-cache")
        .join(format!("{name}-{:016x}", h.finish()))
}

/// What building produced, as kept on disk. `Invocation` itself carries no
/// serde (it is the runner's type, and `millis` is a `u128` TOML has no
/// integer for), so a mirror with the same fields is written instead.
#[derive(Serialize, Deserialize)]
struct Manifest {
    runs: Vec<Inv>,
    lock_first: String,
    lock_second: String,
    reapplied: Vec<Inv>,
    inputs_before: Vec<Input>,
    inputs_after: Vec<Input>,
}

impl Manifest {
    /// The cached build under `cache`, or `None` if there is none that reads.
    fn read(cache: &Path, recipe: &Recipe, dir: &Path) -> Option<Built> {
        let text = std::fs::read_to_string(cache.join("built.toml")).ok()?;
        let m: Manifest = toml::from_str(&text).ok()?;
        if !recipe::db_path(dir).is_file() {
            return None;
        }
        Some(Built {
            recipe: recipe.clone(),
            dir: dir.to_path_buf(),
            runs: m.runs.into_iter().map(Inv::into).collect(),
            lock_first: m.lock_first,
            lock_second: m.lock_second,
            reapplied: m.reapplied.into_iter().map(Inv::into).collect(),
            inputs_before: Input::map(m.inputs_before),
            inputs_after: Input::map(m.inputs_after),
        })
    }
}

#[derive(Serialize, Deserialize)]
struct Inv {
    step: usize,
    kind: String,
    argv: Vec<String>,
    expect_failure: bool,
    case: Option<String>,
    exit: Option<i32>,
    millis: u64,
    stdout: String,
    stderr: String,
}

impl From<&Invocation> for Inv {
    fn from(i: &Invocation) -> Self {
        Inv {
            step: i.step,
            kind: i.kind.clone(),
            argv: i.argv.clone(),
            expect_failure: i.expect_failure,
            case: i.case.clone(),
            exit: i.exit,
            millis: u64::try_from(i.millis).unwrap_or(u64::MAX),
            stdout: i.stdout.clone(),
            stderr: i.stderr.clone(),
        }
    }
}

impl From<Inv> for Invocation {
    fn from(i: Inv) -> Self {
        Invocation {
            step: i.step,
            kind: i.kind,
            argv: i.argv,
            expect_failure: i.expect_failure,
            case: i.case,
            exit: i.exit,
            millis: u128::from(i.millis),
            stdout: i.stdout,
            stderr: i.stderr,
        }
    }
}

/// One input file's size and mtime, as `digest_inputs` records them. The
/// mtime is nanoseconds since the epoch, which fits a `u64` until 2554.
#[derive(Serialize, Deserialize)]
struct Input {
    path: String,
    len: u64,
    mtime: u64,
}

impl Input {
    fn list(m: &BTreeMap<String, (u64, u128)>) -> Vec<Input> {
        m.iter()
            .map(|(p, (len, mtime))| Input {
                path: p.clone(),
                len: *len,
                mtime: u64::try_from(*mtime).unwrap_or(u64::MAX),
            })
            .collect()
    }
    fn map(v: Vec<Input>) -> BTreeMap<String, (u64, u128)> {
        v.into_iter()
            .map(|i| (i.path, (i.len, u128::from(i.mtime))))
            .collect()
    }
}

/// Size and mtime of every file under every step's input — the repo-side
/// half of invariant 6: a build reads its layers and never writes them.
fn digest_inputs(r: &Recipe) -> BTreeMap<String, (u64, u128)> {
    fn walk(p: &Path, out: &mut BTreeMap<String, (u64, u128)>) {
        let Ok(meta) = std::fs::metadata(p) else {
            return;
        };
        if meta.is_dir() {
            if let Ok(rd) = std::fs::read_dir(p) {
                for e in rd.flatten() {
                    walk(&e.path(), out);
                }
            }
        } else {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            out.insert(p.display().to_string(), (meta.len(), mtime));
        }
    }
    let root = recipe::repo_root();
    let mut out = BTreeMap::new();
    for s in &r.steps {
        for p in [&s.dir, &s.library, &s.root, &s.mount, &s.csv, &s.file]
            .into_iter()
            .flatten()
        {
            walk(&root.join(p), &mut out);
        }
    }
    out
}

fn db(b: &Built) -> PathBuf {
    recipe::db_path(&b.dir)
}

/// Whether `what` — a comparison against an expectation recorded with every
/// tool the recipe's steps need — can be made on this machine. Without one
/// (calibre, on CI's ubuntu runner) the library still builds and every other
/// invariant still holds of it, but it is a different library from the one
/// the expectation describes, so comparing reports a difference nobody made
/// and re-recording would record the wrong one. Skips loudly, and fails
/// under `BOOKMOLE_REQUIRE_FIXTURES=1`.
fn comparable(b: &Built, what: &str) -> bool {
    let missing = recipe::missing_tools(&b.recipe);
    if missing.is_empty() {
        return true;
    }
    assert!(
        !require(),
        "BOOKMOLE_REQUIRE_FIXTURES=1 and {what} cannot be compared: not installed: {}",
        missing.join(", ")
    );
    eprintln!(
        "SKIPPED: {what}: not installed: {} — {}'s expectation was recorded with it",
        missing.join(", "),
        b.recipe.name
    );
    false
}

fn rows(b: &Built, sql: &str) -> Vec<Vec<String>> {
    recipe::query(&db(b), sql).unwrap_or_else(|e| panic!("{e}"))
}

fn scalar(b: &Built, sql: &str) -> i64 {
    rows(b, &format!("SELECT hex(({sql}))"))[0][0]
        .parse()
        .unwrap_or_else(|_| panic!("not a number: {sql}"))
}

// ---- 1 -------------------------------------------------------------------

#[test]
fn every_step_exits_as_the_recipe_declares() {
    let Some(b) = built() else { return };
    let bad: Vec<String> = b
        .recipe
        .violations(&b.runs)
        .iter()
        .map(|(r, what)| {
            format!(
                "step {} {} {:?} exit {:?}: {what}\n{}",
                r.step, r.kind, r.argv, r.exit, r.stderr
            )
        })
        .collect();
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

// ---- 2 -------------------------------------------------------------------

#[test]
fn reapplying_the_imports_changes_nothing() {
    let Some(b) = built() else { return };
    let mut changed = Vec::new();
    for r in &b.reapplied {
        assert!(
            r.exit != Some(101),
            "re-applied step {} panicked: {}",
            r.step,
            r.stderr
        );
        let c = recipe::changes_reported(&r.stdout);
        if !c.is_empty() {
            changed.push(format!("step {} {:?}: {c:?}\n{}", r.step, r.argv, r.stdout));
        }
    }
    assert!(
        changed.is_empty(),
        "a second application reported changes:\n{}",
        changed.join("\n")
    );
    assert_eq!(
        b.lock_first,
        b.lock_second,
        "the semantic lock moved on re-application:\n{}",
        recipe::diff(&b.lock_first, &b.lock_second)
    );
    let skipped: Vec<String> = b
        .recipe
        .steps
        .iter()
        .filter(|s| !s.reapplies() && s.reapply == Some(false))
        .map(|s| format!("{} ({})", s.kind, s.reapply_why.as_deref().unwrap_or("")))
        .collect();
    if !skipped.is_empty() {
        eprintln!("NOTE: not re-applied, by the recipe's own declaration: {skipped:?}");
    }
}

/// The strict half of `reapply = false`: an excluded step must still be
/// non-idempotent. Each one is re-applied (its first three inputs) to a copy
/// of the built library; when that changes nothing, the engine bug its
/// `reapply_why` cites is fixed and this fails until the exclusion is removed,
/// so invariant 2 regains the step instead of the comment rotting.
#[test]
fn excluded_steps_are_still_not_idempotent() {
    let Some(b) = built() else { return };
    for (i, step) in b.recipe.steps.iter().enumerate() {
        if step.reapply != Some(false) {
            continue;
        }
        let probe_dir = b.dir.with_file_name(format!(
            "{}-probe{i}",
            b.dir.file_name().unwrap().to_string_lossy()
        ));
        let _ = std::fs::remove_dir_all(&probe_dir);
        copy_tree(&b.dir, &probe_dir);
        let mut probe = b.recipe.clone();
        let mut one = step.clone();
        one.limit = Some(one.limit.map_or(3, |n| n.min(3)));
        one.reapply = None;
        probe.steps = vec![one];
        let before = recipe::semantic_lock(&probe_dir, &b.recipe, &b.runs).unwrap();
        let runs = recipe::run(&probe, &probe_dir, Path::new(BIN), false).unwrap();
        let after = recipe::semantic_lock(&probe_dir, &b.recipe, &b.runs).unwrap();
        let reported: Vec<_> = runs
            .iter()
            .flat_map(|r| recipe::changes_reported(&r.stdout))
            .collect();
        let _ = std::fs::remove_dir_all(&probe_dir);
        assert!(
            before != after || !reported.is_empty(),
            "step {i} ({}) sets reapply = false ({}), but re-applying it changed \
             nothing: the bug is fixed — remove reapply/reapply_why from {} so \
             invariant 2 covers it again",
            step.kind,
            step.reapply_why.as_deref().unwrap_or(""),
            b.recipe.source.display()
        );
        eprintln!(
            "NOTE: step {i} ({}) is still not idempotent, as its reapply_why says",
            step.kind
        );
    }
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let (src, dst) = (e.path(), to.join(e.file_name()));
        let ft = e.file_type().unwrap();
        if ft.is_dir() {
            copy_tree(&src, &dst);
        } else if ft.is_file() {
            std::fs::copy(&src, &dst).unwrap();
        }
    }
}

// ---- 3 -------------------------------------------------------------------

#[test]
fn the_semantic_lock_matches_its_expectation() {
    let Some(b) = built() else { return };
    if !comparable(b, "semantic lock") {
        return;
    }
    let path = recipe::expect_path(&b.recipe);
    let want = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e} — run `cargo run -p corpus -- build-recipe {} --data-dir <tmp> --update-expect`",
            path.display(),
            b.recipe.name
        )
    });
    assert!(
        want == b.lock_first,
        "{} differs from this build:\n{}",
        path.display(),
        recipe::diff(&want, &b.lock_first)
    );
}

// ---- 4 -------------------------------------------------------------------

/// The longest plain ASCII word of four letters or more — something FTS5's
/// porter/unicode61 tokenizer will index as itself on any body.
fn search_word(body: &str) -> Option<String> {
    body.split(|c: char| !c.is_ascii_alphabetic())
        .filter(|w| w.len() >= 4)
        .max_by_key(|w| w.len())
        .map(str::to_lowercase)
}

#[test]
fn every_note_is_found_by_a_word_of_its_own_body() {
    let Some(b) = built() else { return };
    let root = tempfile::tempdir().unwrap();
    let notes = rows(
        b,
        "SELECT hex(n.id), hex(f.body) FROM notes n JOIN notes_fts f ON f.rowid = n.id ORDER BY n.id",
    );
    assert_eq!(
        notes.len() as i64,
        scalar(b, "SELECT count(*) FROM notes"),
        "a note without its notes_fts row is a note search cannot find"
    );
    let mut lost = Vec::new();
    let mut unsearchable = 0;
    for n in &notes {
        let (id, body) = (&n[0], &n[1]);
        let Some(word) = search_word(body) else {
            unsearchable += 1;
            continue;
        };
        let o = sandbox::command(root.path())
            .arg("--data-dir")
            .arg(&b.dir)
            .args(["find", "--notes", "--limit", "10000", "--", &word])
            .output()
            .unwrap();
        let out = String::from_utf8_lossy(&o.stdout);
        if !out.contains(&format!("#{id} ")) {
            lost.push(format!("note #{id} not found by '{word}'"));
        }
    }
    if unsearchable > 0 {
        eprintln!("NOTE: {unsearchable} note(s) have no plain ASCII word to search by");
    }
    assert!(lost.is_empty(), "{}", lost.join("\n"));
}

// ---- 5 -------------------------------------------------------------------

#[test]
fn every_attributed_highlight_is_dated_inside_its_reading() {
    let Some(b) = built() else { return };
    // A day of slack either side: ko_datetime is the device's local wall
    // clock, the window is unix seconds. A reading with no start date (an
    // imported `currently-reading` row) is not bounded by when it was
    // imported: its window opens just after the book's previous finished
    // read, or is open-ended when there is none.
    let bad = rows(
        b,
        "SELECT hex(h.id), hex(h.ko_datetime), hex(r.started_at), hex(r.finished_at) \
         FROM highlights h JOIN readings r ON r.id = h.reading_id \
         WHERE h.ko_datetime IS NOT NULL AND ( \
           CAST(strftime('%s', h.ko_datetime) AS INTEGER) < COALESCE(r.started_at, \
             (SELECT MAX(p.finished_at) + 1 FROM readings p \
               WHERE p.book_id = r.book_id AND p.id <> r.id AND p.finished_at IS NOT NULL \
                 AND p.finished_at <= COALESCE(r.finished_at, 8640000000000)), \
             -8640000000000) - 86400 \
           OR (r.finished_at IS NOT NULL AND CAST(strftime('%s', h.ko_datetime) AS INTEGER) > r.finished_at + 86400))",
    );
    assert!(
        bad.is_empty(),
        "highlights dated outside the reading they are attributed to: {bad:?}"
    );
}

// ---- 6 -------------------------------------------------------------------

#[test]
fn nothing_escapes_and_no_text_holds_a_nul() {
    let Some(b) = built() else { return };
    for r in rows(b, "SELECT hex(file_path) FROM notes") {
        let p = Path::new(&r[0]);
        assert!(
            p.is_relative()
                && !p
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir)),
            "a vault path escapes the vault: {:?}",
            r[0]
        );
        assert!(
            b.dir.join("vault").join(p).is_file(),
            "a note's file is missing from the vault: {:?}",
            r[0]
        );
    }
    for r in rows(b, "SELECT hex(sha256), hex(format) FROM book_files") {
        assert!(
            r[0].len() == 64 && r[0].bytes().all(|c| c.is_ascii_hexdigit()),
            "an owned file's address is not a digest: {:?}",
            r[0]
        );
        assert!(
            !r[1].is_empty() && r[1].bytes().all(|c| c.is_ascii_alphanumeric()),
            "an owned file's extension could leave files/: {:?}",
            r[1]
        );
    }
    for (table, col) in [
        ("books", "title"),
        ("books", "description"),
        ("highlights", "text"),
        ("highlights", "origin_note"),
        ("highlights", "annotation"),
        ("notes", "title"),
        ("notes_fts", "body"),
    ] {
        let n = scalar(
            b,
            &format!("SELECT count(*) FROM {table} WHERE instr(CAST({col} AS BLOB), x'00') > 0"),
        );
        // Exactly the rows a KOReader layer's manifest salts with `nul-byte`
        // carry one, and no others: the engine keeps the byte today (an open
        // question whether import should strip U+0000 before it reaches a
        // C-string consumer such as a terminal or clipboard), so the invariant
        // pins that nothing *else* lets one in.
        let want = nul_salts_for(b).get(&(table, col)).copied().unwrap_or(0);
        assert_eq!(
            n, want,
            "{table}.{col} holds a NUL byte in {n} row(s); the layers salted {want}"
        );
    }
    assert_eq!(
        b.inputs_before, b.inputs_after,
        "the build wrote to the layers it read"
    );
}

/// How many NUL-bearing rows per column the recipe's KOReader layers declare,
/// read from each `ko-sync` root's `manifest.json` (`salted[]` with
/// `case = "nul-byte"`): a `highlight` salt lands in `highlights.text`, a
/// `ko-note` or `lua-string` one in `highlights.origin_note`.
fn nul_salts_for(b: &Built) -> std::collections::BTreeMap<(&'static str, &'static str), i64> {
    let mut allowed = std::collections::BTreeMap::new();
    for step in b.recipe.steps.iter().filter(|s| s.kind == "ko-sync") {
        let Some(root) = &step.root else { continue };
        let path = recipe::repo_root().join(root).join("manifest.json");
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let m = json::parse(&raw).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        for s in m
            .get("salted")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            if s.get("case").and_then(|c| c.as_str()) != Some("nul-byte") {
                continue;
            }
            let col = match s.get("field").and_then(|f| f.as_str()) {
                Some("highlight") => ("highlights", "text"),
                Some("ko-note" | "lua-string") => ("highlights", "origin_note"),
                _ => continue,
            };
            *allowed.entry(col).or_insert(0) += 1;
        }
    }
    allowed
}

// ---- 7 -------------------------------------------------------------------

/// `hostile.json`'s declared class per case id.
fn declared_classes() -> BTreeMap<String, (String, String)> {
    let text = std::fs::read_to_string(recipe::repo_root().join("crates/corpus/hostile.json"))
        .expect("hostile.json");
    let v = json::parse(&text).expect("hostile.json parses");
    v.get("cases")
        .and_then(json::Value::as_array)
        .expect("cases")
        .iter()
        .map(|c| {
            let s = |k| {
                c.get(k)
                    .and_then(json::Value::as_str)
                    .unwrap_or("")
                    .to_owned()
            };
            (s("id"), (s("expect"), s("value")))
        })
        .collect()
}

#[test]
fn every_salted_case_reaches_its_declared_class() {
    let Some(b) = built() else { return };
    let classes = declared_classes();
    let bodies: Vec<String> = rows(b, "SELECT hex(body) FROM notes_fts")
        .into_iter()
        .map(|r| r[0].clone())
        .collect();
    let mut bad = Vec::new();
    let mut seen = 0;
    for r in b.runs.iter().filter(|r| r.case.is_some()) {
        let case = r.case.as_deref().unwrap_or_default();
        seen += 1;
        let Some((class, value)) = classes.get(case) else {
            bad.push(format!("{case}: not declared in hostile.json"));
            continue;
        };
        if r.exit == Some(101) || r.exit.is_none() {
            bad.push(format!("{case}: panicked or was killed: {}", r.stderr));
            continue;
        }
        let known = b.recipe.is_known(Some(case));
        let as_declared = match class.as_str() {
            "rejected" => r.exit != Some(0),
            "verbatim" if r.argv.first().is_some_and(|a| a == "note") => {
                // The vault stores a body with one trailing newline; that is
                // the only difference tolerated, and it is stated here.
                r.exit == Some(0)
                    && bodies
                        .iter()
                        .any(|body| body.trim_end_matches('\n') == value)
            }
            "verbatim" | "normalised" | "no-crash" => r.as_expected(),
            other => {
                bad.push(format!("{case}: unknown class {other}"));
                continue;
            }
        };
        match (as_declared, known) {
            (false, false) => bad.push(format!(
                "{case} (declared {class}): exit {:?}, argv {:?}",
                r.exit, r.argv
            )),
            (true, true) => bad.push(format!(
                "{case}: listed under [[known]] in the recipe but now reaches {class} — promote it"
            )),
            _ => {}
        }
    }
    if seen == 0 {
        eprintln!("NOTE: recipe {} salts no hostile case", b.recipe.name);
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
