//! Replay the `epubkit_package` fuzz seeds on **stable**, on every test run.
//!
//! The nightly fuzzer (`fuzz/fuzz_targets/epubkit_package.rs`) finds things;
//! every input it finds gets minimized into `fuzz/seeds/epubkit_package/` and
//! is guarded here from then on, through the same calls the target makes. The
//! older `epub_info` seeds are EPUB bytes too, so they are replayed through
//! the same calls as well.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn seed_dir(target: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fuzz/seeds")
        .join(target)
}

fn seeds(target: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(seed_dir(target)) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    out.sort();
    out
}

/// Exactly what the fuzz target does with its bytes.
fn exercise(data: &[u8]) {
    let Ok(mut book) = epubkit::Package::from_bytes(data) else {
        return;
    };
    let md = book.metadata();
    let _ = (
        md.main_title(),
        md.publication_date(),
        md.unique_identifier(),
    );
    for e in md.entries() {
        let _ = (&e.key, &e.value);
    }
    let _ = book.cover();
    for nav in [book.nav(), book.ncx()].into_iter().flatten() {
        for list in [&nav.toc, &nav.landmarks, &nav.page_list] {
            for (_, node) in epubkit::flatten(list) {
                if let Some(p) = node.path.as_deref() {
                    let _ = book.spine_index_of_path(p);
                }
            }
        }
    }
    let _ = book.toc();
    for i in 0..book.spine().items.len() {
        if let Ok(blocks) = book.spine_text(i) {
            for b in blocks {
                assert!(!b.text.is_empty(), "a block is never empty");
            }
        }
    }
    for w in book.warnings() {
        let _ = (w.kind(), w.to_string());
    }
}

/// Every seed is opened or refused, never a panic, and within a bound.
#[test]
fn every_epubkit_package_seed_is_survivable() {
    let own = seeds("epubkit_package");
    assert!(
        own.len() >= 3,
        "fuzz/seeds/epubkit_package/ should be committed with its seeds"
    );
    let all: Vec<PathBuf> = own.into_iter().chain(seeds("epub_info")).collect();
    let started = Instant::now();
    for path in &all {
        let bytes = std::fs::read(path).expect("seed is readable");
        let one = Instant::now();
        exercise(&bytes);
        assert!(
            one.elapsed() < Duration::from_secs(30),
            "{} took {:?}",
            path.display(),
            one.elapsed()
        );
    }
    eprintln!("replayed {} seeds in {:?}", all.len(), started.elapsed());
}

/// The committed seeds open and have something in them — a seed set that
/// fails at the zip layer exercises nothing past it.
#[test]
fn the_generated_seed_reaches_every_layer() {
    let bytes = std::fs::read(seed_dir("epubkit_package").join("epub3-nav.epub"))
        .expect("the generated seed is committed");
    let mut book = epubkit::Package::from_bytes(&bytes).expect("it opens");
    assert!(book.cover().is_some());
    assert_eq!(book.toc().map(|t| t.iter().count()), Some(4));
    assert!(!book.spine_text(0).unwrap().is_empty());
    assert!(!book.spine_text(1).unwrap().is_empty());
}

fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    use std::io::Write;
    let mut out = std::io::Cursor::new(Vec::new());
    {
        let mut w = zip::ZipWriter::new(&mut out);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in entries {
            w.start_file(*name, opts).expect("start entry");
            w.write_all(bytes).expect("write entry");
        }
        w.finish().expect("finish zip");
    }
    out.into_inner()
}

/// Regenerate `fuzz/seeds/epubkit_package/epub3-nav.epub`: a small EPUB 3
/// with a navigation document, an NCX beside it, a cover, a well-formed
/// chapter and a sloppy one — so a fuzzer mutating it starts with every
/// layer reachable. Deterministic: no timestamps (zip's default date).
///
/// `cargo test -p epubkit --test fuzz_seeds -- --ignored write_generated_seeds`
#[test]
#[ignore]
fn write_generated_seeds() {
    let container = r#"<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#;
    let opf = r##"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="uid">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="uid">urn:isbn:9780316769488</dc:identifier>
    <dc:title id="t">Seed&mdash;Book</dc:title>
    <meta refines="#t" property="title-type">main</meta>
    <dc:creator id="c">A. Writer</dc:creator>
    <meta refines="#c" property="role" scheme="marc:relators">aut</meta>
    <dc:language>en</dc:language>
    <meta property="dcterms:modified">2026-01-01T00:00:00Z</meta>
  </metadata>
  <manifest>
    <item id="nav" href="nav/nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
    <item id="cover" href="img/cover.png" media-type="image/png" properties="cover-image"/>
    <item id="c1" href="text/c1.xhtml" media-type="application/xhtml+xml"/>
    <item id="c2" href="text/c2.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine toc="ncx"><itemref idref="c1"/><itemref idref="c2"/></spine>
</package>"##;
    let nav = r#"<?xml version="1.0" encoding="utf-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><head><title>Nav</title></head><body>
<nav epub:type="toc"><h1>Contents</h1><ol>
  <li><a href="../text/c1.xhtml">One&nbsp;&hellip;</a><ol><li><a href="../text/c1.xhtml#s">One, s</a></li></ol></li>
  <li><span>Part</span><ol><li><a href="../text/c2.xhtml">Two</a></li></ol></li>
</ol></nav>
<nav epub:type="landmarks"><ol><li><a epub:type="bodymatter" href="../text/c1.xhtml">Start</a></li></ol></nav>
<nav epub:type="page-list"><ol><li><a href="../text/c1.xhtml#p1">1</a></li></ol></nav>
</body></html>"#;
    let ncx = r#"<?xml version="1.0"?><ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"><docTitle><text>Seed</text></docTitle><navMap>
<navPoint id="a" playOrder="1"><navLabel><text>One</text></navLabel><content src="text/c1.xhtml"/></navPoint>
<navPoint id="b"><navLabel><text>Two</text></navLabel><content src="text/c2.xhtml"/></navPoint>
</navMap><pageList><pageTarget type="normal" value="1"><navLabel><text>1</text></navLabel><content src="text/c1.xhtml#p1"/></pageTarget></pageList></ncx>"#;
    let c1 = r#"<?xml version="1.0" encoding="utf-8"?>
<html xmlns="http://www.w3.org/1999/xhtml"><head><style>p{}</style></head><body>
<h1>One</h1><p>It was a <em>dark</em> night&mdash;<span id="p1"/>the end.</p>
<blockquote><p>Quoted.</p></blockquote><ul><li>item</li></ul><pre>  keep
  this</pre></body></html>"#;
    let c2 = "<HTML><BODY><P>Sloppy & loose<BR>text<DIV><B>bold</DIV><P>unclosed &eacute;";
    let png: &[u8] =
        b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0\x1f\x15\xc4\x89";
    let bytes = zip_of(&[
        ("mimetype", b"application/epub+zip"),
        ("META-INF/container.xml", container.as_bytes()),
        ("OEBPS/content.opf", opf.as_bytes()),
        ("OEBPS/nav/nav.xhtml", nav.as_bytes()),
        ("OEBPS/toc.ncx", ncx.as_bytes()),
        ("OEBPS/img/cover.png", png),
        ("OEBPS/text/c1.xhtml", c1.as_bytes()),
        ("OEBPS/text/c2.xhtml", c2.as_bytes()),
    ]);
    let dir = seed_dir("epubkit_package");
    std::fs::create_dir_all(&dir).expect("seed dir");
    std::fs::write(dir.join("epub3-nav.epub"), bytes).expect("write seed");
}
