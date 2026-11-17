//! Fuzz `koreader::parse_sidecar`, which evaluates untrusted Lua.
//! Run with `-timeout=10`: that is how a non-terminating chunk is detected.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &str| {
    // Contract: no panic, no hang; most inputs are not valid Lua.
    if let Ok(sidecar) = bookmole::koreader::parse_sidecar(data) {
        for h in &sidecar.highlights {
            assert!(!h.text.is_empty(), "parsed an empty highlight text");
            assert!(h.pos0.is_some(), "parsed a highlight with no pos0");
            assert_eq!(h.source, "koreader");
        }
    }
});
