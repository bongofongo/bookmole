//! `util.partialMD5`: KOReader's file identity, an MD5 over up to twelve 1 KiB windows from 0.
//! A sampling hash, not a content hash: never a content address (sha256 is that).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use md5::{Digest, Md5};

use crate::error::{EngineError, Result};

const SAMPLE_LEN: usize = 1024;

/// The twelve window offsets, written out so the wrap-around at `i = -1` (offset 0) is visible.
pub const SAMPLE_OFFSETS: [u64; 12] = [
    0, // i = -1: lshift(1024, -2) == lshift(1024, 30) == 0 (mod 2^32)
    1 << 10,
    1 << 12,
    1 << 14,
    1 << 16,
    1 << 18,
    1 << 20,
    1 << 22,
    1 << 24,
    1 << 26,
    1 << 28,
    1 << 30,
];

pub fn partial_md5(path: &Path) -> Result<String> {
    let mut file = File::open(path).map_err(|e| io_at(path, e))?;
    let mut hasher = Md5::new();
    let mut buf = [0u8; SAMPLE_LEN];

    for offset in SAMPLE_OFFSETS {
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| io_at(path, e))?;
        let n = read_up_to(&mut file, &mut buf).map_err(|e| io_at(path, e))?;
        // A short read is still hashed (the partial sample at EOF); only a zero-length read ends the loop.
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Fills `buf` as far as the file allows; `Read::read` may return short before EOF, Lua's does not.
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

    const MD5_OF_NOTHING: &str = "d41d8cd98f00b204e9800998ecf8427e";

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

    /// Expected values come from KOReader's own loop run under LuaJIT, never from this module.
    /// The generated fourth case is the only one whose final window is a short read.
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

    fn patterned(len: usize) -> Vec<u8> {
        (0..len).map(|i| ((i * 31 + 7) % 251) as u8).collect()
    }

    fn skip(reason: &str) {
        if std::env::var("BOOKMOLE_REQUIRE_FIXTURES").is_ok() {
            panic!("REQUIRE_FIXTURES set but {reason}");
        }
        eprintln!("SKIPPED: {reason}");
    }

    #[test]
    fn boundary_sizes() {
        assert_eq!(hash_of(&[]), MD5_OF_NOTHING, "an empty file, and only it");

        for len in [1usize, 255, 256, 257, 1023] {
            let data = bytes(len, 7);
            assert_eq!(
                hash_of(&data),
                md5_of(&data),
                "a {len}-byte file is one short sample of its whole self"
            );
            assert_ne!(hash_of(&data), MD5_OF_NOTHING);
        }

        let k = bytes(1024, 8);
        assert_eq!(hash_of(&k), md5_of(&k));

        let just_over = bytes(65536 + 1, 9);
        let mut expect = Md5::new();
        for off in [0usize, 1024, 4096, 16384, 65536] {
            expect.update(&just_over[off..(off + 1024).min(just_over.len())]);
        }
        assert_eq!(hash_of(&just_over), format!("{:x}", expect.finalize()));
    }

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

#[cfg(test)]
mod tests_support {
    use super::*;
    use rand_chacha::ChaCha8Rng;
    use rand_core::{RngCore, SeedableRng};
    use std::io::Write;

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
