# Fuzzing

Three targets, run nightly (`.github/workflows/fuzz.yml`) — never on a PR,
since this needs nightly and a wall-clock budget.

```
cargo +nightly fuzz run -s none parse_sidecar   -- -timeout=10 fuzz/corpus/parse_sidecar fuzz/seeds/parse_sidecar
cargo +nightly fuzz run -s none epub_info       -- -timeout=10 fuzz/corpus/epub_info fuzz/seeds/epub_info
cargo +nightly fuzz run -s none epubkit_package -- -timeout=10 fuzz/corpus/epubkit_package fuzz/seeds/epubkit_package
```

`epubkit_package` feeds the bytes straight to `epubkit::Package::from_bytes`
— no temp file, so every execution is parsing — and then asks what opened for
everything: metadata, the cover, the navigation document and the NCX (each
entry's spine position), `toc()`, and every spine document's text. Each of
those reads its own document on demand, and each is hostile input of its own.
`epub_info` stays: it covers the engine's adapter over the same reader, path
and all. Its seeds are `Gen-Isbn-Match.epub` and `Gen-Toc-Chapters.epub`
(copied from the engine's generated fixtures) and `epub3-nav.epub` (an EPUB 3
with a nav, an NCX, a cover, a clean chapter and a sloppy one), which
`cargo test -p epubkit --test fuzz_seeds -- --ignored write_generated_seeds`
regenerates byte for byte.

## `-s none` is required, and it is measured

AddressSanitizer does not work here. mlua's vendored Lua propagates errors with
`longjmp`, which ASAN's interceptors do not survive on this platform: with ASAN
the target pins a core and makes no forward progress. Without it:

| target | execs | time |
|---|---|---|
| `parse_sidecar` | 741,558 | 26s |
| `epub_info` | 109,615 | 26s |
| `epubkit_package` | 1,640,715 | 27s (2026-11-12, from its 3 seeds, no crash) |

libFuzzer's coverage feedback, panic detection, and `-timeout` hang detection
all still work. What is lost is memory-error detection inside the C
dependencies (Lua, zlib).

## `-timeout=10` is load-bearing

It is how a non-terminating Lua chunk is detected. `StdLib::NONE` removes the
standard library but not the ability to loop, so `while true do end` in a file
off someone's e-reader is a real denial of service — bounded by
`LUA_INSTRUCTION_BUDGET` in `koreader.rs`, and this flag is what would catch a
regression in that bound.

That budget was originally 50M, which was wrong for a reason worth remembering:
it has to bite quickly under *instrumentation*, not just in a release build. At
50M the fuzzer spent minutes on a single looping input. It is now 5M — still
~50x headroom over the largest plausible real sidecar (the 5000-highlight scale
fixture is ~10^5 instructions).

## Seeds are committed; corpus and artifacts are not

`fuzz/seeds/<target>/` starts from the test fixtures and carries the inputs
earlier runs found worth keeping (the files named by their SHA-1 are libFuzzer's).
The commands above pass it **after** `fuzz/corpus/<target>/` (create it first;
it is gitignored), so a run reads the seeds and writes only to the corpus. **Every crash this ever finds should be minimized and committed
there**, because `crates/engine/tests/fuzz_seeds.rs` (`parse_sidecar`,
`epub_info`) and `crates/epubkit/tests/fuzz_seeds.rs` (`epubkit_package`, and
the `epub_info` seeds again through the same calls) replay the whole seed
directory on *stable*, in milliseconds, on every PR. That replay is what makes
fuzzing pay off over time — the nightly job finds things, the seed replay keeps
them found. A fuzzer that has to be re-run to re-catch its own findings catches
them once.
