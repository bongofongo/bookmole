//! `util.partialMD5` — KOReader's file identity, computed over our own files.
//!
//! The device names a file by an MD5 taken over at most twelve 1024-byte
//! windows rather than over the whole file, and stores the result as a
//! sidecar's root `partial_md5_checksum`. Computing the *same* value here is
//! what lets our file identity agree with the device's: an epub imported by
//! [`crate::Engine::import_epub`] is matched by [`crate::MatchMethod::Md5`] the
//! first time its sidecar arrives, with no fuzzy title guess and no dependence
//! on the sidecar living beside its book.
//!
//! **This is a sampling hash, not a content hash.** Two files identical at
//! those twelve windows collide, however different they are everywhere else.
//! That is fine for its three jobs — dedup, sidecar↔book matching, and joining
//! into the device's `statistics.sqlite3` — and it is *not* fine as a content
//! address, which is sha256's job (owned files are stored under their
//! sha256). Do not reach for this function because it is the cheap one.
//!
//! # The algorithm, and the one thing everyone gets wrong
//!
//! KOReader's `frontend/util.lua:1111-1128` (its core loop; the function
//! around it opens the file, and returns the MD5 of whatever was sampled):
//!
//! ```lua
//! local step, size = 1024, 1024
//! local update = md5()
//! for i = -1, 10 do
//!     file:seek("set", lshift(step, 2*i))
//!     local sample = file:read(size)
//!     if sample then update(sample) else break end
//! end
//! ```
//!
//! Three details, each of which produces a plausible-looking hash that the
//! device disagrees with:
//!
//! 1. **The first offset is 0, not 256.** `lshift` is LuaJIT's BitOp, whose
//!    shift count is taken **modulo 32** on 32-bit integers — it is not an
//!    arithmetic shift and a negative count is not a right shift. So
//!    `lshift(1024, -2)` is `1024 << 30` truncated to 32 bits, which is `0`.
//!    Reading `2*i = -2` as a right shift gives 256, and 256 is wrong:
//!    checksums KOReader wrote on a real reader matched offset 0 and none
//!    matched 256, which is how this was settled — the device is the
//!    authority here, not the reading of the source. `agrees_with_the_device`
//!    below pins it against KOReader's own loop run under LuaJIT.
//! 2. **Break on a zero-length read, not on a short one.** Lua's
//!    `file:read(size)` returns the partial string at EOF, and that string *is*
//!    hashed; the loop ends on the *next* iteration, when the read returns
//!    `nil`. So a short read still contributes. A seek past EOF is legal and
//!    simply yields nothing.
//! 3. **An empty file hashes to the MD5 of nothing** —
//!    `d41d8cd98f00b204e9800998ecf8427e`. Nothing else does: because the first
//!    window starts at 0, every file with at least one byte contributes it.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use md5::{Digest, Md5};

use crate::error::{EngineError, Result};

/// Bytes read at each offset. `size` in the Lua.
const SAMPLE_LEN: usize = 1024;

/// The twelve window offsets, in order: 0, 1 Ki, 4 Ki, 16 Ki, 64 Ki, 256 Ki,
/// 1 Mi, 4 Mi, 16 Mi, 64 Mi, 256 Mi, 1 Gi.
///
/// Written out rather than computed so the wrap-around at `i = -1` is visible
/// instead of implied; [`props::the_offsets_are_what_bitop_produces`] checks
/// the table against the Lua semantics it came from.
pub const SAMPLE_OFFSETS: [u64; 12] = [
    0,       // i = -1: lshift(1024, -2) == lshift(1024, 30) == 0 (mod 2^32)
    1 << 10, // 1 Ki
    1 << 12, // 4 Ki
    1 << 14, // 16 Ki
    1 << 16, // 64 Ki
    1 << 18, // 256 Ki
    1 << 20, // 1 Mi
    1 << 22, // 4 Mi
    1 << 24, // 16 Mi
    1 << 26, // 64 Mi
    1 << 28, // 256 Mi
    1 << 30, // 1 Gi
];

/// KOReader's `partialMD5` for a local file, as 32 lowercase hex characters.
///
/// Never reads more than [`SAMPLE_LEN`] bytes at a time, so a 4 GB file costs
/// the same twelve reads as a 4 KB one.
pub fn partial_md5(path: &Path) -> Result<String> {
    let mut file = File::open(path).map_err(|e| io_at(path, e))?;
    let mut hasher = Md5::new();
    let mut buf = [0u8; SAMPLE_LEN];

    for offset in SAMPLE_OFFSETS {
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| io_at(path, e))?;
        let n = read_up_to(&mut file, &mut buf).map_err(|e| io_at(path, e))?;
        // A *short* read is still a read — it is the partial sample at EOF, and
        // the device hashes it. Only nothing at all ends the loop.
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Fill `buf` as far as the file allows, returning how many bytes arrived.
///
/// `Read::read` is permitted to return fewer bytes than asked for without being
/// at EOF, and Lua's `file:read(n)` is not — so the loop is what makes the two
/// agree, not a nicety.
fn read_up_to(file: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// An I/O failure names the file it happened to. A bare `io error: No such
/// file or directory` is useless in a report that walked a whole library.
fn io_at(path: &Path, e: std::io::Error) -> EngineError {
    EngineError::Io(std::io::Error::new(
        e.kind(),
        format!("{}: {e}", path.display()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand_chacha::ChaCha8Rng;
    use rand_core::{RngCore, SeedableRng};
    use std::io::Write;

    /// MD5 of the empty input.
    const MD5_OF_NOTHING: &str = "d41d8cd98f00b204e9800998ecf8427e";

    /// Deterministic bytes — no committed binary fixture, and no `StdRng`.
    fn bytes(len: usize, seed: u64) -> Vec<u8> {
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        let mut v = vec![0u8; len];
        rng.fill_bytes(&mut v);
        v
    }

    fn write_file(dir: &Path, name: &str, data: &[u8]) -> std::path::PathBuf {
        let p = dir.join(name);
        let mut f = File::create(&p).unwrap();
        f.write_all(data).unwrap();
        p
    }

    fn hash_of(data: &[u8]) -> String {
        let dir = tempfile::tempdir().unwrap();
        let p = write_file(dir.path(), "f.bin", data);
        partial_md5(&p).unwrap()
    }

    /// **The only test that proves we agree with the device's algorithm.**
    /// Every other one proves we agree with ourselves.
    ///
    /// The expected values were not computed by this module. They come from
    /// KOReader's own loop run under **LuaJIT**, whose `bit.lshift` is the
    /// BitOp the device runs (so the wrap-around at `i = -1` is LuaJIT's, not
    /// our reading of it), with the samples piped to an unrelated MD5:
    ///
    /// ```text
    /// -- partial_samples.lua: frontend/util.lua's loop, writing the samples
    /// -- to stdout instead of feeding them to ffi/sha2's md5
    /// local lshift = bit.lshift
    /// local file = assert(io.open(arg[1], "rb"))
    /// local step, size = 1024, 1024
    /// for i = -1, 10 do
    ///     file:seek("set", lshift(step, 2*i))
    ///     local sample = file:read(size)
    ///     if sample then io.write(sample) else break end
    /// end
    ///
    /// $ luajit partial_samples.lua corpus/epub/100.epub | openssl dgst -md5
    /// ```
    ///
    /// The three books are Project Gutenberg epubs from the committed tier-2
    /// corpus, chosen so the last window reached differs: 1 Mi (100, 2.8 MB),
    /// 256 Ki (1342, 558 KB) and 16 Ki (24039, 62 KB). The corpus generator's
    /// own independent `partial_md5` (`crates/corpus/src/gutenberg.rs`) wrote
    /// the same three values into the sidecars under `corpus/generated/`. The
    /// fourth case is generated bytes, `(i * 31 + 7) % 251` for 65 836 bytes,
    /// written by LuaJIT for the oracle and by [`patterned`] here: the only
    /// case whose final window is a *short* read (300 bytes at 64 Ki).
    ///
    /// The corpus is committed, so an absent book is a broken checkout; it
    /// still skips loudly and `BOOKMOLE_REQUIRE_FIXTURES=1` refuses.
    #[test]
    fn agrees_with_the_device() {
        const BOOKS: [(&str, &str); 3] = [
            ("100.epub", "88132b87d577855d5decb5f8015e2a88"),
            ("1342.epub", "2b2f3835b1fadc176a340f7f66ca6842"),
            ("24039.epub", "ccae712b7db49322b18b7139d6b638b1"),
        ];
        let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/epub");
        for (name, expected) in BOOKS {
            let path = corpus.join(name);
            if !path.is_file() {
                skip(&format!("{} is absent", path.display()));
                continue;
            }
            assert_eq!(
                partial_md5(&path).unwrap(),
                expected,
                "{} disagrees with KOReader's partialMD5 run under LuaJIT",
                path.display()
            );
        }

        assert_eq!(
            hash_of(&patterned(65_836)),
            "b88d5560d0d947c65540ea0838ae48e9",
            "a short final window disagrees with KOReader's partialMD5"
        );
    }

    /// `(i * 31 + 7) % 251` — trivially reproducible in Lua, which is the
    /// point: the oracle above had to write the same bytes.
    fn patterned(len: usize) -> Vec<u8> {
        (0..len).map(|i| ((i * 31 + 7) % 251) as u8).collect()
    }

    /// A skip must be loud and refusable — a test that quietly returns is green
    /// without having asserted anything. Same rule as `epub.rs`.
    fn skip(reason: &str) {
        if std::env::var("BOOKMOLE_REQUIRE_FIXTURES").is_ok() {
            panic!("REQUIRE_FIXTURES set but {reason}");
        }
        eprintln!("SKIPPED: {reason}");
    }

    /// The one size that hashes to nothing, and the sizes around the boundary
    /// that the "first offset is 256" misreading gets wrong.
    #[test]
    fn boundary_sizes() {
        assert_eq!(hash_of(&[]), MD5_OF_NOTHING, "an empty file, and only it");

        // Under 1024 bytes: one short read at offset 0, then a seek past EOF
        // ends it. Every one of these hashes its own whole content — which is
        // exactly what the 256-offset misreading would get wrong, since it
        // would hash nothing at all below 256 bytes.
        for len in [1usize, 255, 256, 257, 1023] {
            let data = bytes(len, 7);
            assert_eq!(
                hash_of(&data),
                md5_of(&data),
                "a {len}-byte file is one short sample of its whole self"
            );
            assert_ne!(hash_of(&data), MD5_OF_NOTHING);
        }

        // Exactly one full window, and nothing at 1 Ki to read after it.
        let k = bytes(1024, 8);
        assert_eq!(hash_of(&k), md5_of(&k));

        // Exactly on a sample offset: 64 Ki + 1 byte means the window at 64 Ki
        // is a single byte, and the loop still stops at 256 Ki.
        let just_over = bytes(65536 + 1, 9);
        let mut expect = Md5::new();
        for off in [0usize, 1024, 4096, 16384, 65536] {
            expect.update(&just_over[off..(off + 1024).min(just_over.len())]);
        }
        assert_eq!(hash_of(&just_over), format!("{:x}", expect.finalize()));
    }

    /// Windows past EOF contribute nothing, so appending after the last one
    /// cannot change the hash however much is appended.
    #[test]
    fn growth_beyond_the_last_reached_window_is_invisible() {
        let base = bytes(70_000, 11);
        let mut grown = base.clone();
        grown.extend_from_slice(&bytes(50_000, 12));
        assert_eq!(hash_of(&base), hash_of(&grown));
    }

    fn md5_of(data: &[u8]) -> String {
        let mut h = Md5::new();
        h.update(data);
        format!("{:x}", h.finalize())
    }
}

#[cfg(test)]
mod props {
    use super::tests_support::*;
    use super::*;
    use proptest::prelude::*;

    /// The table against the Lua it was transcribed from: BitOp's `lshift` on
    /// 32-bit integers, shift count taken modulo 32. This is the whole reason
    /// the first offset is 0, so it is worth a machine check rather than a
    /// comment.
    #[test]
    fn the_offsets_are_what_bitop_produces() {
        let bitop_lshift = |b: u32, n: i32| -> u32 { b.wrapping_shl(n.rem_euclid(32) as u32) };
        let derived: Vec<u64> = (-1..=10)
            .map(|i| u64::from(bitop_lshift(1024, 2 * i)))
            .collect();
        assert_eq!(derived, SAMPLE_OFFSETS.to_vec());
        assert_eq!(derived[0], 0, "not 256 — see the module docs");
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// **The hash sees its windows and nothing else.** Flipping a byte
        /// inside a window that the file is long enough to reach must change
        /// the hash; flipping one anywhere else must not.
        ///
        /// This pins the algorithm without pinning it to our own output, which
        /// a golden string would do — a golden agrees with whatever we happen
        /// to compute, including the wrong thing.
        #[test]
        fn only_the_windows_are_hashed(pos in 0usize..80_000) {
            let base = payload();
            let before = hash_bytes(&base);

            let mut flipped = base.clone();
            flipped[pos] ^= 0xff;
            let after = hash_bytes(&flipped);

            let inside = SAMPLE_OFFSETS
                .iter()
                .any(|&o| (o as usize) < base.len()
                    && pos >= o as usize
                    && pos < o as usize + 1024);
            prop_assert_eq!(before == after, !inside, "position {}", pos);
        }
    }
}

/// Shared between the two test modules; `#[cfg(test)]` so it costs nothing.
#[cfg(test)]
mod tests_support {
    use super::*;
    use rand_chacha::ChaCha8Rng;
    use rand_core::{RngCore, SeedableRng};
    use std::io::Write;

    /// 80 000 bytes: long enough to reach the 64 Ki window (and its full 1024
    /// bytes), short enough that the 256 Ki one is never read. Generated once
    /// per call and deterministic, so a failing case reproduces.
    pub fn payload() -> Vec<u8> {
        let mut rng = ChaCha8Rng::seed_from_u64(0xB0_0C);
        let mut v = vec![0u8; 80_000];
        rng.fill_bytes(&mut v);
        v
    }

    pub fn hash_bytes(data: &[u8]) -> String {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.bin");
        File::create(&p).unwrap().write_all(data).unwrap();
        partial_md5(&p).unwrap()
    }
}
