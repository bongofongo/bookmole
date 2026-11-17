//! Cover textures: decode, prescale, accent luma clamp, procedural plates.
//! The border-median accent is measured by the engine; never recompute it here.

use std::path::Path;

use image::imageops::FilterType;
use image::{ImageReader, RgbImage};

use crate::math::{Vec3, vec3};

/// An RGB texture in 0..1 float space, sampled bilinearly.
#[derive(Debug, Clone)]
pub struct Texture {
    pub width: u32,
    pub height: u32,
    px: Vec<Vec3>,
}

impl Texture {
    pub fn from_rgb_image(img: &RgbImage) -> Texture {
        let px = img
            .pixels()
            .map(|p| vec3(p[0] as f32, p[1] as f32, p[2] as f32) / 255.0)
            .collect();
        Texture {
            width: img.width(),
            height: img.height(),
            px,
        }
    }

    fn texel(&self, x: i64, y: i64) -> Vec3 {
        let x = x.clamp(0, self.width as i64 - 1) as usize;
        let y = y.clamp(0, self.height as i64 - 1) as usize;
        self.px[y * self.width as usize + x]
    }

    /// Bilinear sample. `u` runs left→right, `v` runs top→bottom.
    pub fn sample(&self, u: f32, v: f32) -> Vec3 {
        if self.px.is_empty() {
            return Vec3::splat(0.5);
        }
        let fx = u.clamp(0.0, 1.0) * (self.width as f32 - 1.0);
        let fy = v.clamp(0.0, 1.0) * (self.height as f32 - 1.0);
        let (x0, y0) = (fx.floor() as i64, fy.floor() as i64);
        let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
        let top = self.texel(x0, y0) * (1.0 - tx) + self.texel(x0 + 1, y0) * tx;
        let bot = self.texel(x0, y0 + 1) * (1.0 - tx) + self.texel(x0 + 1, y0 + 1) * tx;
        top * (1.0 - ty) + bot * ty
    }
}

#[derive(Debug, Clone)]
pub struct Cover {
    pub texture: Texture,
    pub accent: Vec3,
    /// width / height of the source image.
    pub aspect: f32,
}

/// Downscales (never upscales) to about `target_width`; `accent` comes from the engine, not the pixels.
pub fn load_cover(path: &Path, target_width: u32, accent: Vec3) -> Option<Cover> {
    let img = ImageReader::open(path).ok()?.decode().ok()?.to_rgb8();
    if img.width() == 0 || img.height() == 0 {
        return None;
    }
    let aspect = img.width() as f32 / img.height() as f32;
    let target_width = target_width.max(8);
    let scaled = if img.width() > target_width {
        let h = (img.height() as f32 * target_width as f32 / img.width() as f32).round();
        image::imageops::resize(&img, target_width, (h as u32).max(1), FilterType::Lanczos3)
    } else {
        img
    };
    Some(Cover {
        texture: Texture::from_rgb_image(&scaled),
        accent,
        aspect,
    })
}

/// Luma band for spine and back board, applied at render time; the stored accent stays unclamped.
const ACCENT_LUMA: (f32, f32) = (0.14, 0.62);

/// Clamps the stored accent into `ACCENT_LUMA`; `None` falls back to the title's procedural hue.
pub fn accent_for(measured: Option<[u8; 3]>, title: &str) -> Vec3 {
    let Some(rgb) = measured else {
        return procedural_accent(title);
    };
    let c = vec3(rgb[0] as f32, rgb[1] as f32, rgb[2] as f32) / 255.0;
    clamp_luma(c, ACCENT_LUMA.0, ACCENT_LUMA.1)
}

/// Shared by [`procedural_cover`] and [`accent_for`] so a plate and its spine always match.
fn procedural_accent(title: &str) -> Vec3 {
    let hue = (fnv1a(title) % 360) as f32;
    clamp_luma(hsv_to_rgb(hue, 0.42, 0.55), ACCENT_LUMA.0, ACCENT_LUMA.1)
}

/// Rescale a color so its luma lands inside [lo, hi], preserving hue.
fn clamp_luma(c: Vec3, lo: f32, hi: f32) -> Vec3 {
    let luma = c.x * 0.2126 + c.y * 0.7152 + c.z * 0.0722;
    if luma < 1e-4 {
        return Vec3::splat(lo);
    }
    let target = luma.clamp(lo, hi);
    let scaled = c * (target / luma);
    vec3(
        scaled.x.clamp(0.0, 1.0),
        scaled.y.clamp(0.0, 1.0),
        scaled.z.clamp(0.0, 1.0),
    )
}

/// Deterministic stand-in for a missing cover; the title picks the hue.
pub fn procedural_cover(title: &str) -> Cover {
    let hue = (fnv1a(title) % 360) as f32;
    let base = hsv_to_rgb(hue, 0.42, 0.55);
    let inset = hsv_to_rgb(hue, 0.30, 0.74);

    const W: u32 = 48;
    const H: u32 = 72;
    let mut px = Vec::with_capacity((W * H) as usize);
    for y in 0..H {
        for x in 0..W {
            let in_panel = (6..W - 6).contains(&x) && (10..H - 16).contains(&y);
            let on_rule = (y == H - 12 || y == H - 10) && (8..W - 8).contains(&x);
            px.push(if on_rule {
                inset
            } else if in_panel {
                inset * 0.9
            } else {
                base
            });
        }
    }
    Cover {
        texture: Texture {
            width: W,
            height: H,
            px,
        },
        accent: procedural_accent(title),
        aspect: W as f32 / H as f32,
    }
}

fn fnv1a(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> Vec3 {
    let c = v * s;
    let hp = (h % 360.0) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    vec3(r + m, g + m, b + m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    fn luma(c: Vec3) -> f32 {
        c.x * 0.2126 + c.y * 0.7152 + c.z * 0.0722
    }

    fn framed_file(border: Rgb<u8>, middle: Rgb<u8>) -> tempfile::TempPath {
        let path = tempfile::Builder::new()
            .suffix(".png")
            .tempfile()
            .expect("scratch file")
            .into_temp_path();
        let img = RgbImage::from_fn(20, 20, |x, y| {
            if x < 2 || y < 2 || x >= 18 || y >= 18 {
                border
            } else {
                middle
            }
        });
        img.save(&path).expect("write the cover");
        path
    }

    /// The file's border is deliberately the inverse of the accent: the stored accent, not the pixels, must reach the spine.
    #[test]
    fn accent_follows_the_border_not_the_middle() {
        let red = accent_for(Some([140, 20, 20]), "");
        assert!(
            red.x > red.y * 2.0 && red.x > red.z * 2.0,
            "not red: {red:?}"
        );

        let path = framed_file(Rgb([140, 20, 20]), Rgb([255, 255, 255]));
        let blue = accent_for(Some([20, 20, 140]), "");
        let cover = load_cover(&path, 16, blue).expect("a png is decodable");
        assert_eq!(
            cover.accent, blue,
            "the renderer re-measured the file instead of reading the column"
        );
    }

    #[test]
    fn accent_luma_is_clamped_into_the_visible_band() {
        let white = luma(accent_for(Some([255, 255, 255]), ""));
        assert!(white <= ACCENT_LUMA.1 + 0.01, "white cover: luma {white}");

        let black = luma(accent_for(Some([0, 0, 0]), ""));
        assert!(black >= ACCENT_LUMA.0 - 0.01, "black cover: luma {black}");

        let mid = accent_for(Some([160, 32, 32]), "");
        assert!((mid.x - 160.0 / 255.0).abs() < 1e-6, "{mid:?}");
    }

    #[test]
    fn an_unmeasured_cover_falls_back_to_the_procedural_hue() {
        assert_eq!(
            accent_for(None, "Piranesi"),
            procedural_cover("Piranesi").accent,
            "a NULL accent must match the plate the same title would paint"
        );
        assert_ne!(accent_for(None, "Piranesi"), accent_for(None, "Trust"));

        let path = framed_file(Rgb([140, 20, 20]), Rgb([255, 255, 255]));
        let cover = load_cover(&path, 16, accent_for(None, "Piranesi")).expect("decodable");
        assert_eq!(cover.accent, procedural_cover("Piranesi").accent);
    }

    #[test]
    fn procedural_cover_is_deterministic_and_title_dependent() {
        let a = procedural_cover("Station Eleven");
        let b = procedural_cover("Station Eleven");
        let c = procedural_cover("Piranesi");
        assert_eq!(a.accent, b.accent);
        assert_ne!(a.accent, c.accent);
        assert_eq!(a.texture.width, 48);
    }

    #[test]
    fn bilinear_sample_interpolates_between_texels() {
        let img = RgbImage::from_fn(2, 1, |x, _| {
            if x == 0 {
                Rgb([0, 0, 0])
            } else {
                Rgb([255, 255, 255])
            }
        });
        let tex = Texture::from_rgb_image(&img);
        let mid = tex.sample(0.5, 0.0);
        assert!((mid.x - 0.5).abs() < 0.01, "{mid:?}");
        assert!(tex.sample(0.0, 0.0).x < 0.01);
        assert!(tex.sample(1.0, 0.0).x > 0.99);
    }
}
