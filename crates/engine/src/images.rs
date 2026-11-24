//! Cover files on disk, named by the sha256 of their content so writes are idempotent. Two books
//! may share a file, so deletes check references first. Large covers also get a thumbnail.

use std::path::{Path, PathBuf};

use image::RgbImage;
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use sha2::{Digest, Sha256};

mod fetch;

pub(crate) use fetch::CoverClient;
pub use fetch::CoverRefusal;

use crate::error::{EngineError, Result};

/// Longest edge of the shelf tier; a cover at or below this in both dimensions is its own
/// thumbnail and no second file is written.
pub const THUMB_MAX: u32 = 400;

const THUMB_QUALITY: u8 = 82;

/// Measured by the engine when it wrote the file; not a provider claim, so not a `MERGE_RULES`
/// column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoverMetrics {
    pub width: i64,
    pub height: i64,
    /// Median RGB of a 2px frame around the image, packed `0xRRGGBB`. Unclamped: the luma clamp is the
    /// renderer's policy, not a fact about the file.
    pub accent: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverFile {
    pub path: PathBuf,
    /// The downscaled sibling, or `None` when the cover is already small enough; not a decode failure.
    pub thumb_path: Option<PathBuf>,
    /// `None` when the bytes cannot be decoded. The file is still stored.
    pub metrics: Option<CoverMetrics>,
}

/// Where the shelf tier of `cover` lives, derived from its name (a hash with exactly one `.`) so
/// a caller holding only a `cover_path` can clean up both files.
pub fn thumb_path_of(cover: &Path) -> PathBuf {
    let mut p = cover.to_path_buf();
    p.set_extension("thumb.jpg");
    p
}

fn filename_for(bytes: &[u8]) -> Result<String> {
    let hash = hex(Sha256::digest(bytes).as_slice());
    let ext = image::guess_format(bytes)
        .ok()
        .and_then(|f| f.extensions_str().first().copied())
        .unwrap_or("img");
    let fname = format!("{hash}.{ext}");

    let p = Path::new(&fname);
    let plain = p.components().count() == 1 && p.file_name().is_some_and(|n| n == fname.as_str());
    if !plain {
        return Err(EngineError::InvalidInput(format!(
            "refusing image filename: {fname:?}"
        )));
    }
    Ok(fname)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn store_cover(bytes: &[u8], images_dir: &Path) -> Result<CoverFile> {
    if bytes.is_empty() {
        return Err(EngineError::InvalidInput("empty image bytes".into()));
    }
    let fname = filename_for(bytes)?;
    std::fs::create_dir_all(images_dir)?;
    let path = images_dir.join(fname);
    if let Ok(meta) = std::fs::metadata(&path) {
        // An old file reused is made fresh, so `sweep_unused` cannot take it before a row points
        // at it; a recent one is left alone, so storing twice writes nothing.
        let now = std::time::SystemTime::now();
        if meta.modified().is_ok_and(|m| m + REUSE_FRESHEN < now) {
            std::fs::File::options()
                .write(true)
                .open(&path)?
                .set_modified(now)?;
        }
    } else {
        std::fs::write(&path, bytes)?;
    }

    let Some(img) = decode(bytes) else {
        return Ok(CoverFile {
            path,
            thumb_path: None,
            metrics: None,
        });
    };
    let metrics = CoverMetrics {
        width: i64::from(img.width()),
        height: i64::from(img.height()),
        accent: accent_from_border(&img),
    };
    let thumb_path = write_thumb(&img, &path)?;
    Ok(CoverFile {
        path,
        thumb_path,
        metrics: Some(metrics),
    })
}

/// The longest side of a cover that is decoded. What covers carry: Open Library's large size is
/// about 500 by 800, Google Books' largest about 1,280 by 1,900, Apple Books asks for 1,400 wide,
/// and Kindle's recommended jacket is 1,600 by 2,560; print-resolution EPUB covers reach about
/// 3,000 by 4,500. Anything past that is refused rather than decoded.
pub const COVER_SIDE_MAX: u32 = 4_500;
/// At most 12 megapixels whatever the sides (3,000 by 4,000), so decoding and thumbnailing the
/// largest cover takes a fraction of a second in a release build.
pub const COVER_PIXELS_MAX: u64 = 12_000_000;
/// What a decoder may allocate for one cover: 12 megapixels at four bytes with room to spare. A
/// 16-bit image of the largest size is refused here, though its header passes.
pub const COVER_ALLOC_MAX: u64 = 64 << 20;

fn limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(COVER_SIDE_MAX);
    limits.max_image_height = Some(COVER_SIDE_MAX);
    limits.max_alloc = Some(COVER_ALLOC_MAX);
    limits
}

/// A cover's width and height as its header states them, read without decoding a pixel; `None`
/// for bytes that are not an image this build reads.
pub(crate) fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

/// Whether stated dimensions are within what a cover may be.
pub(crate) fn within_limits((width, height): (u32, u32)) -> bool {
    width > 0
        && height > 0
        && width <= COVER_SIDE_MAX
        && height <= COVER_SIDE_MAX
        && u64::from(width) * u64::from(height) <= COVER_PIXELS_MAX
}

/// Why a cover was not decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Undecoded {
    /// Not an image this build reads.
    Unknown,
    /// Its header states dimensions past the limits.
    Oversized,
    /// The decoder would allocate more than [`COVER_ALLOC_MAX`], or the dimensions it found
    /// exceed the limits.
    OverLimits,
    /// It does not decode.
    Corrupt,
}

/// The only place a cover is decoded. Both bounds are kept, as neither covers the other: the
/// header check refuses a 16-megapixel 8-bit image the decoder's limits would let through, and
/// the limits refuse what a header understates, such as a 16-bit image of an allowed size. An
/// animated image is its first frame.
pub(crate) fn decode_checked(bytes: &[u8]) -> std::result::Result<RgbImage, Undecoded> {
    let size = dimensions(bytes).ok_or(Undecoded::Unknown)?;
    if !within_limits(size) {
        return Err(Undecoded::Oversized);
    }
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| Undecoded::Unknown)?;
    reader.limits(limits());
    let img = reader.decode().map_err(|e| match e {
        image::ImageError::Limits(_) => Undecoded::OverLimits,
        _ => Undecoded::Corrupt,
    })?;
    let img = img.to_rgb8();
    if img.width() == 0 || img.height() == 0 {
        return Err(Undecoded::Corrupt);
    }
    Ok(img)
}

fn decode(bytes: &[u8]) -> Option<RgbImage> {
    decode_checked(bytes).ok()
}

fn write_thumb(img: &RgbImage, cover: &Path) -> Result<Option<PathBuf>> {
    if img.width() <= THUMB_MAX && img.height() <= THUMB_MAX {
        return Ok(None);
    }
    let dest = thumb_path_of(cover);
    if dest.exists() {
        return Ok(Some(dest));
    }
    // Lanczos on the way down: point-sampling a 1200px jacket to 400 aliases the type.
    let scale = f64::from(THUMB_MAX) / f64::from(img.width().max(img.height()));
    let w = ((f64::from(img.width()) * scale).round() as u32).max(1);
    let h = ((f64::from(img.height()) * scale).round() as u32).max(1);
    let small = image::imageops::resize(img, w, h, FilterType::Lanczos3);

    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, THUMB_QUALITY)
        .encode(small.as_raw(), w, h, image::ExtendedColorType::Rgb8)
        .map_err(|e| EngineError::Other(format!("encoding cover thumbnail: {e}")))?;
    std::fs::write(&dest, &out)?;
    Ok(Some(dest))
}

/// Median of a 2px border frame, packed `0xRRGGBB` (a mean would be moved by artwork bleed).
/// The only copy: the 3D renderer reads `books.cover_accent`; do not add another.
fn accent_from_border(img: &RgbImage) -> i64 {
    let (w, h) = (img.width() as i64, img.height() as i64);
    let border = 2i64.min(w / 4).max(1);
    let mut chans: [Vec<u8>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    for y in 0..h {
        for x in 0..w {
            let edge = x < border || y < border || x >= w - border || y >= h - border;
            if !edge {
                continue;
            }
            let p = img.get_pixel(x as u32, y as u32);
            for c in 0..3 {
                chans[c].push(p[c]);
            }
        }
    }
    let mut packed: i64 = 0;
    for chan in &mut chans {
        chan.sort_unstable();
        let med = if chan.is_empty() {
            128
        } else {
            chan[chan.len() / 2]
        };
        packed = (packed << 8) | i64::from(med);
    }
    packed
}

pub fn accent_channels(accent: i64) -> [u8; 3] {
    [
        ((accent >> 16) & 0xff) as u8,
        ((accent >> 8) & 0xff) as u8,
        (accent & 0xff) as u8,
    ]
}

/// How old a stored cover may be before storing the same bytes again marks it fresh; well inside
/// the hour a sweep leaves a fresh file alone.
const REUSE_FRESHEN: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Whether `name` is one this module gives a cover or its thumbnail: a sha256 in hex with an
/// image extension, or a calibre copy.
fn generated_name(name: &str) -> bool {
    let (stem, ext) = name.split_once('.').unwrap_or((name, ""));
    let hashed = stem.len() == 64 && stem.bytes().all(|b| b.is_ascii_hexdigit());
    let calibre = stem.strip_prefix("calibre-").is_some_and(|rest| {
        !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    });
    let plain_ext = !ext.is_empty() && ext.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.');
    (hashed || calibre) && plain_ext
}

/// Removes cover files in `images_dir` that no book uses: only names this module generates,
/// only files last written more than `settled` ago (a cover is stored before its row points at
/// it), and never anything in `used`. Returns how many went. One directory listing; a file that
/// cannot be read or removed is left.
pub(crate) fn sweep_unused(
    images_dir: &Path,
    used: &std::collections::HashSet<String>,
    settled: std::time::Duration,
) -> Result<usize> {
    let entries = match std::fs::read_dir(images_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e.into()),
    };
    let cutoff = std::time::SystemTime::now()
        .checked_sub(settled)
        .unwrap_or(std::time::UNIX_EPOCH);
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if used.contains(name) || !generated_name(name) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() || meta.modified().map_or(true, |m| m > cutoff) {
            continue;
        }
        if std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

/// Re-measures a cover on disk. Reads the bytes (an old name is not its hash) and never renames:
/// the stored `cover_path` is what frontends resolve, so its shape is a contract.
pub fn measure_stored(cover: &Path) -> Result<CoverFile> {
    let bytes = std::fs::read(cover)?;
    let Some(img) = decode(&bytes) else {
        return Ok(CoverFile {
            path: cover.to_path_buf(),
            thumb_path: None,
            metrics: None,
        });
    };
    let metrics = CoverMetrics {
        width: i64::from(img.width()),
        height: i64::from(img.height()),
        accent: accent_from_border(&img),
    };
    let thumb_path = write_thumb(&img, cover)?;
    Ok(CoverFile {
        path: cover.to_path_buf(),
        thumb_path,
        metrics: Some(metrics),
    })
}

/// Where a stored cover path should point in this library (relative or moved-library paths are
/// re-rooted by file name), or `None`. A relative `images_dir` re-roots nothing.
pub(crate) fn rerooted(stored: &str, images_dir: &Path) -> Option<PathBuf> {
    if !images_dir.is_absolute() {
        return None;
    }
    let path = Path::new(stored);
    if path.is_absolute() && path.starts_with(images_dir) {
        return None;
    }
    let candidate = images_dir.join(path.file_name()?);
    candidate.is_file().then_some(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, Rgb};
    use url::Url;

    #[test]
    fn a_cover_is_pointed_into_this_librarys_own_folder() {
        let lib = tempfile::tempdir().unwrap();
        let images = lib.path().join("images");
        std::fs::create_dir_all(&images).unwrap();
        std::fs::write(images.join("a.jpg"), b"x").unwrap();
        let here = images.join("a.jpg");

        assert_eq!(
            rerooted("./database/images/a.jpg", &images),
            Some(here.clone())
        );
        assert_eq!(
            rerooted("/elsewhere/database/images/a.jpg", &images),
            Some(here.clone())
        );
        assert_eq!(rerooted(&here.display().to_string(), &images), None);
        assert_eq!(rerooted("/elsewhere/b.jpg", &images), None);
        assert_eq!(
            rerooted("./database/images/a.jpg", Path::new("images")),
            None
        );
    }

    fn png(w: u32, h: u32, seed: u8) -> Vec<u8> {
        let img = RgbImage::from_fn(w, h, |x, y| {
            let edge = x < 2 || y < 2 || x + 2 >= w || y + 2 >= h;
            if edge {
                Rgb([160, 32, 32])
            } else {
                Rgb([seed, 250, 250])
            }
        });
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn a_name_is_the_hash_and_the_real_format() {
        let bytes = png(8, 8, 7);
        let name = filename_for(&bytes).unwrap();
        let (stem, ext) = name.split_once('.').unwrap();
        assert_eq!(stem.len(), 64, "{name}");
        assert!(stem.chars().all(|c| c.is_ascii_hexdigit()), "{name}");
        assert_eq!(ext, "png");
        assert_eq!(filename_for(b"not an image at all").unwrap().len(), 64 + 4);
    }

    #[test]
    fn a_remote_url_can_never_escape_the_images_dir() {
        let dir = Path::new("/data/images");
        for u in [
            "https://x.test/../../etc/passwd",
            "https://x.test/a/../../../../etc/passwd",
            "https://x.test/%2e%2e%2f%2e%2e%2fetc/passwd",
            "https://x.test/foo/%2Fetc%2Fpasswd",
            "https://books.google.com/books/content?id=../../../etc/passwd",
        ] {
            assert!(Url::parse(u).is_ok());
            let name = filename_for(&png(4, 4, 1)).unwrap();
            let joined = dir.join(&name);
            assert_eq!(joined.parent(), Some(dir), "{u} produced {name:?}");
            assert!(!name.contains('/'), "{u} produced a separator: {name:?}");
            assert!(!name.contains(".."), "{u} produced a climb: {name:?}");
        }
    }

    #[test]
    fn two_google_books_covers_are_two_files() {
        let dir = tempfile::tempdir().unwrap();
        let images = dir.path().join("images");

        let one = "https://books.google.com/books/content?id=AAA&printsec=frontcover&img=1";
        let two = "https://books.google.com/books/content?id=BBB&printsec=frontcover&img=1";
        assert_eq!(
            Url::parse(one)
                .unwrap()
                .path_segments()
                .unwrap()
                .next_back(),
            Url::parse(two)
                .unwrap()
                .path_segments()
                .unwrap()
                .next_back(),
            "the premise: these two URLs used to name the same file"
        );

        let a = store_cover(&png(8, 8, 10), &images).unwrap();
        let b = store_cover(&png(8, 8, 200), &images).unwrap();
        assert_ne!(a.path, b.path, "two covers, one filename");
        assert!(a.path.exists() && b.path.exists());
        assert_eq!(
            std::fs::read_dir(&images).unwrap().count(),
            2,
            "two books, two covers on disk"
        );

        let c = store_cover(&png(9, 9, 10), &images).unwrap();
        assert_ne!(a.path, c.path);
    }

    #[test]
    fn storing_the_same_bytes_twice_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let images = dir.path().join("images");
        let bytes = png(600, 900, 3);

        let first = store_cover(&bytes, &images).unwrap();
        let mtime = std::fs::metadata(&first.path).unwrap().modified().unwrap();
        let second = store_cover(&bytes, &images).unwrap();

        assert_eq!(first, second);
        assert_eq!(
            std::fs::metadata(&second.path).unwrap().modified().unwrap(),
            mtime,
            "the file was rewritten, so the write is not idempotent"
        );
    }

    #[test]
    fn a_measured_cover_carries_its_size_and_its_border_colour() {
        let dir = tempfile::tempdir().unwrap();
        let stored = store_cover(&png(60, 90, 0), &dir.path().join("images")).unwrap();
        let m = stored.metrics.expect("a png is decodable");
        assert_eq!((m.width, m.height), (60, 90));

        let [r, g, b] = accent_channels(m.accent);
        assert_eq!([r, g, b], [160, 32, 32]);
        assert_eq!(m.accent, 0x00a0_2020);
    }

    #[test]
    fn only_a_cover_bigger_than_the_shelf_gets_a_second_tier() {
        let dir = tempfile::tempdir().unwrap();
        let images = dir.path().join("images");

        let small = store_cover(&png(120, 180, 1), &images).unwrap();
        assert_eq!(small.thumb_path, None, "a small cover is its own thumbnail");

        let big = store_cover(&png(800, 1200, 2), &images).unwrap();
        let thumb = big.thumb_path.expect("a large cover gets a shelf tier");
        assert_eq!(thumb, thumb_path_of(&big.path));
        assert!(thumb.exists());
        let t = image::open(&thumb).unwrap();
        assert_eq!((t.width(), t.height()), (267, 400));
        assert!(
            std::fs::metadata(&thumb).unwrap().len() < std::fs::metadata(&big.path).unwrap().len()
        );
    }

    #[test]
    fn bytes_that_are_not_an_image_are_still_kept() {
        let dir = tempfile::tempdir().unwrap();
        let stored = store_cover(b"<svg/>", &dir.path().join("images")).unwrap();
        assert!(
            stored.path.exists(),
            "a cover we cannot read is not deleted"
        );
        assert_eq!(stored.metrics, None);
        assert_eq!(stored.thumb_path, None);
    }

    #[test]
    fn measuring_a_file_in_place_leaves_its_name_alone() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("content");
        std::fs::write(&legacy, png(500, 700, 4)).unwrap();

        let measured = measure_stored(&legacy).unwrap();
        assert_eq!(measured.path, legacy, "the stored path must not move");
        assert_eq!(measured.metrics.unwrap().width, 500);
        assert!(measured.thumb_path.unwrap().exists());
    }

    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &b in bytes {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xedb8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    /// A one-pixel PNG whose header claims `width` by `height`, 8-bit RGB.
    fn png_claiming(width: u32, height: u32) -> Vec<u8> {
        png_header(width, height, None)
    }

    /// A one-pixel PNG whose header claims `width` by `height`, and, when given, a bit depth
    /// and colour type.
    fn png_header(width: u32, height: u32, depth_and_colour: Option<(u8, u8)>) -> Vec<u8> {
        let mut bytes = png(1, 1, 7);
        let ihdr = 8 + 4;
        bytes[ihdr + 4..ihdr + 8].copy_from_slice(&width.to_be_bytes());
        bytes[ihdr + 8..ihdr + 12].copy_from_slice(&height.to_be_bytes());
        if let Some((depth, colour)) = depth_and_colour {
            bytes[ihdr + 12] = depth;
            bytes[ihdr + 13] = colour;
        }
        let crc = crc32(&bytes[ihdr..ihdr + 4 + 13]);
        bytes[ihdr + 4 + 13..ihdr + 4 + 17].copy_from_slice(&crc.to_be_bytes());
        bytes
    }

    #[test]
    fn each_bound_refuses_what_the_other_lets_through() {
        // Sides within, 16 megapixels: the decoder would need 48 MB, which its limits allow.
        assert_eq!(
            decode_checked(&png_claiming(4_000, 4_000)),
            Err(Undecoded::Oversized)
        );
        // 9 megapixels, as its header allows, but 16-bit RGBA: 72 MB, past the decoder's limit.
        assert_eq!(
            decode_checked(&png_header(3_000, 3_000, Some((16, 6)))),
            Err(Undecoded::OverLimits)
        );
        assert_eq!(decode_checked(b"not an image"), Err(Undecoded::Unknown));
    }

    /// A GIF of `frames` 2x2 frames, its screen then made to claim `width` by `height`.
    fn gif(width: u16, height: u16, frames: usize) -> Vec<u8> {
        use image::codecs::gif::GifEncoder;
        use image::{Delay, Frame, RgbaImage};

        let mut out = Vec::new();
        {
            let mut encoder = GifEncoder::new(&mut out);
            for n in 0..frames {
                let shade = 40 * n as u8;
                let frame = Frame::from_parts(
                    RgbaImage::from_pixel(2, 2, image::Rgba([shade, 0, 0, 255])),
                    0,
                    0,
                    Delay::from_numer_denom_ms(100, 1),
                );
                encoder.encode_frame(frame).unwrap();
            }
        }
        out[6..8].copy_from_slice(&width.to_le_bytes());
        out[8..10].copy_from_slice(&height.to_le_bytes());
        out
    }

    #[test]
    fn a_small_file_claiming_huge_dimensions_is_never_decoded() {
        let dir = tempfile::tempdir().unwrap();
        for (bytes, claimed) in [
            (png_claiming(100_000, 100_000), (100_000, 100_000)),
            (
                png_claiming(COVER_SIDE_MAX + 1, 10),
                (COVER_SIDE_MAX + 1, 10),
            ),
            (png_claiming(4_000, 3_001), (4_000, 3_001)),
            (gif(u16::MAX, u16::MAX, 1), (65_535, 65_535)),
        ] {
            assert!(bytes.len() < 1_000);
            assert_eq!(dimensions(&bytes), Some(claimed));
            assert!(!within_limits(claimed));
            assert_eq!(decode_checked(&bytes), Err(Undecoded::Oversized));
            let stored = store_cover(&bytes, dir.path()).unwrap();
            assert!(stored.metrics.is_none() && stored.thumb_path.is_none());
        }
    }

    #[test]
    fn an_animated_cover_is_its_first_frame_within_the_limits() {
        let bytes = gif(2, 2, 3);
        assert_eq!(dimensions(&bytes), Some((2, 2)));
        let img = decode(&bytes).unwrap();
        assert_eq!((img.width(), img.height()), (2, 2));
    }

    #[test]
    fn a_cover_within_the_limits_decodes_as_it_always_did() {
        for bytes in [png(800, 1200, 2), png(60, 90, 0)] {
            let before = image::load_from_memory(&bytes).unwrap().to_rgb8();
            assert_eq!(decode(&bytes).unwrap(), before);
        }
    }
}
