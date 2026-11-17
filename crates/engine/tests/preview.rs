//! Preview: PDF annotations imported as highlights. Fixtures are assembled as raw bytes, not by
//! `lopdf`, so the parser does not build its own goldens.

mod common;
use std::path::Path;

use bookmole::preview::{
    AnnotKind, PdfAnnotation, PdfScan, RescanOutcome, SyntheticPage, highlights_of,
    import_pdf_annotations, rescan_path_place, scan_pdf_annotations, synthetic_annotated_pdf,
};
use bookmole::{
    CopyForm, DiagnosticKind, HighlightOrigin, LearnedFrom, NewCopy, NewPlace, PlaceKind, PosKind,
    Tenure,
};
use common::{engine, seed_book};

const LINE: &str = "The quick brown fox jumps over the lazy dog";

fn quad(from: usize, to: usize) -> String {
    let (x0, x1) = (72.0 + 7.2 * from as f64, 72.0 + 7.2 * to as f64);
    format!("{x0:.1} 712 {x1:.1} 712 {x0:.1} 697 {x1:.1} 697")
}

fn markup(subtype: &str, from: usize, to: usize, extra: &str) -> String {
    let q = quad(from, to);
    let (x0, x1) = (72.0 + 7.2 * from as f64, 72.0 + 7.2 * to as f64);
    format!(
        "/Subtype /{subtype} /Rect [{x0:.1} 697 {x1:.1} 712] /QuadPoints [{q}] /C [1 1 0] {extra}"
    )
}

fn page_one(sticky_note: &str) -> SyntheticPage {
    SyntheticPage {
        lines: vec![(72.0, 700.0, 1, LINE.to_string())],
        annots: vec![
            markup(
                "Highlight",
                10,
                19,
                "/NM (nm-high) /Contents (why the fox) /M (D:20260105211408+01'00') /T (A. Reader)",
            ),
            markup("Underline", 35, 43, "/NM (nm-under)"),
            markup("StrikeOut", 4, 9, "/NM (nm-strike)"),
            format!(
                "/Subtype /Text /Rect [300 600 320 620] /NM (nm-sticky) /Contents ({sticky_note})"
            ),
            "/Subtype /FreeText /Rect [50 500 250 530] /NM (nm-free) /Contents (typed on the page)"
                .into(),
            "/Subtype /Link /Rect [0 0 10 10]".into(),
        ],
    }
}

fn page_two() -> SyntheticPage {
    SyntheticPage {
        lines: vec![(72.0, 700.0, 2, "set in helvetica with no widths".into())],
        annots: vec![
            markup("Highlight", 0, 8, "/NM (nm-unmeasured) /Contents (scanned)"),
            "/Subtype /Highlight /Rect [72 300 150 315] /QuadPoints [72 315 150 315 72 300 150 300]".into(),
        ],
    }
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

fn fixture(sticky: &str) -> Vec<u8> {
    synthetic_annotated_pdf(&[page_one(sticky), page_two()])
}

#[tokio::test]
async fn one_of_each_annotation_becomes_a_preview_highlight() {
    let (tmp, e) = engine().await;
    let s = e.storage();
    let book = seed_book(&e, "A").await;
    let p = write(tmp.path(), "a.pdf", &fixture("remember this"));

    let sum = import_pdf_annotations(s, book, None, &p).await.unwrap();
    assert_eq!(
        (sum.inserted, sum.updated, sum.unchanged, sum.unparsed),
        (7, 0, 0, 0)
    );
    assert!(sum.diagnostics.is_empty());

    let rows = s
        .imported_highlights(book, HighlightOrigin::Preview)
        .await
        .unwrap();
    assert_eq!(rows.len(), 7);
    for r in &rows {
        assert_eq!(r.origin, HighlightOrigin::Preview);
        assert_eq!(r.source, "preview");
        assert_eq!(r.pos_kind, Some(PosKind::PdfRect));
        assert!(r.page == Some(1) || r.page == Some(2));
    }
    let by = |nm: &str| {
        rows.iter()
            .find(|r| r.origin_id.as_deref() == Some(nm))
            .unwrap()
    };

    let high = by("nm-high");
    assert_eq!(high.text, "brown fox");
    assert!(!high.text_unavailable);
    assert_eq!(high.origin_note.as_deref(), Some("why the fox"));
    assert_eq!(high.page, Some(1));
    assert_eq!(high.ko_datetime.as_deref(), Some("2026-01-05 20:14:08"));
    assert_eq!(by("nm-under").text, "lazy dog");
    assert_eq!(by("nm-strike").text, "quick");

    for (nm, note) in [
        ("nm-sticky", "remember this"),
        ("nm-free", "typed on the page"),
    ] {
        let r = by(nm);
        assert_eq!(r.text, "");
        assert!(!r.text_unavailable);
        assert_eq!(r.origin_note.as_deref(), Some(note));
        assert_eq!(r.page, Some(1));
    }

    let built = highlights_of(&scan_pdf_annotations(&p).unwrap(), None);
    let b = |nm: &str| {
        built
            .iter()
            .find(|h| h.origin_id.as_deref() == Some(nm))
            .unwrap()
    };
    assert_eq!(b("nm-high").color.as_deref(), Some("#ffff00"));
    assert_eq!(
        b("nm-high").pos0.as_deref(),
        Some("p1:h:144,712,208.8,712,144,697,208.8,697")
    );
    assert_eq!(b("nm-sticky").pos0.as_deref(), Some("p1:t:300,600,320,620"));
    assert_eq!(b("nm-high").pos1, None);
}

#[tokio::test]
async fn words_that_cannot_be_read_keep_the_highlight() {
    let (tmp, e) = engine().await;
    let s = e.storage();
    let book = seed_book(&e, "A").await;
    let p = write(tmp.path(), "a.pdf", &fixture("n"));
    import_pdf_annotations(s, book, None, &p).await.unwrap();
    let rows = s
        .imported_highlights(book, HighlightOrigin::Preview)
        .await
        .unwrap();
    let page2: Vec<_> = rows.iter().filter(|r| r.page == Some(2)).collect();
    assert_eq!(page2.len(), 2);
    for r in page2 {
        assert!(r.text_unavailable);
        assert_eq!(r.text, "");
        assert_eq!(r.pos_kind, Some(PosKind::PdfRect));
    }
    assert!(
        rows.iter()
            .any(|r| r.text_unavailable && r.origin_note.as_deref() == Some("scanned"))
    );
}

#[tokio::test]
async fn a_second_identical_import_is_all_unchanged() {
    let (tmp, e) = engine().await;
    let s = e.storage();
    let book = seed_book(&e, "A").await;
    let p = write(tmp.path(), "a.pdf", &fixture("n"));
    import_pdf_annotations(s, book, None, &p).await.unwrap();
    let again = import_pdf_annotations(s, book, None, &p).await.unwrap();
    assert_eq!(
        (
            again.inserted,
            again.updated,
            again.unchanged,
            again.removed
        ),
        (0, 0, 7, 0)
    );
    assert_eq!(
        s.imported_highlights(book, HighlightOrigin::Preview)
            .await
            .unwrap()
            .len(),
        7
    );
}

#[tokio::test]
async fn an_edited_note_updates_and_the_annotation_is_untouched() {
    let (tmp, e) = engine().await;
    let s = e.storage();
    let book = seed_book(&e, "A").await;
    let p = write(tmp.path(), "a.pdf", &fixture("before"));
    import_pdf_annotations(s, book, None, &p).await.unwrap();
    let rows = s
        .imported_highlights(book, HighlightOrigin::Preview)
        .await
        .unwrap();
    let sticky = rows
        .iter()
        .find(|r| r.origin_id.as_deref() == Some("nm-sticky"))
        .unwrap()
        .id;
    s.set_annotation(sticky, Some("my own thought"))
        .await
        .unwrap();

    write(tmp.path(), "a.pdf", &fixture("after"));
    let sum = import_pdf_annotations(s, book, None, &p).await.unwrap();
    assert_eq!((sum.inserted, sum.updated, sum.unchanged), (0, 1, 6));
    let r = s.get_highlight(sticky).await.unwrap().unwrap();
    assert_eq!(r.origin_note.as_deref(), Some("after"));
    assert_eq!(r.annotation.as_deref(), Some("my own thought"));
}

#[tokio::test]
async fn without_a_name_identity_is_page_kind_and_rounded_rect() {
    let (tmp, e) = engine().await;
    let s = e.storage();
    let book = seed_book(&e, "A").await;
    let mk = |dx: f64, note: &str| {
        let (x0, x1) = (144.0 + dx, 208.8 + dx);
        synthetic_annotated_pdf(&[SyntheticPage {
            lines: vec![(72.0, 700.0, 1, LINE.into())],
            annots: vec![format!(
                "/Subtype /Highlight /Rect [{x0} 697 {x1} 712] /QuadPoints [{x0} 712 {x1} 712 {x0} 697 {x1} 697] /Contents ({note})"
            )],
        }])
    };
    let p = write(tmp.path(), "a.pdf", &mk(0.0, "one"));
    import_pdf_annotations(s, book, None, &p).await.unwrap();
    write(tmp.path(), "a.pdf", &mk(0.2, "two"));
    let sum = import_pdf_annotations(s, book, None, &p).await.unwrap();
    assert_eq!((sum.inserted, sum.updated), (0, 1));
    let rows = s
        .imported_highlights(book, HighlightOrigin::Preview)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].origin_id, None);
}

#[tokio::test]
async fn identical_unnamed_annotations_are_two_highlights() {
    let (tmp, e) = engine().await;
    let s = e.storage();
    let book = seed_book(&e, "A").await;
    let a = markup("Highlight", 10, 19, "");
    let bytes = synthetic_annotated_pdf(&[SyntheticPage {
        lines: vec![(72.0, 700.0, 1, LINE.into())],
        annots: vec![a.clone(), a],
    }]);
    let p = write(tmp.path(), "a.pdf", &bytes);
    let sum = import_pdf_annotations(s, book, None, &p).await.unwrap();
    assert_eq!(sum.inserted, 2);
    assert_eq!(
        import_pdf_annotations(s, book, None, &p)
            .await
            .unwrap()
            .unchanged,
        2
    );
}

async fn copy_with_path(e: &bookmole::Engine, book: i64, path: &Path) -> (i64, String) {
    let s = e.storage();
    let copy = s
        .create_copy(&NewCopy::new(
            book,
            Some(CopyForm::File),
            Tenure::Own,
            LearnedFrom::FileImport,
        ))
        .await
        .unwrap();
    let reference = path.display().to_string();
    let mut place = NewPlace::new(PlaceKind::Path, reference.clone());
    place.last_hash = Some(bookmole::files::sha256_of(path).unwrap());
    place.last_mtime = std::fs::metadata(path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64);
    s.attach_place(copy.id, &place).await.unwrap();
    (copy.id, reference)
}

fn bump_mtime(p: &Path, secs: u64) {
    let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
    f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(secs))
        .unwrap();
}

#[tokio::test]
async fn rescan_skips_an_unchanged_mtime_and_keeps_one_copy_on_changed_bytes() {
    let (tmp, e) = engine().await;
    let s = e.storage();
    let book = seed_book(&e, "A").await;
    let p = write(tmp.path(), "a.pdf", &fixture("before"));
    let (copy, reference) = copy_with_path(&e, book, &p).await;
    let hash_before = s.get_copy(copy).await.unwrap().unwrap().places[0]
        .last_hash
        .clone();

    let r = rescan_path_place(s, copy, &reference).await.unwrap();
    assert_eq!(r.outcome, RescanOutcome::Unchanged);
    assert!(
        s.imported_highlights(book, HighlightOrigin::Preview)
            .await
            .unwrap()
            .is_empty()
    );

    write(tmp.path(), "a.pdf", &fixture("after"));
    bump_mtime(&p, 100);
    let r = rescan_path_place(s, copy, &reference).await.unwrap();
    assert_eq!(r.outcome, RescanOutcome::Reimported);
    assert_eq!(r.summary.inserted, 7);

    let copies = s.copies_for_book(book).await.unwrap();
    assert_eq!(copies.len(), 1, "same place, new bytes is the same copy");
    let place = &copies[0].places[0];
    assert_ne!(place.last_hash, hash_before);
    assert_eq!(
        place.last_hash.as_deref(),
        Some(bookmole::files::sha256_of(&p).unwrap().as_str())
    );
    let rows = s
        .imported_highlights(book, HighlightOrigin::Preview)
        .await
        .unwrap();
    assert!(rows.iter().all(|r| r.copy_id == Some(copy)));

    let r = rescan_path_place(s, copy, &reference).await.unwrap();
    assert_eq!(r.outcome, RescanOutcome::Unchanged);
}

#[tokio::test]
async fn a_missing_file_marks_the_place_and_leaves_the_copy() {
    let (tmp, e) = engine().await;
    let s = e.storage();
    let book = seed_book(&e, "A").await;
    let p = write(tmp.path(), "a.pdf", &fixture("n"));
    let (copy, reference) = copy_with_path(&e, book, &p).await;
    std::fs::remove_file(&p).unwrap();

    let r = rescan_path_place(s, copy, &reference).await.unwrap();
    assert_eq!(r.outcome, RescanOutcome::Missing);
    let c = s.get_copy(copy).await.unwrap().unwrap();
    assert!(c.places[0].missing_since.is_some());
    assert_eq!(s.copies_for_book(book).await.unwrap().len(), 1);

    write(tmp.path(), "a.pdf", &fixture("n"));
    bump_mtime(&p, 100);
    let r = rescan_path_place(s, copy, &reference).await.unwrap();
    assert_eq!(r.outcome, RescanOutcome::Reimported);
    assert!(
        s.get_copy(copy).await.unwrap().unwrap().places[0]
            .missing_since
            .is_none()
    );
}

#[tokio::test]
async fn a_removed_annotation_is_removed_unless_it_carries_ours() {
    let (tmp, e) = engine().await;
    let s = e.storage();
    let book = seed_book(&e, "A").await;
    let p = write(tmp.path(), "a.pdf", &fixture("n"));
    let (copy, _) = copy_with_path(&e, book, &p).await;
    import_pdf_annotations(s, book, Some(copy), &p)
        .await
        .unwrap();
    let rows = s
        .imported_highlights(book, HighlightOrigin::Preview)
        .await
        .unwrap();
    let id_of = |nm: &str| {
        rows.iter()
            .find(|r| r.origin_id.as_deref() == Some(nm))
            .unwrap()
            .id
    };
    s.set_annotation(id_of("nm-under"), Some("mine"))
        .await
        .unwrap();

    let mut one = page_one("n");
    one.annots.truncate(1);
    write(
        tmp.path(),
        "a.pdf",
        &synthetic_annotated_pdf(&[one, page_two()]),
    );
    let sum = import_pdf_annotations(s, book, Some(copy), &p)
        .await
        .unwrap();
    assert_eq!(sum.removed, 3);
    let kept: Vec<_> = sum
        .diagnostics
        .iter()
        .filter(|d| matches!(d.kind, DiagnosticKind::PdfAnnotationsKept { count: 1, .. }))
        .collect();
    assert_eq!(kept.len(), 1);
    let rows = s
        .imported_highlights(book, HighlightOrigin::Preview)
        .await
        .unwrap();
    assert_eq!(rows.len(), 4);
    assert!(rows.iter().any(|r| r.id == id_of("nm-under")));
}

#[tokio::test]
async fn no_copy_means_nothing_is_removed() {
    let (tmp, e) = engine().await;
    let s = e.storage();
    let book = seed_book(&e, "A").await;
    let p = write(tmp.path(), "a.pdf", &fixture("n"));
    import_pdf_annotations(s, book, None, &p).await.unwrap();
    write(
        tmp.path(),
        "a.pdf",
        &synthetic_annotated_pdf(&[SyntheticPage::default()]),
    );
    let sum = import_pdf_annotations(s, book, None, &p).await.unwrap();
    assert_eq!(sum.removed, 0);
    assert_eq!(
        s.imported_highlights(book, HighlightOrigin::Preview)
            .await
            .unwrap()
            .len(),
        7
    );
}

#[tokio::test]
async fn a_file_with_no_annotations_is_an_empty_import() {
    let (tmp, e) = engine().await;
    let book = seed_book(&e, "A").await;
    let bytes = synthetic_annotated_pdf(&[SyntheticPage {
        lines: vec![(72.0, 700.0, 1, LINE.into())],
        annots: vec![],
    }]);
    let p = write(tmp.path(), "a.pdf", &bytes);
    let sum = import_pdf_annotations(e.storage(), book, None, &p)
        .await
        .unwrap();
    assert_eq!(
        (
            sum.inserted,
            sum.updated,
            sum.unchanged,
            sum.removed,
            sum.unparsed
        ),
        (0, 0, 0, 0, 0)
    );
    assert!(sum.diagnostics.is_empty());
}

#[tokio::test]
async fn hostile_files_degrade_to_a_diagnostic_and_remove_nothing() {
    let (tmp, e) = engine().await;
    let s = e.storage();
    let book = seed_book(&e, "A").await;
    let good = write(tmp.path(), "good.pdf", &fixture("n"));
    let (copy, _) = copy_with_path(&e, book, &good).await;
    import_pdf_annotations(s, book, Some(copy), &good)
        .await
        .unwrap();

    let full = fixture("n");
    let mut encrypted = String::from_utf8(full.clone()).unwrap();
    encrypted = encrypted.replace(
        "/Root 1 0 R",
        "/Root 1 0 R /Encrypt << /Filter /Standard /V 1 /R 2 /Length 40 \
         /O (00000000000000000000000000000000) /U (00000000000000000000000000000000) /P -4 >>",
    );
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("text.pdf", b"this is not a pdf".to_vec()),
        ("empty.pdf", Vec::new()),
        ("truncated.pdf", full[..full.len() / 2].to_vec()),
        (
            "garbage.pdf",
            (0..4096u32).map(|i| (i * 31 % 251) as u8).collect(),
        ),
        ("encrypted.pdf", encrypted.into_bytes()),
    ];
    for (name, bytes) in cases {
        let p = write(tmp.path(), name, &bytes);
        let sum = import_pdf_annotations(s, book, Some(copy), &p)
            .await
            .unwrap();
        assert_eq!(
            sum.inserted + sum.updated + sum.unchanged + sum.removed,
            0,
            "{name}"
        );
        assert_eq!(sum.diagnostics.len(), 1, "{name}");
        assert!(
            matches!(
                sum.diagnostics[0].kind,
                DiagnosticKind::PdfUnreadable { .. } | DiagnosticKind::PdfEncrypted { .. }
            ),
            "{name}: {:?}",
            sum.diagnostics[0].kind
        );
        if name == "encrypted.pdf" {
            assert!(matches!(
                sum.diagnostics[0].kind,
                DiagnosticKind::PdfEncrypted { .. }
            ));
        }
    }
    let sum = import_pdf_annotations(s, book, Some(copy), &tmp.path().join("nope.pdf"))
        .await
        .unwrap();
    assert!(matches!(
        sum.diagnostics[0].kind,
        DiagnosticKind::PdfUnreadable { .. }
    ));
    assert_eq!(
        s.imported_highlights(book, HighlightOrigin::Preview)
            .await
            .unwrap()
            .len(),
        7
    );
}

fn twin(note: &str) -> SyntheticPage {
    SyntheticPage {
        lines: vec![(72.0, 700.0, 1, LINE.into())],
        annots: vec![
            markup(
                "Highlight",
                10,
                19,
                &format!("/NM (nm-same) /Contents ({note})"),
            ),
            markup("Underline", 35, 43, &format!("/Contents ({note})")),
        ],
    }
}

fn counts(s: &bookmole::HighlightImportSummary) -> (usize, usize, usize, usize) {
    (s.inserted, s.updated, s.unchanged, s.removed)
}

#[tokio::test]
async fn two_copies_annotated_at_the_same_place_are_two_highlights() {
    let (tmp, e) = engine().await;
    let s = e.storage();
    let book = seed_book(&e, "A").await;
    let pa = write(
        tmp.path(),
        "a.pdf",
        &synthetic_annotated_pdf(&[twin("in a")]),
    );
    let pb = write(
        tmp.path(),
        "b.pdf",
        &synthetic_annotated_pdf(&[twin("in b")]),
    );
    let (ca, _) = copy_with_path(&e, book, &pa).await;
    let (cb, _) = copy_with_path(&e, book, &pb).await;

    let a = import_pdf_annotations(s, book, Some(ca), &pa)
        .await
        .unwrap();
    let b = import_pdf_annotations(s, book, Some(cb), &pb)
        .await
        .unwrap();
    assert_eq!(counts(&a), (2, 0, 0, 0));
    assert_eq!(
        counts(&b),
        (2, 0, 0, 0),
        "B's same /NM and same place are B's own"
    );
    let rows_of = |copy: i64| {
        let s = s.clone();
        async move {
            s.imported_highlights(book, HighlightOrigin::Preview)
                .await
                .unwrap()
                .into_iter()
                .filter(|r| r.copy_id == Some(copy))
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(rows_of(ca).await.len(), 2);
    assert_eq!(rows_of(cb).await.len(), 2);

    write(
        tmp.path(),
        "b.pdf",
        &synthetic_annotated_pdf(&[twin("edited in b")]),
    );
    let b = import_pdf_annotations(s, book, Some(cb), &pb)
        .await
        .unwrap();
    assert_eq!(counts(&b), (0, 2, 0, 0));
    assert!(
        rows_of(ca)
            .await
            .iter()
            .all(|r| r.origin_note.as_deref() == Some("in a"))
    );
    assert!(
        rows_of(cb)
            .await
            .iter()
            .all(|r| r.origin_note.as_deref() == Some("edited in b"))
    );
    let a = import_pdf_annotations(s, book, Some(ca), &pa)
        .await
        .unwrap();
    assert_eq!(counts(&a), (0, 0, 2, 0));

    write(
        tmp.path(),
        "b.pdf",
        &synthetic_annotated_pdf(&[SyntheticPage::default()]),
    );
    let b = import_pdf_annotations(s, book, Some(cb), &pb)
        .await
        .unwrap();
    assert_eq!(counts(&b), (0, 0, 0, 2));
    assert!(rows_of(cb).await.is_empty());
    assert_eq!(rows_of(ca).await.len(), 2);
}

#[tokio::test]
async fn a_merged_copy_re_imports_both_files_without_duplicating_or_removing() {
    let (tmp, e) = engine().await;
    let s = e.storage();
    let book = seed_book(&e, "A").await;
    let pa = write(
        tmp.path(),
        "a.pdf",
        &synthetic_annotated_pdf(&[twin("in a")]),
    );
    let pb = write(
        tmp.path(),
        "b.pdf",
        &synthetic_annotated_pdf(&[twin("in b")]),
    );
    let (ca, _) = copy_with_path(&e, book, &pa).await;
    let (cb, _) = copy_with_path(&e, book, &pb).await;
    import_pdf_annotations(s, book, Some(ca), &pa)
        .await
        .unwrap();
    import_pdf_annotations(s, book, Some(cb), &pb)
        .await
        .unwrap();

    s.merge_copies(ca, cb).await.unwrap();
    for p in [&pb, &pa] {
        let sum = import_pdf_annotations(s, book, Some(ca), p).await.unwrap();
        assert_eq!(counts(&sum), (0, 0, 2, 0), "{}", p.display());
    }
    let all = || {
        let s = s.clone();
        async move {
            s.imported_highlights(book, HighlightOrigin::Preview)
                .await
                .unwrap()
        }
    };
    assert_eq!(all().await.len(), 4);

    // With two files behind one copy a row's source file is unknown, so a file that lost its
    // annotations removes nothing.
    write(
        tmp.path(),
        "a.pdf",
        &synthetic_annotated_pdf(&[SyntheticPage::default()]),
    );
    let sum = import_pdf_annotations(s, book, Some(ca), &pa)
        .await
        .unwrap();
    assert_eq!(sum.removed, 0);
    assert_eq!(all().await.len(), 4);
}

#[tokio::test]
async fn a_first_import_with_no_copy_is_claimed_by_the_copy() {
    let (tmp, e) = engine().await;
    let s = e.storage();
    let book = seed_book(&e, "A").await;
    let p = write(tmp.path(), "a.pdf", &fixture("n"));
    import_pdf_annotations(s, book, None, &p).await.unwrap();
    let (copy, _) = copy_with_path(&e, book, &p).await;
    let sum = import_pdf_annotations(s, book, Some(copy), &p)
        .await
        .unwrap();
    assert_eq!(counts(&sum), (0, 0, 7, 0));
    let rows = s
        .imported_highlights(book, HighlightOrigin::Preview)
        .await
        .unwrap();
    assert_eq!(rows.len(), 7);
    assert!(rows.iter().all(|r| r.copy_id == Some(copy)));
}

#[tokio::test]
async fn a_run_past_the_glyph_cap_leaves_its_page_text_unavailable() {
    let (tmp, e) = engine().await;
    let s = e.storage();
    let book = seed_book(&e, "A").await;
    let hostile = SyntheticPage {
        lines: vec![
            (72.0, 700.0, 1, LINE.into()),
            (72.0, 600.0, 1, "x".repeat(500_000)),
        ],
        annots: vec![markup(
            "Highlight",
            10,
            19,
            "/NM (nm-long) /Contents (kept)",
        )],
    };
    let p = write(
        tmp.path(),
        "long.pdf",
        &synthetic_annotated_pdf(&[hostile, page_one("n")]),
    );
    let started = std::time::Instant::now();
    let sum = import_pdf_annotations(s, book, None, &p).await.unwrap();
    assert!(started.elapsed().as_secs() < 20, "{:?}", started.elapsed());
    assert_eq!(sum.inserted, 1 + 5);
    let rows = s
        .imported_highlights(book, HighlightOrigin::Preview)
        .await
        .unwrap();
    let by = |nm: &str| {
        rows.iter()
            .find(|r| r.origin_id.as_deref() == Some(nm))
            .unwrap()
    };
    let long = by("nm-long");
    assert!(long.text_unavailable);
    assert_eq!(long.text, "");
    assert_eq!(long.origin_note.as_deref(), Some("kept"));
    assert_eq!(by("nm-high").text, "brown fox");
}

fn pos0_numbers(pos0: &str) -> Vec<f64> {
    let nums = pos0.splitn(3, ':').nth(2).unwrap();
    nums.split(',')
        .filter(|n| !n.is_empty())
        .map(|n| n.parse::<f64>().unwrap())
        .collect()
}

fn assert_finite_place(h: &bookmole::ImportedHighlight) {
    let pos0 = h.pos0.as_deref().unwrap();
    for text in [pos0, h.identity.as_str()] {
        let lower = text.to_ascii_lowercase();
        assert!(!lower.contains("inf") && !lower.contains("nan"), "{text}");
    }
    for n in pos0_numbers(pos0) {
        assert!(n.is_finite() && n.abs() <= 100_000.0, "{pos0}");
    }
}

#[tokio::test]
async fn non_finite_and_far_off_coordinates_never_reach_a_place() {
    let (tmp, e) = engine().await;
    let s = e.storage();
    let book = seed_book(&e, "A").await;
    // `lopdf` reads a real as f32, so fifty digits are an infinity; 1e20 is finite but
    // nowhere near a 612 x 792 page.
    let inf = format!("1{}.0", "0".repeat(50));
    let huge = "100000000000000000000.0";
    let page = SyntheticPage {
        lines: vec![(72.0, 700.0, 1, LINE.into())],
        annots: vec![
            format!(
                "/Subtype /Highlight /Rect [{inf} 697 -{inf} 712] \
                 /QuadPoints [{inf} 712 {inf} 712 -{inf} 697 {inf} 697]"
            ),
            format!(
                "/Subtype /Highlight /NM (huge-quads) /Rect [144 697 208.8 712] \
                 /QuadPoints [{huge} 712 -{huge} 712 144 697 208.8 697]"
            ),
            format!(
                "/Subtype /Highlight /NM (huge-rect) /Rect [-{huge} 697 {huge} 712] \
                 /QuadPoints [{}]",
                quad(10, 19)
            ),
            format!("/Subtype /Text /NM (inf-note) /Rect [-{inf} 0 0 0] /Contents (a note)"),
        ],
    };
    let p = write(tmp.path(), "a.pdf", &synthetic_annotated_pdf(&[page]));
    let sum = import_pdf_annotations(s, book, None, &p).await.unwrap();
    assert_eq!((sum.inserted, sum.unparsed), (3, 1));

    let built = highlights_of(&scan_pdf_annotations(&p).unwrap(), None);
    assert_eq!(built.len(), 3);
    built.iter().for_each(assert_finite_place);
    let b = |nm: &str| {
        built
            .iter()
            .find(|h| h.origin_id.as_deref() == Some(nm))
            .unwrap()
    };
    assert!(b("huge-quads").text_unavailable);
    assert_eq!(
        b("huge-quads").pos0.as_deref(),
        Some("p1:h:144,697,208.8,712")
    );
    assert_eq!(b("huge-rect").text, "brown fox");
    assert_eq!(
        b("huge-rect").pos0.as_deref(),
        Some("p1:h:144,712,208.8,712,144,697,208.8,697")
    );
    assert_eq!(b("inf-note").pos0.as_deref(), Some("p1:t:"));

    let hand = PdfScan {
        annotations: vec![
            PdfAnnotation {
                page: 1,
                kind: AnnotKind::Highlight,
                name: None,
                rect: Some([f64::NAN, 0.0, f64::INFINITY, 1.0]),
                quads: vec![[1.0; 8], [f64::NAN; 8]],
                contents: None,
                made_at: None,
                color: None,
                text: None,
            },
            PdfAnnotation {
                page: 1,
                kind: AnnotKind::Underline,
                name: None,
                rect: None,
                quads: vec![[1e300; 8]; 256],
                contents: None,
                made_at: None,
                color: None,
                text: None,
            },
        ],
        unparsed: 0,
        truncated: false,
    };
    let built = highlights_of(&hand, None);
    for h in &built {
        let pos0 = h.pos0.as_deref().unwrap();
        assert!(pos0.len() <= 24 * 1024, "{}", pos0.len());
        for text in [pos0, h.identity.as_str()] {
            assert!(!text.to_ascii_lowercase().contains("nan"), "{text}");
            assert!(!text.to_ascii_lowercase().contains("inf"), "{text}");
        }
        assert!(pos0_numbers(pos0).iter().all(|n| n.is_finite()));
    }
    assert_eq!(built[0].pos0.as_deref(), Some("p1:h:"));
    assert_eq!(built[0].identity, "1|h|0,0,0,0");
}

#[tokio::test]
async fn a_file_past_the_quad_budget_stops_there_and_counts_the_rest() {
    let (tmp, e) = engine().await;
    let s = e.storage();
    let book = seed_book(&e, "A").await;
    // 200 annotations of 256 quads is 51 200 quads; the file's budget is 50 000.
    let quads = vec![quad(10, 19); 256].join(" ");
    let annot = format!("/Subtype /Highlight /Rect [144 697 208.8 712] /QuadPoints [{quads}]");
    let page = SyntheticPage {
        lines: vec![(72.0, 700.0, 1, LINE.into())],
        annots: vec![annot; 200],
    };
    let p = write(tmp.path(), "many.pdf", &synthetic_annotated_pdf(&[page]));
    let scan = scan_pdf_annotations(&p).unwrap();
    assert_eq!((scan.annotations.len(), scan.unparsed), (195, 5));
    let total: usize = scan.annotations.iter().map(|a| a.quads.len()).sum();
    assert!(total <= 50_000);

    let sum = import_pdf_annotations(s, book, None, &p).await.unwrap();
    assert_eq!((sum.inserted, sum.unparsed), (195, 5));
}

#[tokio::test]
async fn rows_from_a_full_scan_survive_a_later_capped_scan() {
    let (tmp, e) = engine().await;
    let s = e.storage();
    let book = seed_book(&e, "A").await;
    let quads = vec![quad(10, 19); 256].join(" ");
    let marked = format!(
        "/Subtype /Highlight /NM (x-marks) /Rect [144 697 208.8 712] /QuadPoints [{quads}]"
    );
    let page = |annots: Vec<String>| SyntheticPage {
        lines: vec![(72.0, 700.0, 1, LINE.into())],
        annots,
    };
    let p = write(
        tmp.path(),
        "many.pdf",
        &synthetic_annotated_pdf(&[page(vec![marked.clone()])]),
    );
    let (copy, _) = copy_with_path(&e, book, &p).await;
    let first = import_pdf_annotations(s, book, Some(copy), &p)
        .await
        .unwrap();
    assert_eq!((first.inserted, first.unparsed), (1, 0));
    assert!(
        !first
            .diagnostics
            .iter()
            .any(|d| matches!(d.kind, DiagnosticKind::PdfScanIncomplete { .. })),
        "a complete scan says nothing"
    );

    let filler = format!("/Subtype /Highlight /Rect [144 697 208.8 712] /QuadPoints [{quads}]");
    let mut annots = vec![filler; 196];
    annots.push(marked);
    write(
        tmp.path(),
        "many.pdf",
        &synthetic_annotated_pdf(&[page(annots)]),
    );
    let scan = scan_pdf_annotations(&p).unwrap();
    assert!(!scan.is_complete());

    let second = import_pdf_annotations(s, book, Some(copy), &p)
        .await
        .unwrap();
    assert_eq!((second.inserted, second.unparsed), (195, 2));
    assert_eq!(second.removed, 0, "an incomplete scan removes nothing");
    assert!(
        second
            .diagnostics
            .iter()
            .any(|d| matches!(d.kind, DiagnosticKind::PdfScanIncomplete { .. }))
    );
    assert!(
        s.imported_highlights(book, HighlightOrigin::Preview)
            .await
            .unwrap()
            .iter()
            .any(|r| r.origin_id.as_deref() == Some("x-marks"))
    );
}

#[tokio::test]
async fn the_facade_adds_a_named_pdf_and_rescans_the_books_pdfs() {
    use bookmole::EngineError;
    let (tmp, e) = engine().await;
    let s = e.storage();
    let book = seed_book(&e, "A").await;
    let p = write(tmp.path(), "a.pdf", &fixture("first"));

    let sum = e.import_preview_file(book, &p).await.unwrap();
    assert_eq!(counts(&sum), (7, 0, 0, 0));
    let copies = s.copies_for_book(book).await.unwrap();
    assert_eq!(copies.len(), 1, "added as a file of the book: one copy");
    let paths = copies[0]
        .places
        .iter()
        .filter(|pl| pl.kind == PlaceKind::Path)
        .count();
    assert_eq!(paths, 1);
    let rows = s
        .imported_highlights(book, HighlightOrigin::Preview)
        .await
        .unwrap();
    assert!(rows.iter().all(|r| r.copy_id == Some(copies[0].id)));

    let again = e.import_preview_file(book, &p).await.unwrap();
    assert_eq!(counts(&again), (0, 0, 7, 0));
    assert_eq!(s.copies_for_book(book).await.unwrap().len(), 1);

    let r = e.import_preview_annotations(book).await.unwrap();
    assert_eq!(r.places.len(), 1);
    assert_eq!(r.places[0].copy_id, copies[0].id);
    assert_eq!(r.places[0].outcome, RescanOutcome::Unchanged);
    assert_eq!(r.totals.inserted, 0);

    write(tmp.path(), "a.pdf", &fixture("second"));
    bump_mtime(&p, 100);
    let r = e.import_preview_annotations(book).await.unwrap();
    assert_eq!(r.places[0].outcome, RescanOutcome::Reimported);
    assert_eq!((r.totals.updated, r.totals.unchanged), (1, 6));
    assert!(r.totals.diagnostics.is_empty());

    std::fs::remove_file(&p).unwrap();
    let r = e.import_preview_annotations(book).await.unwrap();
    assert_eq!(r.places[0].outcome, RescanOutcome::Missing);

    let missing = e
        .import_preview_file(book, &tmp.path().join("nope.pdf"))
        .await;
    assert!(
        matches!(missing, Err(EngineError::NotFound(_))),
        "{missing:?}"
    );
    let txt = write(tmp.path(), "notes.txt", b"not a pdf");
    let wrong = e.import_preview_file(book, &txt).await;
    assert!(
        matches!(wrong, Err(EngineError::InvalidInput(_))),
        "{wrong:?}"
    );
    let dir = tmp.path().join("folder.pdf");
    std::fs::create_dir_all(&dir).unwrap();
    let wrong = e.import_preview_file(book, &dir).await;
    assert!(
        matches!(wrong, Err(EngineError::InvalidInput(_))),
        "{wrong:?}"
    );
    let nobody = e.import_preview_annotations(9_999).await;
    assert!(
        matches!(nobody, Err(EngineError::NotFound(_))),
        "{nobody:?}"
    );

    let other = seed_book(&e, "B").await;
    let theirs = write(tmp.path(), "b.pdf", &fixture("third"));
    e.import_preview_file(book, &theirs).await.unwrap();
    let taken = e.import_preview_file(other, &theirs).await;
    assert!(
        matches!(taken, Err(EngineError::InvalidInput(_))),
        "{taken:?}"
    );
    assert!(s.copies_for_book(other).await.unwrap().is_empty());
}

#[tokio::test]
async fn adding_an_annotated_pdf_as_a_file_reads_its_annotations() {
    use bookmole::{FileImportOptions, FileOutcome};
    let (tmp, e) = engine().await;
    let s = e.storage();

    let book = seed_book(&e, "A").await;
    let p = write(tmp.path(), "a.pdf", &fixture("already there"));
    let report = e.add_file_to_book(book, &p).await.unwrap();
    assert_eq!(report.outcome, FileOutcome::Stored);
    let rows = s
        .imported_highlights(book, HighlightOrigin::Preview)
        .await
        .unwrap();
    assert_eq!(rows.len(), 7, "every annotation in the file, read on add");
    let copy = s.copies_for_book(book).await.unwrap()[0].id;
    assert!(rows.iter().all(|r| r.origin == HighlightOrigin::Preview));
    assert!(rows.iter().all(|r| r.copy_id == Some(copy)));
    let r = e.import_preview_annotations(book).await.unwrap();
    assert_eq!(r.places.len(), 1);
    assert_eq!(r.places[0].outcome, RescanOutcome::Unchanged);
    assert_eq!(counts(&r.totals), (0, 0, 0, 0));

    let q = write(tmp.path(), "a-second-book.pdf", &fixture("also there"));
    let report = e
        .import_file(&q, FileImportOptions { new: true })
        .await
        .unwrap();
    let made = report.book_id.unwrap();
    assert_ne!(made, book);
    let rows = s
        .imported_highlights(made, HighlightOrigin::Preview)
        .await
        .unwrap();
    assert_eq!(rows.len(), 7);
    let r = e.import_preview_annotations(made).await.unwrap();
    assert_eq!(r.places[0].outcome, RescanOutcome::Unchanged);

    write(tmp.path(), "a.pdf", &fixture("edited before the re-add"));
    e.add_file_to_book(book, &p).await.unwrap();
    let rows = s
        .imported_highlights(book, HighlightOrigin::Preview)
        .await
        .unwrap();
    assert_eq!(rows.len(), 7);
    assert!(
        rows.iter()
            .any(|r| r.origin_note.as_deref() == Some("edited before the re-add"))
    );

    let fresh = seed_book(&e, "C").await;
    let named = write(tmp.path(), "c.pdf", &fixture("named"));
    let sum = e.import_preview_file(fresh, &named).await.unwrap();
    assert_eq!(counts(&sum), (7, 0, 0, 0));
}

#[tokio::test]
async fn finding_a_moved_pdf_reads_its_annotations_into_the_same_copy() {
    let (tmp, e) = engine().await;
    let s = e.storage();
    let book = seed_book(&e, "A").await;
    let p = write(tmp.path(), "a.pdf", &fixture("before"));
    e.import_preview_file(book, &p).await.unwrap();
    let copy = s.copies_for_book(book).await.unwrap()[0].id;

    std::fs::remove_file(&p).unwrap();
    let moved_dir = tmp.path().join("moved");
    std::fs::create_dir_all(&moved_dir).unwrap();
    let q = write(&moved_dir, "a.pdf", &fixture("after the move"));

    let found = e.relocate_copy_file(copy, &q).await.unwrap();
    assert_eq!(found.id, copy);
    let paths: Vec<&str> = found
        .places
        .iter()
        .filter(|pl| pl.kind == PlaceKind::Path)
        .map(|pl| pl.reference.as_str())
        .collect();
    assert_eq!(paths, vec![q.display().to_string().as_str()]);
    assert_eq!(s.copies_for_book(book).await.unwrap().len(), 1);

    let rows = s
        .imported_highlights(book, HighlightOrigin::Preview)
        .await
        .unwrap();
    assert_eq!(rows.len(), 7, "the same highlights, not a second set");
    assert!(rows.iter().all(|r| r.copy_id == Some(copy)));
    assert!(
        rows.iter()
            .any(|r| r.origin_note.as_deref() == Some("after the move")
                || r.text == "after the move"),
        "the annotations at the new place were read"
    );

    let r = e.import_preview_annotations(book).await.unwrap();
    assert_eq!(r.places.len(), 1);
    assert_eq!(r.places[0].outcome, RescanOutcome::Unchanged);

    let sources = e.copy_highlight_sources(book).await.unwrap();
    let preview = sources
        .iter()
        .find(|s| s.origin == HighlightOrigin::Preview)
        .unwrap();
    assert_eq!(preview.copy_id, Some(copy));
    assert!(preview.last_read_at.is_some());
}
