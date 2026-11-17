//! What a PDF says about itself: a page count and sometimes a title.
//! Zero is never a page count: an unknown length is absent.

use std::path::Path;

use crate::error::{EngineError, Result};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PdfInfo {
    /// Page count; never `Some(0)`, since zero means unknown.
    pub page_count: Option<i64>,
    /// The document's own title; never `Some("")`.
    pub title: Option<String>,
}

/// Reads a PDF's metadata. `Err` means unparseable; `Ok` with nothing set means it parsed and
/// would not say (encrypted included). Only xref, catalog and `/Info` are read, not page contents.
#[tracing::instrument(fields(path = %path.display()))]
pub fn pdf_info(path: &Path) -> Result<PdfInfo> {
    let meta = lopdf::Document::load_metadata(path)
        .map_err(|e| EngineError::Pdf(format!("{}: {e}", path.display())))?;

    // lopdf reports 0 for every way of not knowing, so 0 is absence, not an empty document.
    let page_count = (meta.page_count > 0).then_some(i64::from(meta.page_count));
    let title = meta.title.as_deref().and_then(title_or_none);

    tracing::debug!(page_count, has_title = title.is_some(), "read pdf metadata");
    Ok(PdfInfo { page_count, title })
}

/// Extensions that show up in `/Info /Title` when a producer echoed a source filename.
/// Deliberately narrow: a broader filename heuristic would discard real titles like *Sync*.
const AUTHORING_EXTENSIONS: [&str; 10] = [
    ".doc", ".docx", ".rtf", ".odt", ".pages", ".tex", ".indd", ".qxd", ".pdf", ".ps",
];

fn title_or_none(raw: &str) -> Option<String> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    let lower = t.to_ascii_lowercase();
    if AUTHORING_EXTENSIONS.iter().any(|ext| lower.ends_with(ext)) {
        tracing::debug!("pdf title looks like a source filename; falling back");
        return None;
    }
    Some(t.to_string())
}

/// Builds a classic (PDF 1.4, uncompressed xref) document so no test commits a binary fixture.
/// Assembled by hand so the fixture owes nothing to `lopdf`.
#[cfg(any(test, feature = "internals"))]
pub fn synthetic_pdf(pages: usize, title: Option<&[u8]>) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut offsets: Vec<usize> = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n");

    let push = |out: &mut Vec<u8>, offsets: &mut Vec<usize>, n: usize, body: &[u8]| {
        offsets.push(out.len());
        out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    };

    push(
        &mut out,
        &mut offsets,
        1,
        b"<< /Type /Catalog /Pages 2 0 R >>",
    );

    let kids: String = (0..pages)
        .map(|i| format!("{} 0 R ", i + 3))
        .collect::<String>();
    push(
        &mut out,
        &mut offsets,
        2,
        format!("<< /Type /Pages /Kids [{kids}] /Count {pages} >>").as_bytes(),
    );
    for i in 0..pages {
        push(
            &mut out,
            &mut offsets,
            i + 3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>",
        );
    }

    let info_num = pages + 3;
    if let Some(t) = title {
        let mut body = b"<< /Title (".to_vec();
        body.extend_from_slice(t);
        body.extend_from_slice(b") >>");
        push(&mut out, &mut offsets, info_num, &body);
    }

    let count = offsets.len() + 1; // +1 for the free object 0
    let xref_at = out.len();
    out.extend_from_slice(format!("xref\n0 {count}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for off in &offsets {
        // Exactly twenty bytes per entry, which the format requires.
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    let info = if title.is_some() {
        format!(" /Info {info_num} 0 R")
    } else {
        String::new()
    };
    out.extend_from_slice(
        format!("trailer\n<< /Size {count} /Root 1 0 R{info} >>\nstartxref\n{xref_at}\n%%EOF\n")
            .as_bytes(),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(bytes: &[u8], name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).expect("write");
        (dir, path)
    }

    #[test]
    fn reads_the_page_count_and_the_title() {
        let (_d, p) = write(&synthetic_pdf(7, Some(b"Long Walk to Freedom")), "a.pdf");
        let info = pdf_info(&p).unwrap();
        assert_eq!(info.page_count, Some(7));
        assert_eq!(info.title.as_deref(), Some("Long Walk to Freedom"));
    }

    #[test]
    fn a_pdf_with_no_info_dictionary_has_no_title_and_still_has_a_length() {
        let (_d, p) = write(&synthetic_pdf(3, None), "b.pdf");
        let info = pdf_info(&p).unwrap();
        assert_eq!(info.page_count, Some(3));
        assert_eq!(info.title, None);
    }

    #[test]
    fn a_length_that_could_not_be_read_is_absent_and_never_zero() {
        let (_d, p) = write(&synthetic_pdf(0, None), "empty.pdf");
        let info = pdf_info(&p).unwrap();
        assert_eq!(info.page_count, None, "zero pages must read as absence");
    }

    #[test]
    fn a_pdf_with_no_catalog_is_absence_rather_than_zero() {
        let mut bytes = synthetic_pdf(4, None);
        let s = String::from_utf8(bytes.clone()).unwrap();
        bytes = s.replace("/Root 1 0 R", "").into_bytes();
        let (_d, p) = write(&bytes, "rootless.pdf");
        match pdf_info(&p) {
            Ok(info) => assert_eq!(info.page_count, None),
            Err(EngineError::Pdf(_)) => {}
            Err(e) => panic!("unexpected error: {e}"),
        }
    }

    #[test]
    fn bytes_that_are_not_a_pdf_are_a_typed_error() {
        let (_d, p) = write(b"this is a text file, not a pdf", "c.pdf");
        assert!(matches!(pdf_info(&p), Err(EngineError::Pdf(_))));
    }

    #[test]
    fn a_truncated_pdf_is_a_typed_error_rather_than_a_panic() {
        let full = synthetic_pdf(5, Some(b"Cut Short"));
        let (_d, p) = write(&full[..full.len() / 2], "d.pdf");
        assert!(matches!(pdf_info(&p), Err(EngineError::Pdf(_))));
    }

    #[test]
    fn a_missing_file_is_an_error_and_not_a_page_count() {
        assert!(pdf_info(Path::new("/nonexistent/nope.pdf")).is_err());
    }

    #[test]
    fn a_producers_echo_of_a_source_filename_is_not_a_title() {
        for junk in [
            "Microsoft Word - chapter3final.doc",
            "thesis.tex",
            "  report.DOCX  ",
            "scan_0001.pdf",
        ] {
            assert_eq!(title_or_none(junk), None, "{junk:?} should be refused");
        }
    }

    #[test]
    fn a_real_title_survives_the_filter() {
        for good in [
            "Long Walk to Freedom",
            "The C Programming Language",
            // Ends in a word that is also an extension but with no dot: the narrow rule must keep these.
            "Learning Pages",
            "Ghosts of the Tsunami",
        ] {
            assert_eq!(title_or_none(good).as_deref(), Some(good.trim()));
        }
    }

    #[test]
    fn an_empty_title_is_absence() {
        assert_eq!(title_or_none(""), None);
        assert_eq!(title_or_none("   \n "), None);
    }

    #[test]
    fn a_real_pdf_reports_a_plausible_length() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pdf/real");
        let files: Vec<_> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf")))
            .collect();

        if files.is_empty() {
            let msg = format!(
                "SKIPPED: a_real_pdf_reports_a_plausible_length — no .pdf in {}",
                dir.display()
            );
            assert!(
                std::env::var("BOOKMOLE_REQUIRE_FIXTURES").is_err(),
                "{msg} (BOOKMOLE_REQUIRE_FIXTURES=1)"
            );
            println!("{msg}");
            return;
        }

        for path in files {
            let info = pdf_info(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            match info.page_count {
                Some(n) => assert!(n > 0, "{}: a page count of {n}", path.display()),
                None => println!(
                    "note: {} parsed but would not give a length",
                    path.display()
                ),
            }
            assert_ne!(
                info.title.as_deref(),
                Some(""),
                "{}: an empty title must be absence",
                path.display()
            );
        }
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn a_reported_length_is_never_zero(pages in 0usize..24) {
                let (_d, p) = write(&synthetic_pdf(pages, None), "p.pdf");
                if let Ok(info) = pdf_info(&p) {
                    prop_assert!(info.page_count.is_none_or(|n| n > 0));
                    if pages > 0 {
                        prop_assert_eq!(info.page_count, Some(pages as i64));
                    }
                }
            }

            #[test]
            fn a_title_is_absent_or_non_empty(s in ".{0,80}") {
                match title_or_none(&s) {
                    Some(t) => {
                        prop_assert!(!t.is_empty());
                        prop_assert_eq!(t, s.trim());
                    }
                    None => prop_assert!(
                        s.trim().is_empty()
                            || AUTHORING_EXTENSIONS
                                .iter()
                                .any(|e| s.trim().to_ascii_lowercase().ends_with(e))
                    ),
                }
            }
        }
    }
}
