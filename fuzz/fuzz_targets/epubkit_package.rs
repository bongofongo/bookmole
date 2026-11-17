//! Fuzz `epubkit::Package` over arbitrary bytes, in memory.
//!
//! Unlike `epub_info`, this needs no temp file: `Package::from_bytes` opens
//! the archive straight from the fuzzer's buffer, so every execution is
//! parsing rather than disk I/O. Whatever opens is then asked everything a
//! caller can ask — metadata, the cover, both tables of contents flattened,
//! every spine document and its text — because each of those reads its own
//! document on demand and each is hostile input in its own right.
//!
//! `crates/epubkit/tests/fuzz_seeds.rs` replays `fuzz/seeds/epubkit_package/`
//! through the same calls on stable, on every PR.

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
