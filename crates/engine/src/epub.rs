//! The app's view of an EPUB over `epubkit`: `normalize_isbn` decides what an ISBN is; errors
//! become `EngineError::Epub`.

use std::collections::BTreeMap;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

use epubkit::Package;

use crate::book::normalize_isbn;
use crate::error::{EngineError, Result};

/// Survivable `epubkit` warnings are logged at `debug!` as kinds and a count only: titles, text
/// and paths are private reading.
pub(crate) struct EpubFile {
    package: Package,
}

impl EpubFile {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        Package::open(path)
            .map(|package| EpubFile { package })
            .map_err(|e| EngineError::Epub(format!("{}: {e}", path.display())))
    }

    pub(crate) fn info(&self) -> EpubInfo {
        // Flat view read exactly as the old `epub` crate's metadata map: stored books were imported with
        // these values.
        let md = self.package.metadata();
        EpubInfo {
            isbn: md.values("identifier").find_map(normalize_isbn),
            title: md.values("title").next().map(str::to_string),
            authors: md.values("creator").map(str::to_string).collect(),
            language: md.values("language").next().map(str::to_string),
        }
    }

    pub(crate) fn cover(&mut self, images_dir: &Path) -> Result<Option<crate::images::CoverFile>> {
        let Some(cover) = self.package.cover() else {
            return Ok(None);
        };
        crate::images::store_cover(&cover.bytes, images_dir).map(Some)
    }

    pub(crate) fn toc(&mut self) -> Vec<TocEntry> {
        // NCX first when it yields anything, then the EPUB 3 navigation document; `epubkit`'s own `toc()`
        // prefers the reverse.
        let mut nodes = self.package.ncx_toc();
        if nodes.is_empty() {
            nodes = self.package.nav_toc();
        }
        epubkit::flatten(&nodes)
            .map(|(depth, node)| {
                // The spine is keyed on resources, which carry no fragment: look up by the document and keep the
                // fragment in `target`.
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

pub fn epub_info(path: &Path) -> Result<EpubInfo> {
    Ok(EpubFile::open(path)?.info())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableOfContents {
    pub sha256: String,
    /// In reading order; empty means the file has no navigable TOC.
    pub entries: Vec<TocEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TocEntry {
    pub label: String,
    pub depth: usize,
    /// Resource relative to the epub root, fragment included, as the book wrote it; empty for an
    /// unlinked heading.
    pub target: String,
    /// `None` when the target is mid-document or not in the spine. Deliberately not a page number.
    pub spine_index: Option<usize>,
}

/// Derived on demand and never stored: a pure function of the file, so a stored copy would go
/// stale and a provenance stamp for it would be false. An empty list means no navigable TOC, not
/// an error. Does not resolve a `pos0` (a cre-engine xpointer).
pub fn table_of_contents(path: &Path) -> Result<Vec<TocEntry>> {
    Ok(EpubFile::open(path)?.toc())
}

/// Stores the embedded cover named and typed by its bytes via `store_cover`, not by title or the
/// OPF's declared mime.
pub fn extract_cover(path: &Path, images_dir: &Path) -> Result<Option<crate::images::CoverFile>> {
    EpubFile::open(path)?.cover(images_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generated_fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/koreader/synthetic/Gen-Isbn-Match.epub")
    }

    fn toc_fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/koreader/synthetic/Gen-Toc-Chapters.epub")
    }

    fn real_epub() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/epub/55.epub")
    }

    #[test]
    fn reads_metadata_from_the_generated_epub() {
        let info = epub_info(&generated_fixture()).expect("generated fixture must parse");
        assert_eq!(info.title.as_deref(), Some("The ISBN Matched Book"));
        assert_eq!(info.language.as_deref(), Some("en"));
        assert_eq!(info.isbn.as_deref(), Some("9780316769488"));
    }

    /// `import_epub` holds the handle across an await, so it must be `Send`.
    #[test]
    fn the_handle_can_cross_an_await() {
        fn send<T: Send>() {}
        send::<EpubFile>();
    }

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
            // The contract is "does not panic"; some corruptions still leave a readable OPF.
            let _ = epub_info(&path);
            let _ = extract_cover(&path, &tmp.path().join("images"));
            let _ = table_of_contents(&path);
        }
    }

    /// Reading order is `playOrder`, not document order; the fixture declares chapter three before two
    /// to catch that.
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
                ("Two, part two", 1, Some(1)),
                ("Three: The End", 0, Some(2)),
            ]
        );
        assert!(
            toc[2].target.ends_with("ch2.xhtml#part-two"),
            "{:?}",
            toc[2].target
        );
    }

    #[test]
    fn an_epub_without_a_toc_answers_with_no_chapters() {
        assert!(
            table_of_contents(&generated_fixture())
                .expect("a valid epub without an ncx still parses")
                .is_empty()
        );
    }

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
        if let Some(isbn) = &info.isbn {
            assert!(normalize_isbn(isbn).is_some());
        }
    }

    #[test]
    fn extracts_cover_from_fixture() {
        let dir = tempfile::tempdir().unwrap();
        let c = extract_cover(&real_epub(), dir.path())
            .unwrap()
            .expect("an illustrated Gutenberg epub carries its cover");
        assert!(c.path.exists());
        assert!(std::fs::metadata(&c.path).unwrap().len() > 0);
        let stem = c.path.file_stem().unwrap().to_string_lossy().to_string();
        assert_eq!(stem.len(), 64, "{stem}");
    }
}
