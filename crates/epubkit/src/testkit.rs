//! Test fixtures, built in memory with `zip` — never committed as binaries.

use std::io::{Cursor, Write};

use zip::write::SimpleFileOptions;

/// A zip of these entries, deflated, in this order.
pub(crate) fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    {
        let mut w = zip::ZipWriter::new(&mut out);
        let opts =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in entries {
            w.start_file(*name, opts).expect("start entry");
            w.write_all(bytes).expect("write entry");
        }
        w.finish().expect("finish zip");
    }
    out.into_inner()
}

pub(crate) fn container(opf_path: &str) -> String {
    format!(
        r#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="{opf_path}" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#
    )
}

/// An OPF around this metadata, manifest and spine markup.
pub(crate) fn opf(version: &str, metadata: &str, manifest: &str, spine: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="{version}" unique-identifier="uid">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:opf="http://www.idpf.org/2007/opf">
{metadata}
  </metadata>
  <manifest>
{manifest}
  </manifest>
  {spine}
</package>"#
    )
}

/// The smallest complete book: OPF at `OEBPS/content.opf`, one chapter.
pub(crate) fn minimal_book(metadata: &str) -> Vec<u8> {
    let opf = opf(
        "3.0",
        metadata,
        r#"<item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/>"#,
        r#"<spine><itemref idref="ch1"/></spine>"#,
    );
    zip_of(&[
        ("mimetype", b"application/epub+zip"),
        (
            "META-INF/container.xml",
            container("OEBPS/content.opf").as_bytes(),
        ),
        ("OEBPS/content.opf", opf.as_bytes()),
        ("OEBPS/ch1.xhtml", b"<html><body><p>One</p></body></html>"),
    ])
}

/// Escape text for an XML element or a double-quoted attribute.
pub(crate) fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
