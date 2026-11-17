use std::path::{Path, PathBuf};

use crate::diagnostic::Diagnostic;
use crate::error::{EngineError, Result};
use crate::koreader::{
    self, KoSidecar, KoStatus, MatchCandidate, MatchMethod, PullReport, find_sidecars,
    parse_sidecar,
};
use crate::storage::DeviceDigest;
use crate::storage::{NewHighlight, SidecarFacts, Storage};

/// Kindle keeps KOReader at the volume root, Kobo in `.adds/koreader`, PocketBook in
/// `applications/koreader`.
const KOREADER_DIRS: [&str; 3] = ["koreader", ".adds/koreader", "applications/koreader"];

/// A bare `koreader` folder is not an install. This predicate also gates the plugin installer's
/// writes,
/// so bookmole never writes to an unrecognised volume.
const KOREADER_MARKERS: [(&str, Kind); 3] = [
    ("reader.lua", Kind::File),
    ("frontend", Kind::Dir),
    ("plugins", Kind::Dir),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    File,
    Dir,
}

const MOUNT_ROOTS: [&str; 1] = ["/Volumes"];
const USER_MOUNT_ROOTS: [&str; 2] = ["/run/media", "/media"];

pub fn koreader_dir(mount: &Path) -> Option<PathBuf> {
    KOREADER_DIRS
        .iter()
        .map(|rel| mount.join(rel))
        .find(|dir| has_markers(dir))
}

fn has_markers(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    KOREADER_MARKERS.iter().all(|(name, kind)| {
        let p = dir.join(name);
        match kind {
            Kind::File => p.is_file(),
            Kind::Dir => p.is_dir(),
        }
    })
}

pub fn is_koreader_mount(p: &Path) -> bool {
    koreader_dir(p).is_some()
}

/// Public because the watcher must watch exactly the directories `candidate_mounts` walks.
pub fn mount_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = MOUNT_ROOTS.iter().map(PathBuf::from).collect();
    if let Ok(user) = std::env::var("USER") {
        roots.extend(USER_MOUNT_ROOTS.iter().map(|r| Path::new(r).join(&user)));
    }
    roots.retain(|r| r.is_dir());
    roots
}

/// `is_koreader_mount` plus the symlink rule; the watcher and the lister must agree on it.
pub fn offers_reader(path: &Path) -> bool {
    // `/Volumes/Macintosh HD` is a symlink to `/` on macOS; following it would offer the boot disk.
    let is_link = std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);
    !is_link && is_koreader_mount(path)
}

pub fn candidate_mounts() -> Vec<PathBuf> {
    let mut found = Vec::new();
    for root in mount_roots() {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if offers_reader(&p) {
                found.push(p);
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

/// Kindle `documents/My Clippings.txt` files on volumes directly under `roots`. Symlinked volumes
/// are skipped.
pub fn kindle_clippings_on(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let volume = entry.path();
            let is_link = std::fs::symlink_metadata(&volume)
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(true);
            if is_link || !volume.is_dir() {
                continue;
            }
            if let Some(path) = crate::kindle_clippings::clippings_path_on(&volume) {
                found.push(path);
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

#[derive(Debug, Clone, PartialEq)]
pub enum DeviceState {
    /// Not in the library. Non-empty `candidates` are likely matches to an existing book.
    New {
        candidates: Vec<MatchCandidate>,
    },
    Unchanged,
    /// Linked, with annotations to bring across. `refreshed` counts rows whose device-owned fields
    /// differ.
    Updated {
        new_highlights: usize,
        refreshed: usize,
    },
    /// The sidecar could not be read or parsed. Never fatal to the scan.
    Unreadable(Diagnostic),
}

impl DeviceState {
    pub fn is_syncable(&self) -> bool {
        matches!(self, DeviceState::New { .. } | DeviceState::Updated { .. })
    }

    pub fn label(&self) -> &'static str {
        match self {
            DeviceState::New { .. } => "new",
            DeviceState::Unchanged => "unchanged",
            DeviceState::Updated { .. } => "updated",
            DeviceState::Unreadable(_) => "unreadable",
        }
    }
}

#[derive(Debug, Clone)]
pub struct DeviceBook {
    pub path: PathBuf,
    pub title: Option<String>,
    pub authors: Option<String>,
    pub partial_md5: Option<String>,
    pub book_id: Option<i64>,
    pub matched_by: Option<MatchMethod>,
    pub state: DeviceState,
    pub ko_percent: Option<f64>,
    pub ko_status: Option<KoStatus>,
}

impl DeviceBook {
    /// Falls back to the `.sdr` directory name, the only identifier an unreadable sidecar has.
    pub fn display_title(&self) -> String {
        if let Some(t) = &self.title {
            return t.clone();
        }
        self.path
            .parent()
            .and_then(|d| d.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unknown title".to_string())
    }
}

#[derive(Debug, Clone, Default)]
pub struct DeviceScan {
    pub root: PathBuf,
    pub books: Vec<DeviceBook>,
    pub warnings: Vec<Diagnostic>,
    /// Sidecars whose Lua was evaluated by this scan; zero on a second scan of an unmodified tree.
    pub parsed: usize,
    pub cached: usize,
}

impl DeviceScan {
    pub fn syncable(&self) -> impl Iterator<Item = &DeviceBook> {
        self.books.iter().filter(|b| b.state.is_syncable())
    }
}

/// Carries the scan as well as the imports: "nothing to bring across" and "nothing on the device"
/// differ.
#[derive(Debug, Default)]
pub struct MountSync {
    pub mount: PathBuf,
    /// `None` for an unpaired or foreign-paired volume; the sync still runs but `last_synced_at` is not
    /// stamped.
    pub device_id: Option<String>,
    pub found: usize,
    pub synced: usize,
    pub reports: Vec<PullReport>,
    /// The scan's warnings; each report carries its own import's.
    pub warnings: Vec<Diagnostic>,
}

/// Read-only. An unreadable sidecar becomes `DeviceState::Unreadable`, not an error.
#[tracing::instrument(skip(storage), fields(root = %root.display()))]
pub async fn scan_device(storage: &Storage, root: &Path) -> Result<DeviceScan> {
    // Nanoseconds, compared for equality rather than order.
    let seen_at = nanos(std::time::SystemTime::now());
    let mut scan = DeviceScan {
        root: root.to_path_buf(),
        ..Default::default()
    };

    let sidecars = find_sidecars(root)?;
    if sidecars.is_empty() {
        tracing::warn!(root = %root.display(), "no KOReader sidecars found");
        scan.warnings
            .push(Diagnostic::no_sidecars_found(root.to_path_buf()));
        return Ok(scan);
    }

    let mut counts = (0usize, 0usize);
    for path in sidecars {
        scan.books
            .push(scan_one(storage, &path, seen_at, &mut counts).await?);
    }
    (scan.parsed, scan.cached) = counts;

    // Scoped to this root so unplugging one device does not forget another's cache rows.
    let forgotten = storage
        .forget_sidecars_under(&root.to_string_lossy(), seen_at)
        .await?;

    tracing::info!(
        books = scan.books.len(),
        parsed = scan.parsed,
        cached = scan.cached,
        forgotten,
        "scanned device"
    );
    Ok(scan)
}

async fn scan_one(
    storage: &Storage,
    path: &Path,
    seen_at: i64,
    counts: &mut (usize, usize),
) -> Result<DeviceBook> {
    let key = path.to_string_lossy().into_owned();

    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) => {
            let err = EngineError::from(e);
            return Ok(unreadable(
                path,
                Diagnostic::sidecar_unreadable(path.to_path_buf(), &err),
            ));
        }
    };
    let size = meta.len() as i64;
    let mtime = mtime_nanos(&meta);

    let fresh = storage
        .sidecar_facts(&key)
        .await?
        .filter(|f| f.size == size && f.mtime == mtime);

    let mut highlights: Option<Vec<NewHighlight>> = None;
    let facts = match fresh {
        Some(f) => {
            counts.1 += 1;
            storage.touch_sidecar_seen(&key, seen_at).await?;
            f
        }
        None => {
            counts.0 += 1;
            let sc = match read_sidecar(path) {
                Ok(sc) => sc,
                // An unparsable file has nothing to cache; it is re-read on every scan.
                Err(diag) => return Ok(unreadable(path, diag)),
            };
            let f = facts_of(&sc, size, mtime);
            storage.record_sidecar_facts(&key, &f, seen_at).await?;
            highlights = Some(sc.highlights);
            f
        }
    };

    // The matcher reads only `partial_md5`, `title` and `authors`, which the cache keeps verbatim,
    // so a hit and a fresh parse reach the same verdict.
    let probe = KoSidecar {
        title: facts.title.clone(),
        authors: facts.authors.clone(),
        partial_md5: facts.partial_md5.clone(),
        ..Default::default()
    };

    let ko_status = facts.ko_status.as_deref().map(KoStatus::from);
    let mut book = DeviceBook {
        path: path.to_path_buf(),
        title: facts.title.clone(),
        authors: facts.authors.clone(),
        partial_md5: facts.partial_md5.clone(),
        book_id: None,
        matched_by: None,
        state: DeviceState::Unchanged,
        ko_percent: facts.ko_percent,
        ko_status,
    };

    let Some((matched, matched_by)) = koreader::match_book(storage, path, &probe).await? else {
        book.state = DeviceState::New {
            candidates: koreader::match_candidates(storage, &probe).await?,
        };
        return Ok(book);
    };
    let Some(book_id) = matched.id else {
        tracing::error!(title = %matched.display_title(), "matched book has no id");
        book.state = DeviceState::New {
            candidates: Vec::new(),
        };
        return Ok(book);
    };
    book.book_id = Some(book_id);
    book.matched_by = Some(matched_by);
    if book.title.is_none() {
        book.title = Some(matched.display_title().to_string());
    }

    // A row count would miss a note rewritten on the device; the digest covers the device-owned
    // payload.
    if highlights.is_none() && storage.device_highlight_digest(book_id).await? == facts.entry_digest
    {
        book.state = DeviceState::Unchanged;
        return Ok(book);
    }

    let highlights = match highlights {
        Some(h) => h,
        None => {
            counts.0 += 1;
            match read_sidecar(path) {
                Ok(sc) => sc.highlights,
                Err(diag) => return Ok(unreadable(path, diag)),
            }
        }
    };

    let mut new_highlights = 0;
    let mut refreshed = 0;
    for h in &highlights {
        if !storage.highlight_exists(book_id, h).await? {
            new_highlights += 1;
        } else if storage.device_fields_differ(book_id, h).await? {
            refreshed += 1;
        }
    }
    book.state = if new_highlights == 0 && refreshed == 0 {
        DeviceState::Unchanged
    } else {
        DeviceState::Updated {
            new_highlights,
            refreshed,
        }
    };
    Ok(book)
}

fn unreadable(path: &Path, diagnostic: Diagnostic) -> DeviceBook {
    DeviceBook {
        path: path.to_path_buf(),
        title: None,
        authors: None,
        partial_md5: None,
        book_id: None,
        matched_by: None,
        state: DeviceState::Unreadable(diagnostic),
        ko_percent: None,
        ko_status: None,
    }
}

fn read_sidecar(path: &Path) -> std::result::Result<KoSidecar, Diagnostic> {
    let src = std::fs::read_to_string(path).map_err(|e| {
        let err = EngineError::from(e);
        tracing::warn!(path = %path.display(), error = %err, "sidecar unreadable");
        Diagnostic::sidecar_unreadable(path.to_path_buf(), &err)
    })?;
    parse_sidecar(&src).map_err(|e| {
        tracing::warn!(path = %path.display(), error = %e, "sidecar unparsable");
        Diagnostic::sidecar_unparsable(path.to_path_buf(), &e)
    })
}

/// Highlight text is deliberately not kept: it is private reading and already lives in
/// `highlights`.
fn facts_of(sc: &KoSidecar, size: i64, mtime: i64) -> SidecarFacts {
    SidecarFacts {
        size,
        mtime,
        partial_md5: sc.partial_md5.clone(),
        entry_count: sc.highlights.len() as i64,
        entry_digest: digest_of(&sc.highlights),
        // Must equal `doc_props.title` exactly: the matcher must reach the same verdict from cache or
        // parse.
        title: sc.title.clone(),
        authors: sc.authors.clone(),
        ko_percent: sc.percent_finished,
        ko_status: sc
            .summary
            .as_ref()
            .and_then(|s| s.status.as_ref())
            .map(|s| s.to_string()),
    }
}

/// Must share `DeviceDigest` with `Storage::device_highlight_digest`, or the scan stops noticing
/// device edits.
fn digest_of(highlights: &[NewHighlight]) -> String {
    let mut digest = DeviceDigest::new();
    for h in highlights {
        digest.add(h.device_entry());
    }
    digest.finish()
}

fn mtime_nanos(meta: &std::fs::Metadata) -> i64 {
    meta.modified().ok().map(nanos).unwrap_or(0)
}

/// A pre-1970 stamp (dead device clock) collapses to 0; the cost is a re-parse.
fn nanos(t: std::time::SystemTime) -> i64 {
    t.duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// A directory expands to every sidecar under it, so each report covers one book.
/// Unlike a scan, an unreadable sidecar is an error here.
#[tracing::instrument(skip_all, fields(paths = paths.len()))]
pub async fn sync_device(storage: &Storage, paths: &[PathBuf]) -> Result<Vec<PullReport>> {
    sync_device_for(storage, paths, None).await
}

pub async fn sync_device_for(
    storage: &Storage,
    paths: &[PathBuf],
    device_id: Option<&str>,
) -> Result<Vec<PullReport>> {
    let mut out = Vec::new();
    for path in paths {
        for sidecar in find_sidecars(path)? {
            out.push(sync_one(storage, &sidecar, device_id).await?);
        }
    }
    Ok(out)
}

async fn sync_one(
    storage: &Storage,
    sidecar: &Path,
    device_id: Option<&str>,
) -> Result<PullReport> {
    // Import first: it matches an existing book; creating directly would duplicate an already-linked
    // book.
    let mut report = koreader::import_for_device(storage, sidecar, false, device_id).await?;
    if let Some(stats) = report.imported.pop() {
        return Ok(PullReport {
            stats,
            warnings: report.warnings,
        });
    }

    let mut pulled =
        koreader::import_book_from_sidecar_for_device(storage, sidecar, device_id).await?;
    let mut warnings = report.warnings;
    warnings.append(&mut pulled.warnings);
    pulled.warnings = warnings;
    Ok(pulled)
}

/// One definition of a plausible reader, shared by every fixture, since `is_koreader_mount` gates
/// reads and writes.
#[cfg(any(test, feature = "internals"))]
pub fn install_fake_reader(root: &Path) {
    let dir = root.join("koreader");
    std::fs::create_dir_all(dir.join("frontend")).unwrap();
    std::fs::create_dir_all(dir.join("plugins")).unwrap();
    std::fs::write(dir.join("reader.lua"), "-- entry point\n").unwrap();
}

#[cfg(test)]
pub(crate) mod tests {
    pub(crate) use super::install_fake_reader as install;
    use super::*;

    #[test]
    fn a_real_install_is_recognised_wherever_the_platform_puts_it() {
        for rel in KOREADER_DIRS {
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path().join(rel);
            std::fs::create_dir_all(dir.join("frontend")).unwrap();
            std::fs::create_dir_all(dir.join("plugins")).unwrap();
            std::fs::write(dir.join("reader.lua"), "-- entry point\n").unwrap();

            assert!(is_koreader_mount(tmp.path()), "{rel} should be recognised");
            assert_eq!(koreader_dir(tmp.path()), Some(dir));
        }
    }

    #[test]
    fn an_ordinary_directory_and_a_look_alike_are_both_refused() {
        let plain = tempfile::tempdir().unwrap();
        assert!(!is_koreader_mount(plain.path()));
        assert_eq!(koreader_dir(plain.path()), None);

        let fake = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(fake.path().join("koreader")).unwrap();
        std::fs::write(fake.path().join("koreader/notes.txt"), "mine").unwrap();
        assert!(!is_koreader_mount(fake.path()));

        let partial = tempfile::tempdir().unwrap();
        let dir = partial.path().join("koreader");
        std::fs::create_dir_all(dir.join("frontend")).unwrap();
        std::fs::write(dir.join("reader.lua"), "-- entry point\n").unwrap();
        assert!(
            !is_koreader_mount(partial.path()),
            "plugins/ is missing, and that is where we would write"
        );

        let typed = tempfile::tempdir().unwrap();
        let dir = typed.path().join("koreader");
        std::fs::create_dir_all(dir.join("frontend")).unwrap();
        std::fs::create_dir_all(dir.join("plugins")).unwrap();
        std::fs::create_dir_all(dir.join("reader.lua")).unwrap();
        assert!(!is_koreader_mount(typed.path()));
    }

    #[test]
    fn every_offered_mount_is_one_we_would_accept() {
        for m in candidate_mounts() {
            assert!(is_koreader_mount(&m), "{} was offered", m.display());
        }
    }

    #[test]
    fn a_mount_is_recognised_by_its_contents_not_its_name() {
        let tmp = tempfile::tempdir().unwrap();
        install(tmp.path());
        assert!(is_koreader_mount(tmp.path()));
    }
}
