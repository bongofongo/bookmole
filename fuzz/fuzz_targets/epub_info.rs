//! Fuzz `epub::epub_info` over arbitrary bytes.
//!
//! What this exercises is the engine's adapter (`crates/engine/src/epub.rs`)
//! and `epubkit` beneath it, reached the way an import reaches them: through
//! a file on disk, so the bytes go to a temp file first. `epubkit_package` is
//! the direct target — `epubkit` on the bytes, no temp file — and the one to
//! run for the reader itself; this one earns its place because a malformed
//! epub is a *realistic* input — users import arbitrary files — and a panic
//! anywhere on the import path crashes the app rather than reporting a bad
//! file.
//!
//! The cheap 80% of this is already covered by
//! `epub::tests::corrupt_epubs_are_rejected_rather_than_panicking`, which runs
//! on stable in CI. This target is for the long tail.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // `epub_info` takes a path, so the bytes have to reach the disk.
    let Ok(dir) = tempfile::tempdir() else { return };
    let path = dir.path().join("fuzz.epub");
    if std::fs::write(&path, data).is_err() {
        return;
    }

    if let Ok(info) = bookmole::epub::epub_info(&path) {
        // Any ISBN that comes out must already be normalized and valid —
        // epub_info is one of the entry points that feeds the library.
        if let Some(isbn) = &info.isbn {
            assert!(
                bookmole::normalize_isbn(isbn).as_deref() == Some(isbn.as_str()),
                "epub_info returned a non-normalized ISBN: {isbn:?}"
            );
        }
    }

    let _ = bookmole::epub::extract_cover(&path, &dir.path().join("images"));
});
