//! Fuzz `epubkit::Package` in memory: metadata, cover, both TOCs and every spine document.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(mut book) = epubkit::Package::from_bytes(data) else {
        return;
    };
    let md = book.metadata();
    let _ = (md.main_title(), md.publication_date(), md.unique_identifier());
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
});
