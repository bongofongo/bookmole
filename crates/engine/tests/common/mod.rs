//! Shared offline scaffolding for the integration tests: in-memory SQLite plus a `TempDir` vault.

// Compiled into every test binary, so helpers unused in one are dead code there; clippy `-D
// warnings` needs the allow.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use bookmole::{Book, Engine, EngineConfig, NewHighlight};

pub const SYNTHETIC: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/koreader/synthetic"
);

/// The caller must hold the `TempDir`.
pub async fn engine() -> (tempfile::TempDir, Engine) {
    engine_with_calibre(None).await
}

pub async fn engine_with_calibre(bin_dir: Option<PathBuf>) -> (tempfile::TempDir, Engine) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = EngineConfig::in_memory(tmp.path());
    config.calibre_bin_dir = bin_dir;
    let engine = Engine::open(config).await.expect("engine opens");
    (tmp, engine)
}

pub fn book(title: &str) -> Book {
    Book {
        title: Some(title.into()),
        authors: vec!["George Eliot".into()],
        ..Default::default()
    }
}

pub async fn seed_book(engine: &Engine, title: &str) -> i64 {
    engine
        .save_book(&book(title))
        .await
        .expect("save")
        .id
        .expect("a stored book has an id")
}

/// `when` is a `%Y-%m-%d %H:%M:%S` device timestamp, which reading-window attribution matches on.
pub fn highlight(text: &str, when: &str) -> NewHighlight {
    NewHighlight {
        text: text.into(),
        chapter: Some("Ch 1".into()),
        page: Some(12),
        pos0: Some(format!("/body/p[1]/text().{text}")),
        pos1: None,
        ko_datetime: Some(when.into()),
        ko_datetime_updated: None,
        color: Some("yellow".into()),
        note: None,
        source: "koreader".into(),
    }
}

/// Always a copy: a scan rewrites mtimes and prunes rows, so running one on the committed
/// fixture would edit the repository.
pub fn place(device_root: &Path, fixture: &str, as_name: &str) -> PathBuf {
    let src = Path::new(SYNTHETIC).join(fixture);
    let dst = device_root.join(as_name);
    std::fs::create_dir_all(&dst).unwrap();
    let mut sidecar = None;
    for entry in std::fs::read_dir(&src).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        std::fs::copy(entry.path(), dst.join(&name)).unwrap();
        let name = name.to_string_lossy().into_owned();
        if name.starts_with("metadata.") && name.ends_with(".lua") {
            sidecar = Some(dst.join(name));
        }
    }
    sidecar.unwrap_or_else(|| panic!("{fixture} has no sidecar"))
}

/// Bumps the mtime explicitly: `sidecar_seen` skips the re-parse when `(size, mtime)` is
/// unchanged, and a test writes edits faster than the clock moves.
pub fn rewrite_sidecar(path: &Path, edit: impl FnOnce(String) -> String) {
    let before = std::fs::read_to_string(path).expect("read sidecar");
    std::fs::write(path, edit(before)).expect("write sidecar");
    let f = std::fs::File::options()
        .write(true)
        .open(path)
        .expect("open sidecar");
    let bumped = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
    f.set_modified(bumped).expect("bump mtime");
}

/// Prints `SKIPPED:` and honours `BOOKMOLE_REQUIRE_FIXTURES`, so a missing fixture fails the
/// nightly job instead of passing silently.
pub fn skipped(reason: &str, hint: &str) -> bool {
    if std::env::var("BOOKMOLE_REQUIRE_FIXTURES").is_ok() {
        panic!("REQUIRE_FIXTURES is set but {reason}");
    }
    eprintln!("SKIPPED: {reason} — {hint}");
    true
}

/// An epub with no ISBN, so `import_epub` makes no provider lookup.
pub fn write_isbnless_epub(path: &std::path::Path, title: &str) {
    write_isbnless_epub_by(path, title, "A Test Author")
}

/// The matcher reads authors, so "the same book from elsewhere" must carry the same author on
/// both sides.
pub fn write_isbnless_epub_by(path: &std::path::Path, title: &str, author: &str) {
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    // `mimetype` must be first and STORED per the epub spec.
    zip.start_file(
        "mimetype",
        SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
    )
    .unwrap();
    zip.write_all(b"application/epub+zip").unwrap();

    let opts = SimpleFileOptions::default();
    zip.start_file("META-INF/container.xml", opts).unwrap();
    zip.write_all(
        br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>
"#,
    )
    .unwrap();

    zip.start_file("OEBPS/content.opf", opts).unwrap();
    zip.write_all(
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="bookid">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="bookid">urn:uuid:00000000-0000-4000-8000-000000000000</dc:identifier>
    <dc:title>{title}</dc:title>
    <dc:creator>{author}</dc:creator>
    <dc:language>en</dc:language>
  </metadata>
  <manifest>
    <item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine>
    <itemref idref="ch1"/>
  </spine>
</package>
"#
        )
        .as_bytes(),
    )
    .unwrap();

    zip.start_file("OEBPS/ch1.xhtml", opts).unwrap();
    zip.write_all(
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>{title}</title></head>
<body><h1>{title}</h1><p>A single paragraph, so the spine is not empty.</p></body></html>
"#
        )
        .as_bytes(),
    )
    .unwrap();
    zip.finish().unwrap();
}
