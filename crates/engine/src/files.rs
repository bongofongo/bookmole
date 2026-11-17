//! Owned files: a content-addressed store (`database/files/<ab>/<sha256>.<ext>`) plus the dedup
//! ladder. Importing records a `path` copy without copying bytes; `keep_copy_here` stores one.
//! `partial_md5` samples windows and can collide, so it is never a content address; sha256 is.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

use crate::book::Book;
use crate::copies::{CopyForm, LearnedFrom, NewCopy, NewPlace, PlaceAttach, PlaceKind, Tenure};
use crate::error::{EngineError, Result};
use crate::koreader::MatchCandidate;
use crate::storage::{BookFile, LinkedBy, Storage};

const COPY_BUF: usize = 64 * 1024;

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// A separate enum from `koreader::MatchMethod`: `Sha256` is a rung a sidecar can never take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileMatch {
    Sha256,
    Isbn,
    Md5,
    Title,
}

impl std::fmt::Display for FileMatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileMatch::Sha256 => write!(f, "sha256"),
            FileMatch::Isbn => write!(f, "isbn"),
            FileMatch::Md5 => write!(f, "md5"),
            FileMatch::Title => write!(f, "title"),
        }
    }
}

/// Read-only: writes nothing to the database and copies no bytes.
#[derive(Debug, Clone)]
pub struct FileIdentity {
    pub path: PathBuf,
    pub sha256: String,
    pub partial_md5: String,
    pub format: String,
    pub size: i64,
    /// The file's own title, else the filename stem.
    pub title: Option<String>,
    /// PDF only. `None` is not zero: a zero here would be a false denominator.
    pub page_count: Option<i64>,
    pub matched: Option<(i64, FileMatch)>,
    pub candidates: Vec<MatchCandidate>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ImportOptions {
    /// Create a book even when a near-miss candidate exists; without it a variant title silently
    /// becomes a second copy.
    pub new: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOutcome {
    /// Recorded against the book; no bytes copied (`keep_copy_here` does that).
    Stored,
    AlreadyOwned,
    Unmatched,
}

#[derive(Debug, Clone)]
pub struct FileImportReport {
    pub outcome: FileOutcome,
    /// `None` only for `FileOutcome::Unmatched`.
    pub book_id: Option<i64>,
    pub matched_by: Option<FileMatch>,
    pub created_book: bool,
    pub sha256: String,
    /// The copy's `path` place: the absolute path imported from. Present for `Stored` and
    /// `AlreadyOwned`.
    pub stored_path: Option<PathBuf>,
    pub candidates: Vec<MatchCandidate>,
}

/// `<files_dir>/<first two hex>/<sha>.<ext>`.
pub fn content_path(files_dir: &Path, sha256: &str, format: &str) -> PathBuf {
    let shard = if sha256.len() >= 2 {
        &sha256[..2]
    } else {
        "00"
    };
    files_dir.join(shard).join(format!("{sha256}.{format}"))
}

/// The stored extension, lowercased, or `bin`. Sanitized to ASCII alphanumerics because it becomes
/// a path component.
pub fn format_of(path: &Path) -> String {
    let ext: String = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .take(16)
        .collect();
    if ext.is_empty() {
        "bin".to_string()
    } else {
        ext
    }
}

pub fn sha256_of(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; COPY_BUF];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[derive(Debug, Clone)]
pub struct Stored {
    pub sha256: String,
    pub size: i64,
    pub format: String,
    pub path: PathBuf,
    pub already_present: bool,
}

/// Crash-safe: copies to a temp name in `files_dir` and renames when complete, so a kill never
/// leaves a partial file at a name claiming to be its sha256.
pub fn store(files_dir: &Path, src: &Path) -> Result<Stored> {
    let format = format_of(src);
    std::fs::create_dir_all(files_dir)?;
    // Temp file in `files_dir`, not `$TMPDIR`: a rename across filesystems is a non-atomic copy.
    let tmp = files_dir.join(format!(
        ".incoming-{}-{}",
        std::process::id(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));

    let result = copy_and_hash(src, &tmp);
    let (sha256, size) = match result {
        Ok(v) => v,
        Err(e) => {
            std::fs::remove_file(&tmp).ok();
            return Err(e);
        }
    };

    let dest = content_path(files_dir, &sha256, &format);
    if let Some(parent) = dest.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        std::fs::remove_file(&tmp).ok();
        return Err(e.into());
    }

    // `exists()` then `rename` races only with a writer of identical bytes to an identical name, so it
    // is harmless.
    let already_present = dest.exists();
    if already_present {
        std::fs::remove_file(&tmp).ok();
    } else if let Err(e) = std::fs::rename(&tmp, &dest) {
        std::fs::remove_file(&tmp).ok();
        return Err(e.into());
    }

    Ok(Stored {
        sha256,
        size,
        format,
        path: dest,
        already_present,
    })
}

fn copy_and_hash(src: &Path, tmp: &Path) -> Result<(String, i64)> {
    let mut reader = std::fs::File::open(src)?;
    let mut writer = std::fs::File::create(tmp)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; COPY_BUF];
    let mut size: i64 = 0;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        writer.write_all(&buf[..n])?;
        size += n as i64;
    }
    // Flush before the rename, or the rename can land before the contents.
    writer.sync_all()?;
    Ok((hex(&hasher.finalize()), size))
}

/// Read-only: copies nothing and writes no row. Rungs are tried in descending certainty; the first
/// answer wins.
pub async fn identify(storage: &Storage, path: &Path) -> Result<FileIdentity> {
    if !path.is_file() {
        return Err(EngineError::InvalidInput(format!(
            "{} is not a file",
            path.display()
        )));
    }
    let format = format_of(path);
    let sha256 = sha256_of(path)?;
    let size = std::fs::metadata(path)?.len() as i64;
    let partial_md5 = crate::partial_md5::partial_md5(path)?;

    // A metadata failure (corrupt epub, encrypted pdf) is a fact about the file, not an error: the
    // bytes are still the user's book.
    let info = (format == "epub")
        .then(|| crate::epub::epub_info(path).ok())
        .flatten();
    let pdf = (format == "pdf")
        .then(|| crate::pdf::pdf_info(path).ok())
        .flatten()
        .unwrap_or_default();
    let title = info
        .as_ref()
        .and_then(|i| i.title.clone())
        .or_else(|| pdf.title.clone())
        .or_else(|| stem_title(path));

    if let Some(existing) = storage.book_file(&sha256).await? {
        return Ok(FileIdentity {
            path: path.to_path_buf(),
            sha256,
            partial_md5,
            format,
            size,
            title,
            page_count: pdf.page_count,
            matched: Some((existing.book_id, FileMatch::Sha256)),
            candidates: Vec::new(),
        });
    }

    let mut matched = None;
    if let Some(isbn) = info.as_ref().and_then(|i| i.isbn.clone())
        && let Some(book) = storage.find_book_by_isbn(&isbn).await?
        && let Some(id) = book.id
    {
        matched = Some((id, FileMatch::Isbn));
    }
    if matched.is_none()
        && let Some(book) = storage.find_book_by_partial_md5(&partial_md5).await?
        && let Some(id) = book.id
    {
        matched = Some((id, FileMatch::Md5));
    }
    let mut candidates = Vec::new();
    if matched.is_none() {
        let authors = info.as_ref().map(|i| i.authors.clone()).unwrap_or_default();
        let scored = crate::koreader::scores_for(
            storage,
            &crate::matching::Query::new(title.as_deref(), &authors),
        )
        .await?;
        match scored.first() {
            Some(s) if s.can_auto => {
                if let Some(id) = s.book.id {
                    matched = Some((id, FileMatch::Title));
                }
            }
            _ => candidates = crate::koreader::band(scored),
        }
    }

    Ok(FileIdentity {
        path: path.to_path_buf(),
        sha256,
        partial_md5,
        format,
        size,
        title,
        page_count: pdf.page_count,
        matched,
        candidates,
    })
}

/// Only fed to the fuzzy matcher, never stored as the title.
fn stem_title(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_string_lossy();
    let cleaned = stem.replace(['_', '.'], " ");
    let cleaned = cleaned.trim();
    (!cleaned.is_empty()).then(|| cleaned.to_string())
}

/// Copies no bytes; `files_dir` is unused and kept for call-site compatibility.
pub async fn attach(
    storage: &Storage,
    _files_dir: &Path,
    book_id: i64,
    path: &Path,
) -> Result<FileImportReport> {
    let id = identify(storage, path).await?;
    attach_identified(storage, book_id, &id, false, Annotations::Read).await
}

/// `attach` without reading a PDF's annotations, for the caller that reads them itself; reading
/// twice would report *unchanged*.
pub(crate) async fn attach_leaving_annotations(
    storage: &Storage,
    book_id: i64,
    path: &Path,
) -> Result<FileImportReport> {
    let id = identify(storage, path).await?;
    attach_identified(storage, book_id, &id, false, Annotations::Leave).await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Annotations {
    Read,
    Leave,
}

/// Reads the annotations already in a PDF, which would otherwise never be read since rescans
/// trigger only on mtime change.
/// Never fails the import; failures are logged as paths and kinds only.
async fn read_pdf_annotations(storage: &Storage, book_id: i64, copy_id: i64, id: &FileIdentity) {
    if id.format != "pdf" {
        return;
    }
    match crate::preview::import_pdf_annotations(storage, book_id, Some(copy_id), &id.path).await {
        Ok(summary) => {
            for d in &summary.diagnostics {
                tracing::warn!(diagnostic = %d, "a pdf's annotations were not all read on import");
            }
            tracing::debug!(
                inserted = summary.inserted,
                updated = summary.updated,
                unchanged = summary.unchanged,
                "read a pdf's annotations on import"
            );
        }
        Err(e) => {
            tracing::warn!(error = %e, path = %id.path.display(), "a pdf's annotations could not be stored on import");
        }
    }
}

/// A file's mtime in whole unix seconds, as `copy_places.last_mtime` holds it.
pub(crate) fn mtime_of(path: &Path) -> Option<i64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)
}

fn report_for(
    outcome: FileOutcome,
    book_id: i64,
    matched_by: Option<FileMatch>,
    created_book: bool,
    id: &FileIdentity,
    at: PathBuf,
) -> FileImportReport {
    FileImportReport {
        outcome,
        book_id: Some(book_id),
        matched_by,
        created_book,
        sha256: id.sha256.clone(),
        stored_path: Some(at),
        candidates: Vec::new(),
    }
}

/// Dedup level 0: same place with new bytes is the same copy with an updated hash, whatever the
/// ladder would say.
async fn known_place(
    storage: &Storage,
    id: &FileIdentity,
    annotations: Annotations,
) -> Result<Option<FileImportReport>> {
    let at = std::path::absolute(&id.path).unwrap_or_else(|_| id.path.clone());
    let reference = at.display().to_string();
    let Some(copy) = storage.copy_for_place(PlaceKind::Path, &reference).await? else {
        return Ok(None);
    };
    let unchanged = copy
        .places
        .iter()
        .any(|p| p.kind == PlaceKind::Path && p.last_hash.as_deref() == Some(id.sha256.as_str()));
    storage
        .refresh_place(
            PlaceKind::Path,
            &reference,
            Some(&id.sha256),
            mtime_of(&id.path),
        )
        .await?;
    storage.unmiss_place(PlaceKind::Path, &reference).await?;
    storage
        .link_device_book(&id.partial_md5, copy.book_id, LinkedBy::Auto)
        .await?;
    // New bytes at a known place: the refresh recorded the new mtime, so the rescan would skip; read
    // the changes here.
    if !unchanged && annotations == Annotations::Read {
        read_pdf_annotations(storage, copy.book_id, copy.id, id).await;
    }
    Ok(Some(report_for(
        if unchanged {
            FileOutcome::AlreadyOwned
        } else {
            FileOutcome::Stored
        },
        copy.book_id,
        unchanged.then_some(FileMatch::Sha256),
        false,
        id,
        at,
    )))
}

async fn attach_identified(
    storage: &Storage,
    book_id: i64,
    id: &FileIdentity,
    created_book: bool,
    annotations: Annotations,
) -> Result<FileImportReport> {
    if let Some(known) = known_place(storage, id, annotations).await? {
        return Ok(known);
    }
    let at = std::path::absolute(&id.path).unwrap_or_else(|_| id.path.clone());

    // Not skipped just because a caller named a book: repointing known content at another book is what
    // the global primary key refuses.
    let by_hash = storage.copy_for_hash(&id.sha256).await?;
    let owner = storage.book_file(&id.sha256).await?.map(|f| f.book_id);
    if let Some(other) = by_hash.as_ref().map(|c| c.book_id).or(owner)
        && other != book_id
    {
        return Ok(report_for(
            FileOutcome::AlreadyOwned,
            other,
            Some(FileMatch::Sha256),
            false,
            id,
            at,
        ));
    }

    // One file is one copy wherever it sits: same bytes or same KOReader identity means another place
    // of that copy.
    let known_copy = match by_hash {
        Some(c) => Some(c),
        None => match storage
            .copy_for_place(PlaceKind::Reader, &id.partial_md5)
            .await?
        {
            Some(c) if c.book_id == book_id => Some(c),
            _ => None,
        },
    };
    let already = known_copy.is_some() || owner.is_some();
    let copy_id = match known_copy {
        Some(c) => c.id,
        None => {
            storage
                .create_copy(&NewCopy::new(
                    book_id,
                    Some(CopyForm::File),
                    Tenure::Own,
                    LearnedFrom::FileImport,
                ))
                .await?
                .id
        }
    };
    let mut place = NewPlace::new(PlaceKind::Path, at.display().to_string());
    place.last_hash = Some(id.sha256.clone());
    place.last_mtime = mtime_of(&id.path);
    storage.attach_place(copy_id, &place).await?;
    if annotations == Annotations::Read {
        read_pdf_annotations(storage, book_id, copy_id, id).await;
    }

    // `link_device_book`, never `set_device_link`: importing is scan-shaped and must not relabel a link
    // made by hand.
    storage
        .link_device_book(&id.partial_md5, book_id, LinkedBy::Auto)
        .await?;

    // Through `fill_book` (stored row wins): a pdf is one more partial record, so it fills a gap and
    // never overwrites.
    // `None` writes nothing; absence is not zero.
    if let Some(pages) = id.page_count {
        storage
            .fill_book(
                book_id,
                &Book {
                    page_count: Some(pages),
                    ..Default::default()
                },
                Some(crate::storage::Source::Pdf),
            )
            .await?;
    }

    Ok(report_for(
        if already {
            FileOutcome::AlreadyOwned
        } else {
            FileOutcome::Stored
        },
        book_id,
        if already {
            Some(FileMatch::Sha256)
        } else {
            id.matched.map(|(_, m)| m)
        },
        created_book,
        id,
        at,
    ))
}

#[derive(Debug, Clone)]
pub struct KeptCopy {
    pub sha256: String,
    pub stored_path: PathBuf,
    pub already_present: bool,
    pub attached: PlaceAttach,
}

/// Stores a copy's file in the content store and attaches a `store` place; the only way bytes enter
/// the store.
/// Reads the first `path` place present on disk; a copy without one gets
/// `EngineError::InvalidInput`.
pub async fn keep_copy_here(storage: &Storage, files_dir: &Path, copy_id: i64) -> Result<KeptCopy> {
    let copy = storage
        .get_copy(copy_id)
        .await?
        .ok_or_else(|| EngineError::NotFound(format!("copy id {copy_id}")))?;
    let src = copy
        .places
        .iter()
        .filter(|p| p.kind == PlaceKind::Path && p.missing_since.is_none())
        .map(|p| PathBuf::from(&p.reference))
        .find(|p| p.is_file())
        .ok_or_else(|| {
            EngineError::InvalidInput("this copy has no file on this computer to keep".into())
        })?;
    let stored = store(files_dir, &src)?;
    storage
        .add_book_file(&BookFile {
            sha256: stored.sha256.clone(),
            book_id: copy.book_id,
            format: stored.format.clone(),
            original_name: src.file_name().map(|n| n.to_string_lossy().into_owned()),
            size: stored.size,
            added_at: 0,
        })
        .await?;
    let mut place = NewPlace::new(PlaceKind::Store, stored.sha256.clone());
    place.last_hash = Some(stored.sha256.clone());
    let attached = storage.attach_place(copy_id, &place).await?;
    Ok(KeptCopy {
        sha256: stored.sha256,
        stored_path: stored.path,
        already_present: stored.already_present,
        attached,
    })
}

/// Identify, attach to the matching book, and create one only when nothing plausible exists. A
/// near-miss with `new` unset
/// returns `Unmatched` with candidates and writes nothing, to avoid a silent duplicate.
pub async fn import(
    engine: &crate::Engine,
    path: &Path,
    opts: ImportOptions,
) -> Result<FileImportReport> {
    let storage = &engine.storage;
    let id = identify(storage, path).await?;

    if let Some(known) = known_place(storage, &id, Annotations::Read).await? {
        return Ok(known);
    }
    if let Some(c) = storage.copy_for_hash(&id.sha256).await? {
        return attach_identified(storage, c.book_id, &id, false, Annotations::Read).await;
    }
    if let Some((book_id, _)) = id.matched {
        return attach_identified(storage, book_id, &id, false, Annotations::Read).await;
    }
    if !id.candidates.is_empty() && !opts.new {
        return Ok(FileImportReport {
            outcome: FileOutcome::Unmatched,
            book_id: None,
            matched_by: None,
            created_book: false,
            sha256: id.sha256,
            stored_path: None,
            candidates: id.candidates,
        });
    }

    let book_id = create_book_for(engine, &id).await?;
    attach_identified(storage, book_id, &id, true, Annotations::Read).await
}

async fn create_book_for(engine: &crate::Engine, id: &FileIdentity) -> Result<i64> {
    if id.format == "epub" {
        match engine.import_epub(&id.path).await {
            Ok(book) => {
                if let Some(book_id) = book.id {
                    return Ok(book_id);
                }
            }
            // A corrupt epub does not refuse the file; fall through to the filename and warn so an empty book
            // is not mistaken for success.
            Err(e) => tracing::warn!(
                path = %id.path.display(),
                error = %e,
                "epub metadata import failed; creating the book from the filename"
            ),
        }
    }
    let saved = engine
        .save_book(&Book {
            title: id.title.clone(),
            ..Default::default()
        })
        .await?;
    saved
        .id
        .ok_or_else(|| EngineError::Other("a stored book has an id".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn the_hash_is_the_sha256_everyone_elses_tool_prints() {
        let tmp = tmpdir();
        assert_eq!(
            sha256_of(&write(tmp.path(), "empty", b"")).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_of(&write(tmp.path(), "abc", b"abc")).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn hashing_streams_rather_than_reading_once() {
        let tmp = tmpdir();
        let big: Vec<u8> = (0..COPY_BUF * 3 + 17).map(|i| (i % 251) as u8).collect();
        let path = write(tmp.path(), "big.epub", &big);

        let mut expect = Sha256::new();
        expect.update(&big);
        assert_eq!(sha256_of(&path).unwrap(), hex(&expect.finalize()));

        let files = tmp.path().join("files");
        let stored = store(&files, &path).unwrap();
        assert_eq!(stored.size, big.len() as i64);
        assert_eq!(std::fs::read(&stored.path).unwrap(), big);
    }

    #[test]
    fn the_address_shards_on_the_first_byte_and_keeps_the_format() {
        let p = content_path(Path::new("/data/files"), "ab12cd", "epub");
        assert_eq!(p, Path::new("/data/files/ab/ab12cd.epub"));
    }

    #[test]
    fn a_hostile_extension_cannot_leave_the_store() {
        let cases = [
            ("book.epub", "epub"),
            ("book.EPUB", "epub"),
            ("book.AZW3", "azw3"),
            ("book", "bin"),
            ("book.", "bin"),
            (".hidden", "bin"),
            // `Path` already yields no extension here; the filter is the second line of defence.
            ("book.../etc/passwd", "bin"),
            ("archive.tar.gz", "gz"),
            ("book.e p u b", "epub"),
            ("book.épub", "pub"),
        ];
        for (name, want) in cases {
            let got = format_of(Path::new(name));
            assert_eq!(got, want, "{name}");
            let p = content_path(Path::new("/data/files"), "abcd", &got);
            assert!(
                p.starts_with("/data/files/ab"),
                "{name} escaped the store: {}",
                p.display()
            );
            assert_eq!(p.components().count(), 5, "{name} added path components");
        }
    }

    #[test]
    fn a_stored_file_is_named_for_what_is_actually_in_it() {
        let tmp = tmpdir();
        let files = tmp.path().join("files");
        let src = write(tmp.path(), "Middlemarch.epub", b"not really an epub");

        let stored = store(&files, &src).unwrap();
        assert!(!stored.already_present);
        assert_eq!(stored.path, content_path(&files, &stored.sha256, "epub"));
        assert_eq!(sha256_of(&stored.path).unwrap(), stored.sha256);
    }

    #[test]
    fn the_same_bytes_are_stored_once_however_they_are_named() {
        let tmp = tmpdir();
        let files = tmp.path().join("files");
        let a = write(tmp.path(), "Middlemarch.epub", b"the same bytes");
        let b = write(tmp.path(), "middlemarch (1).epub", b"the same bytes");

        let first = store(&files, &a).unwrap();
        let second = store(&files, &b).unwrap();
        assert_eq!(first.sha256, second.sha256);
        assert_eq!(first.path, second.path);
        assert!(!first.already_present);
        assert!(
            second.already_present,
            "the second copy must be recognised, not rewritten"
        );
    }

    #[test]
    fn an_import_leaves_no_temp_files_behind() {
        let tmp = tmpdir();
        let files = tmp.path().join("files");
        let src = write(tmp.path(), "a.epub", b"aaa");
        store(&files, &src).unwrap();
        store(&files, &src).unwrap();

        let strays: Vec<PathBuf> = std::fs::read_dir(&files)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.is_file())
            .collect();
        assert!(strays.is_empty(), "left behind: {strays:?}");
    }

    #[test]
    fn a_failed_copy_stores_nothing() {
        let tmp = tmpdir();
        let files = tmp.path().join("files");
        let err = store(&files, &tmp.path().join("does-not-exist.epub")).unwrap_err();
        assert!(matches!(err, EngineError::Io(_)), "{err:?}");

        let left: Vec<PathBuf> = std::fs::read_dir(&files)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert!(left.is_empty(), "left behind: {left:?}");
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn any_bytes_round_trip_to_their_own_address(bytes in prop::collection::vec(any::<u8>(), 0..200_000)) {
                let tmp = tempfile::tempdir().unwrap();
                let files = tmp.path().join("files");
                let src = tmp.path().join("in.epub");
                std::fs::write(&src, &bytes).unwrap();

                let stored = store(&files, &src).unwrap();
                prop_assert_eq!(stored.size, bytes.len() as i64);
                prop_assert_eq!(std::fs::read(&stored.path).unwrap(), bytes);
                prop_assert_eq!(sha256_of(&stored.path).unwrap(), stored.sha256.clone());
                prop_assert_eq!(stored.path, content_path(&files, &stored.sha256, "epub"));
            }

            #[test]
            fn a_format_is_always_one_safe_component(name in ".{0,40}") {
                let format = format_of(Path::new(&name));
                prop_assert!(!format.is_empty());
                prop_assert!(format.chars().all(|c| c.is_ascii_alphanumeric()));
                let p = content_path(Path::new("/store"), "abcdef", &format);
                prop_assert!(p.starts_with("/store/ab"));
                prop_assert_eq!(p.components().count(), 4);
            }
        }
    }
}
