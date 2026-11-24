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
    if !path.exists() {
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

fn decode(bytes: &[u8]) -> Option<RgbImage> {
    let img = image::load_from_memory(bytes).ok()?.to_rgb8();
    (img.width() > 0 && img.height() > 0).then_some(img)
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
}
