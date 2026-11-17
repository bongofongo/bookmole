//! The app's view of an EPUB, over the standalone [`epubkit`] reader.
//!
//! `epubkit` knows nothing of bookmole; this file is where its answers
//! become ours — [`normalize_isbn`] decides what an ISBN is, and its errors
//! become [`EngineError::Epub`].

use std::collections::BTreeMap;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

use epubkit::Package;

use crate::book::normalize_isbn;
use crate::error::{EngineError, Result};

/// One opened EPUB: parsed once, then asked for its metadata, its cover and
/// its table of contents through the same handle — so an import that wants
/// two of them opens and parses the file once, not twice.
///
/// What `epubkit` found wrong but survivable (an undeclared entity, a
/// manifest item missing from the archive, a chapter list it could not read)
/// has no return path here that `crates/api` would not have to change for,
/// so it is logged once, when the handle is dropped: at `debug!`, as warning
/// **kinds and a count only**. The kinds carry none of the book's titles,
/// labels, text or paths, which are the user's private reading.
pub(crate) struct EpubFile {
    package: Package,
}

impl EpubFile {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        Package::open(path)
            .map(|package| EpubFile { package })
            .map_err(|e| EngineError::Epub(format!("{}: {e}", path.display())))
    }

    /// The metadata an import reads. Scans ALL identifier entries for the
    /// first one that validates as an ISBN (identifiers are often UUIDs or
    /// `urn:isbn:` forms).
    pub(crate) fn info(&self) -> EpubInfo {
        // The flat view, read exactly as the old `epub` crate's metadata map
        // was: these are the values every stored book was imported with.
        let md = self.package.metadata();
        EpubInfo {
            isbn: md.values("identifier").find_map(normalize_isbn),
            title: md.values("title").next().map(str::to_string),
            authors: md.values("creator").map(str::to_string).collect(),
            language: md.values("language").next().map(str::to_string),
        }
    }

    /// The embedded cover, stored into `images_dir`; see [`extract_cover`].
    pub(crate) fn cover(&mut self, images_dir: &Path) -> Result<Option<crate::images::CoverFile>> {
        let Some(cover) = self.package.cover() else {
            return Ok(None);
        };
        crate::images::store_cover(&cover.bytes, images_dir).map(Some)
    }

    /// The chapter list; see [`table_of_contents`].
    pub(crate) fn toc(&mut self) -> Vec<TocEntry> {
        // The NCX first, when it yields anything — every book that had a
        // chapter list keeps exactly that list — then the EPUB 3 navigation
        // document. (`epubkit`'s own `toc()` prefers the navigation document;
        // this order is the app's, for parity.)
        let mut nodes = self.package.ncx_toc();
        if nodes.is_empty() {
            nodes = self.package.nav_toc();
        }
        epubkit::flatten(&nodes)
            .map(|(depth, node)| {
                // The spine is keyed on resources, and a resource never
                // carries the fragment an entry may point at
                // (`ch2.xhtml#part-two`). Look it up by the document, and
                // keep the fragment in `target` where it belongs.
                let spine_index = node
                    .path
                    .as_deref()
                    .and_then(|p| self.package.spine_index_of_path(p));
                TocEntry {
                    label: node.label.clone(),
                    depth,
                    target: node
                        .target()
                        .or_else(|| node.href.clone())
                        .unwrap_or_default(),
                    spine_index,
                }
            })
            .collect()
    }
}

impl Drop for EpubFile {
    fn drop(&mut self) {
        let warnings = self.package.warnings();
        if warnings.is_empty() {
            return;
        }
        let mut kinds: BTreeMap<&'static str, usize> = BTreeMap::new();
        for w in warnings {
            *kinds.entry(w.kind()).or_default() += 1;
        }
        let kinds = kinds
            .iter()
            .map(|(k, n)| format!("{k}={n}"))
            .collect::<Vec<_>>()
            .join(",");
        tracing::debug!(count = warnings.len(), %kinds, "epub read with warnings");
    }
}

#[derive(Debug, Default)]
pub struct EpubInfo {
    pub isbn: Option<String>,
    pub title: Option<String>,
    pub authors: Vec<String>,
    pub language: Option<String>,
}

/// Read epub metadata. Scans ALL identifier entries for the first one that
/// validates as an ISBN (identifiers are often UUIDs or urn:isbn: forms).
pub fn epub_info(path: &Path) -> Result<EpubInfo> {
    Ok(EpubFile::open(path)?.info())
}

/// A book's chapter list, and **which file said so**.
///
/// The sha256 travels with the entries because nothing is stored: a caller that
/// wants to cache this — a TUI redrawing per keypress — keys on exactly that,
/// and a caller comparing two answers can tell whether the book got a better
/// file rather than whether the parser changed its mind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableOfContents {
    pub sha256: String,
    /// In reading order. Empty means the file carries no navigable TOC.
    pub entries: Vec<TocEntry>,
}

/// One line of a book's table of contents.
///
/// Flat with a `depth` rather than a tree of children: every consumer this has
/// (a chapter list, naming the chapter a position falls in) walks it in reading
/// order, and a tree makes the common case — "the entries, in order" — the
/// awkward one. The nesting is not lost; it is a column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TocEntry {
    /// The chapter's name, as the book gives it.
    pub label: String,
    /// 0 for a top-level entry, 1 for a section inside one, and so on.
    pub depth: usize,
    /// The resource this entry points at, relative to the epub root, fragment
    /// included (`OEBPS/ch2.xhtml#part-two`). Kept as text because it is the
    /// book's own name for the place, not a path on this machine. An absolute
    /// URL is kept as the book wrote it; an unlinked heading (an EPUB 3 `span`
    /// grouping the entries below it) has an empty target.
    pub target: String,
    /// Where `target` sits in the spine, when it names a spine item at all.
    ///
    /// `None` is ordinary: a TOC may point into the middle of a document, or at
    /// a resource the spine does not carry. It is deliberately **not** a page
    /// number — see [`table_of_contents`].
    pub spine_index: Option<usize>,
}

/// The book's own table of contents, read from the file.
///
/// **Derived on demand, never stored, and that is the decision this carries.**
/// The file is content-addressed in `files_dir`, so re-reading it is cheap and
/// is always current; a stored copy is a *second answer* that goes stale the
/// moment a better file is attached to the same book, and nothing would notice.
/// Three more reasons, each of which outlived the first:
///
/// - **A TOC is not a fact with an origin.** Every other column added by
///   migration `0013` is a claim by a *source* — a provider said this — and gets
///   a `field_provenance` row saying who. A chapter list is a pure function of a
///   sha256, so a stamp naming `epub` as its source would be attributing a
///   value to a party that never claimed anything, and `rb set` could not
///   meaningfully let the user own it. A column whose provenance is a lie is
///   worse than a column that does not exist.
/// - **It cannot be corrected, so it must not be persisted.** A stored field a
///   provider can write and a user cannot override is not allowed anywhere in
///   the schema; the honest way out is a field nobody writes.
/// - **The parse gets better.** It did, on 2026-11-12: this used to read
///   `toc.ncx` only, by the rules of the `epub =2.1.4` crate it replaced, so an
///   EPUB 3 book with just a `nav` document came back empty and any `navPoint`
///   without a `playOrder` was silently dropped. Now the NCX is used when it
///   yields at least one entry — every book that had a chapter list keeps
///   exactly that list — and the EPUB 3 navigation document otherwise; a
///   `navPoint` with no `playOrder` is kept, and a level where any sibling
///   lacks one stays in document order. Derived, every book got the better
///   answer that day. Stored, the fleet would have kept the worse one until
///   something invalidated a cache nobody wrote.
///
/// If this ever becomes hot — a TUI redrawing a chapter list per keypress — the
/// cache keys on `book_files.sha256`, which is already the primary key there.
/// Adding a cache later is cheap; removing a stored copy later is a migration.
///
/// **An empty list is an answer**: this file carries no navigable TOC. It is
/// not an error, and no caller should read it as one.
///
/// **It does not resolve a `pos0`.** A `pos0` is a cre-engine (KOReader's
/// renderer) xpointer, and resolving one means reimplementing enough of that
/// engine to agree with it. Naming a chapter is a smaller claim
/// and it still needs a locator; the locator is the excerpt view's text search,
/// not this.
pub fn table_of_contents(path: &Path) -> Result<Vec<TocEntry>> {
    Ok(EpubFile::open(path)?.toc())
}

/// Extract the embedded cover image into `images_dir`, measured and named by
/// content like every other cover.
///
/// **This used to name the file `slugify(title).ext`**, which collides on two
/// editions of one title — the same class of bug as the Google Books one, minus
/// the URL. It also trusted the OPF's declared mime type for the extension,
/// which is an epub author's claim about their own file rather than a reading
/// of it. Both go through [`crate::images::store_cover`] now, which hashes the
/// bytes and takes the extension from what they actually are.
pub fn extract_cover(path: &Path, images_dir: &Path) -> Result<Option<crate::images::CoverFile>> {
    EpubFile::open(path)?.cover(images_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real, committed epub — generated by `cargo run -p corpus --
    /// gen-synthetic`, ~2 KB, with a known ISBN in its OPF. Unconditional, so
    /// these tests actually run on every machine and in CI.
    fn generated_fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/koreader/synthetic/Gen-Isbn-Match.epub")
    }

    /// The other committed epub, whose whole reason to exist is its `toc.ncx`
    /// (`cargo run -p corpus -- gen-synthetic`). Unconditional, like its
    /// neighbour: a TOC test that only runs on the machine with `epubs/` is a
    /// TOC test that does not run.
    fn toc_fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/koreader/synthetic/Gen-Toc-Chapters.epub")
    }

    /// A real book from the committed corpus: *The Wonderful Wizard of Oz*,
    /// Project Gutenberg 55, kept there for its real cover image. These tests
    /// used to read a gitignored drop-in and print `SKIPPED:` on every machine
    /// without one; the corpus is committed, so they run everywhere.
    fn real_epub() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/epub/55.epub")
    }

    #[test]
    fn reads_metadata_from_the_generated_epub() {
        let info = epub_info(&generated_fixture()).expect("generated fixture must parse");
        assert_eq!(info.title.as_deref(), Some("The ISBN Matched Book"));
        assert_eq!(info.language.as_deref(), Some("en"));
        // The OPF carries a uuid identifier *before* the isbn one; picking the
        // ISBN out means scanning them all rather than taking the first.
        assert_eq!(info.isbn.as_deref(), Some("9780316769488"));
    }

    /// `import_epub` holds one handle across its provider lookup, so the
    /// handle must be `Send` or the import's future would not be.
    #[test]
    fn the_handle_can_cross_an_await() {
        fn send<T: Send>() {}
        send::<EpubFile>();
    }

    /// One handle answers all three questions, in any order.
    #[test]
    fn one_handle_reads_info_cover_and_toc() {
        let tmp = tempfile::tempdir().unwrap();
        let mut file = EpubFile::open(&toc_fixture()).unwrap();
        let info = file.info();
        assert!(info.title.is_some());
        let toc = file.toc();
        assert_eq!(toc.len(), 4);
        let _ = file.cover(tmp.path()).unwrap();
        assert_eq!(file.toc(), toc, "a second read gives the same answer");
    }

    #[test]
    fn a_nonexistent_epub_is_an_error_not_a_panic() {
        let err = epub_info(Path::new("/definitely/not/here.epub")).unwrap_err();
        assert!(matches!(err, EngineError::Epub(_)), "{err:?}");
    }

    /// Users import arbitrary files. A corrupt one must be rejected, never
    /// panic the app.
    #[test]
    fn corrupt_epubs_are_rejected_rather_than_panicking() {
        let tmp = tempfile::tempdir().unwrap();
        let good = std::fs::read(generated_fixture()).unwrap();

        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("empty", Vec::new()),
            ("not a zip", b"this is plainly not a zip archive".to_vec()),
            ("truncated zip", good[..good.len() / 2].to_vec()),
            ("header only", good[..4].to_vec()),
            ("zip with garbage tail", {
                let mut v = good.clone();
                v.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
                v
            }),
            ("nulled central directory", {
                let mut v = good.clone();
                let n = v.len();
                for b in v.iter_mut().skip(n.saturating_sub(64)) {
                    *b = 0;
                }
                v
            }),
        ];

        for (name, bytes) in cases {
            let path = tmp.path().join(format!("{}.epub", name.replace(' ', "-")));
            std::fs::write(&path, &bytes).unwrap();
            // The contract is "does not panic". Either outcome is acceptable —
            // some corruptions still leave a readable OPF.
            let _ = epub_info(&path);
            let _ = extract_cover(&path, &tmp.path().join("images"));
            let _ = table_of_contents(&path);
        }
    }

    /// The chapter list, flattened, in **reading order** — which is
    /// `playOrder`, not the order the `navPoint`s are written in. The fixture
    /// declares chapter three before chapter two precisely so that a parse that
    /// took document order would fail here rather than on somebody's book.
    #[test]
    fn reads_the_chapter_list_from_the_generated_epub() {
        let toc = table_of_contents(&toc_fixture()).expect("generated fixture must parse");
        let shape: Vec<(&str, usize, Option<usize>)> = toc
            .iter()
            .map(|e| (e.label.as_str(), e.depth, e.spine_index))
            .collect();
        assert_eq!(
            shape,
            vec![
                ("One: The Beginning", 0, Some(0)),
                ("Two: The Middle", 0, Some(1)),
                // Nested inside its parent, and pointing at a fragment of the
                // *same* document — so it shares chapter two's spine position.
                ("Two, part two", 1, Some(1)),
                ("Three: The End", 0, Some(2)),
            ]
        );
        // The fragment survives in the target: it is the book's own name for
        // the place, and dropping it would make two entries indistinguishable.
        assert!(
            toc[2].target.ends_with("ch2.xhtml#part-two"),
            "{:?}",
            toc[2].target
        );
    }

    /// An epub with no `toc.ncx` is not a broken epub. It answers "no navigable
    /// table of contents", which is an empty list rather than an error — the
    /// other committed fixture is exactly that file.
    #[test]
    fn an_epub_without_a_toc_answers_with_no_chapters() {
        assert!(
            table_of_contents(&generated_fixture())
                .expect("a valid epub without an ncx still parses")
                .is_empty()
        );
    }

    /// A TOC read off a **real** book, which the synthetic one cannot stand in
    /// for: real ncx files nest three deep, point at fragments, and carry
    /// labels with entities in them.
    #[test]
    fn reads_a_chapter_list_from_a_real_epub() {
        let path = real_epub();
        let toc = table_of_contents(&path).unwrap();
        assert!(!toc.is_empty(), "a real epub has chapters");
        for e in &toc {
            assert!(!e.label.is_empty(), "a nameless chapter is not a chapter");
            assert!(!e.target.is_empty());
        }
        assert!(
            toc.iter().any(|e| e.spine_index.is_some()),
            "a TOC none of whose entries names a spine item is a parse that missed"
        );
    }

    /// A small book built in `dir`: the package document at
    /// `OEBPS/content.opf` with one chapter, `OEBPS/ch1.xhtml`, plus `extra`.
    fn built_book(
        dir: &Path,
        name: &str,
        metadata: &str,
        manifest: &str,
        spine_attrs: &str,
        extra: &[(&str, &[u8])],
    ) -> PathBuf {
        use std::io::Write;
        let container = r#"<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#;
        let opf = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="uid"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/">{metadata}</metadata><manifest><item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/>{manifest}</manifest><spine{spine_attrs}><itemref idref="ch1"/></spine></package>"#
        );
        let mut entries: Vec<(&str, &[u8])> = vec![
            ("mimetype", b"application/epub+zip"),
            ("META-INF/container.xml", container.as_bytes()),
            ("OEBPS/content.opf", opf.as_bytes()),
            ("OEBPS/ch1.xhtml", b"<html/>"),
        ];
        entries.extend_from_slice(extra);
        let path = dir.join(name);
        let mut w = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        for (entry, bytes) in entries {
            w.start_file(entry, opts).unwrap();
            w.write_all(bytes).unwrap();
        }
        w.finish().unwrap();
        path
    }

    fn rows(toc: &[TocEntry]) -> Vec<(&str, usize, &str, Option<usize>)> {
        toc.iter()
            .map(|e| (e.label.as_str(), e.depth, e.target.as_str(), e.spine_index))
            .collect()
    }

    /// `&nbsp;` in a title is U+00A0, the character it names, so an import of
    /// such a book stores U+00A0. (The old `epub` crate predefined `nbsp` as a
    /// plain space.)
    #[test]
    fn an_nbsp_in_the_title_is_the_no_break_space() {
        let tmp = tempfile::tempdir().unwrap();
        let p = built_book(
            tmp.path(),
            "nbsp.epub",
            "<dc:title>A&nbsp;B</dc:title>",
            "",
            "",
            &[],
        );
        assert_eq!(epub_info(&p).unwrap().title.as_deref(), Some("A\u{a0}B"));
    }

    /// An EPUB 3 book with only a navigation document has chapters: its links
    /// resolve against the nav's own directory, the target keeps the `..` as
    /// written while the spine position is found by folding it, and an
    /// unlinked heading is a row with an empty target. (The old crate read
    /// only the NCX, so this book had no chapters.)
    #[test]
    fn a_nav_only_book_has_chapters() {
        let tmp = tempfile::tempdir().unwrap();
        let nav = r#"<?xml version="1.0" encoding="utf-8"?><html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="../ch1.xhtml">One</a><ol><li><a href="../ch1.xhtml#s">One, s</a></li></ol></li><li><span>Heading</span></li></ol></nav></body></html>"#;
        let p = built_book(
            tmp.path(),
            "navonly.epub",
            "<dc:title>T</dc:title>",
            r#"<item id="nav" href="nav/nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>"#,
            "",
            &[("OEBPS/nav/nav.xhtml", nav.as_bytes())],
        );
        assert_eq!(
            rows(&table_of_contents(&p).unwrap()),
            vec![
                ("One", 0, "OEBPS/nav/../ch1.xhtml", Some(0)),
                ("One, s", 1, "OEBPS/nav/../ch1.xhtml#s", Some(0)),
                ("Heading", 0, "", None),
            ]
        );
    }

    /// When the spine names an NCX that yields entries, it wins over a
    /// navigation document — every book that had a chapter list keeps exactly
    /// that list. A link with `..` keeps its text in `target`, and its spine
    /// position is found by folding. (The old crate missed that position.)
    #[test]
    fn the_ncx_wins_over_the_nav_and_a_dotdot_link_finds_its_spine_item() {
        let tmp = tempfile::tempdir().unwrap();
        let ncx = r#"<?xml version="1.0"?><ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"><navMap><navPoint playOrder="1"><navLabel><text>From the NCX</text></navLabel><content src="../OEBPS/ch1.xhtml"/></navPoint></navMap></ncx>"#;
        let nav = r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="ch1.xhtml">From the nav</a></li></ol></nav></body></html>"#;
        let p = built_book(
            tmp.path(),
            "both.epub",
            "<dc:title>T</dc:title>",
            r#"<item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>"#,
            r#" toc="ncx""#,
            &[
                ("OEBPS/toc.ncx", ncx.as_bytes()),
                ("OEBPS/nav.xhtml", nav.as_bytes()),
            ],
        );
        assert_eq!(
            rows(&table_of_contents(&p).unwrap()),
            vec![("From the NCX", 0, "OEBPS/../OEBPS/ch1.xhtml", Some(0))]
        );
    }

    #[test]
    fn reads_metadata_from_fixture() {
        let info = epub_info(&real_epub()).unwrap();
        assert!(info.title.is_some());
        // If an ISBN was found it must be a validated, normalized one.
        if let Some(isbn) = &info.isbn {
            assert!(normalize_isbn(isbn).is_some());
        }
    }

    #[test]
    fn extracts_cover_from_fixture() {
        let dir = std::env::temp_dir().join(format!("rb-epub-test-{}", std::process::id()));
        let c = extract_cover(&real_epub(), &dir)
            .unwrap()
            .expect("an illustrated Gutenberg epub carries its cover");
        assert!(c.path.exists());
        assert!(std::fs::metadata(&c.path).unwrap().len() > 0);
        // Named by content, not by `slugify(title)` — so two editions of
        // one book no longer overwrite each other's jacket.
        let stem = c.path.file_stem().unwrap().to_string_lossy().to_string();
        assert_eq!(stem.len(), 64, "{stem}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
