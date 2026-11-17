//! Fuzz `epub::epub_info` over arbitrary bytes, written to a temp file as an import reads them.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(dir) = tempfile::tempdir() else { return };
    let path = dir.path().join("fuzz.epub");
    if std::fs::write(&path, data).is_err() {
        return;
    }

    if let Ok(info) = bookmole::epub::epub_info(&path) {
        if let Some(isbn) = &info.isbn {
            assert!(
                bookmole::normalize_isbn(isbn).as_deref() == Some(isbn.as_str()),
                "epub_info returned a non-normalized ISBN: {isbn:?}"
            );
        }
    }

    let _ = bookmole::epub::extract_cover(&path, &dir.path().join("images"));
});
