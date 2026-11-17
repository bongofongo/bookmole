//! Software renderer for the book: ray-traced cuboid to subpixels to terminal cells.
//! Only the blit step knows about ratatui.

pub mod blit;
pub mod caps;
pub mod kitty;
pub mod present;
pub mod raster;

pub use bookmole_book3d::{math, scene, texture};

use std::path::{Path, PathBuf};

use bookmole::{Book, EditionShape};

pub use blit::{GlyphSet, RgbBuf};
pub use caps::Caps;
pub use kitty::ImageWire;
pub use present::presenter_for;
pub use scene::Pose;

/// `Rich` draws glyphs while the book animates and pixels once it parks; image bytes cost a
/// terminal far more than text, so an animated pixel book cannot be made cheap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RenderMode {
    #[default]
    Glyph,
    Rich,
    /// Pixels while moving: the known-bad configuration, kept for calibrating the latency probe.
    RichAlways,
}

impl RenderMode {
    pub fn is_rich(self) -> bool {
        matches!(self, RenderMode::Rich | RenderMode::RichAlways)
    }

    pub fn label(self) -> &'static str {
        match self {
            RenderMode::Glyph => "glyph",
            RenderMode::Rich => "rich",
            RenderMode::RichAlways => "rich-always",
        }
    }
}

use math::Vec3;
use texture::Cover;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderParams {
    pub pose: Pose,
    pub ss: u8,
    pub glyphs: GlyphSet,
    /// `Some` is set by the event loop, since a parked book stops redrawing and inference from
    /// pose changes would never see it park. `None` infers it (dump-frame, bench, tests).
    pub moving: Option<bool>,
}

impl Default for RenderParams {
    fn default() -> Self {
        RenderParams {
            pose: Pose::default(),
            ss: 3,
            glyphs: GlyphSet::Octant,
            moving: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Model {
    pub half: Vec3,
}

impl Model {
    /// Aspect comes from stored cover dimensions, falling back to the decoded image where they are
    /// `None` (older covers); a shelf must not decode covers, this single-book view already has one.
    pub fn new(book: &Book, cover: &Cover) -> Model {
        let aspect = book
            .cover_aspect()
            .map(|a| a as f32)
            .unwrap_or(cover.aspect);
        let shape = EditionShape::of_book(book, Some(aspect));
        let scaled = bookmole_book3d::Model::from_proportions(
            shape.width_over_height,
            shape.thickness_over_height,
        );
        Model { half: scaled.half }
    }
}

type CoverKey = (Option<i64>, Option<String>, String, Option<[u8; 3]>, u32);

/// The single authority on whether a book has a real cover: stored paths are relative to the
/// data root, so they must not be joined onto the images dir.
pub fn resolve_cover(images_dir: &Path, stored: &str) -> Option<PathBuf> {
    let direct = Path::new(stored);
    if direct.exists() {
        return Some(direct.to_path_buf());
    }
    let by_name = images_dir.join(direct.file_name()?);
    by_name.exists().then_some(by_name)
}

fn glyph_texels(cols: u16) -> u32 {
    (cols as u32 * 4).max(24)
}

pub struct Scene {
    images_dir: PathBuf,
    cover: Option<(CoverKey, Cover)>,
    frame: Option<(FrameKey, RgbBuf)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FrameKey {
    cover: CoverKey,
    yaw_q: i32,
    pitch_q: i32,
    cols: u16,
    rows: u16,
    ss: u8,
    glyphs: GlyphSet,
}

impl Scene {
    pub fn new(images_dir: impl Into<PathBuf>) -> Scene {
        Scene {
            images_dir: images_dir.into(),
            cover: None,
            frame: None,
        }
    }

    fn resolve_cover(&self, stored: &str) -> Option<PathBuf> {
        resolve_cover(&self.images_dir, stored)
    }

    fn cover_key(book: &Book, texels: u32) -> CoverKey {
        (
            book.id,
            book.cover_path.clone(),
            book.display_title().to_string(),
            // The accent is a stored column that can be back-filled without the path or title changing.
            book.cover_accent_rgb(),
            texels,
        )
    }

    pub fn cover(&mut self, book: &Book, texels: u32) -> &Cover {
        let key = Self::cover_key(book, texels);
        let stale = self.cover.as_ref().map(|(k, _)| k != &key).unwrap_or(true);
        if stale {
            // Read from the book, never measured off the file; `Scene::cover` is the one site with a `&Book`.
            let accent = texture::accent_for(book.cover_accent_rgb(), book.display_title());
            let loaded = book
                .cover_path
                .as_deref()
                .and_then(|p| self.resolve_cover(p))
                .and_then(|p| texture::load_cover(&p, texels, accent))
                .unwrap_or_else(|| texture::procedural_cover(book.display_title()));
            self.cover = Some((key, loaded));
        }
        &self.cover.as_ref().expect("just populated").1
    }

    pub fn frame(&mut self, book: &Book, cols: u16, rows: u16, params: RenderParams) -> &RgbBuf {
        let key = FrameKey {
            cover: Self::cover_key(book, glyph_texels(cols)),
            yaw_q: (params.pose.yaw * 512.0).round() as i32,
            pitch_q: (params.pose.pitch * 512.0).round() as i32,
            cols,
            rows,
            ss: params.ss,
            glyphs: params.glyphs,
        };
        let hit = self.frame.as_ref().map(|(k, _)| k == &key).unwrap_or(false);
        if !hit {
            let fb = {
                let cover = self.cover(book, glyph_texels(cols));
                render(cols, rows, &Model::new(book, cover), cover, params)
            };
            self.frame = Some((key, fb));
        }
        &self.frame.as_ref().expect("just populated").1
    }
}

pub fn render(cols: u16, rows: u16, model: &Model, cover: &Cover, params: RenderParams) -> RgbBuf {
    let (width, height) = (cols * 2, rows * params.glyphs.cell_h());
    let mut fb = RgbBuf::new(width, height);
    if cols == 0 || rows == 0 {
        return fb;
    }
    let rot = params.pose.rotation();
    let aspect = cols as f32 / (rows as f32 * 2.0);
    let origin = scene::camera_origin(aspect, params.pose.pitch, model.half, rows);
    let ss = params.ss.max(1) as u16;
    let samples = (ss * ss) as f32;
    let (fw, fh) = (width as f32, height as f32);

    for y in 0..height {
        for x in 0..width {
            let mut sum = Vec3::ZERO;
            let mut hits = 0.0f32;
            for sy in 0..ss {
                for sx in 0..ss {
                    let u = (x as f32 + (sx as f32 + 0.5) / ss as f32) / fw;
                    let v = (y as f32 + (sy as f32 + 0.5) / ss as f32) / fh;
                    let dir = scene::primary_ray(u, v, aspect);
                    if let Some(c) = scene::shade(origin, dir, rot, model.half, cover) {
                        sum = sum + c;
                        hits += 1.0;
                    }
                }
            }
            if hits * 2.0 >= samples {
                fb.set(x, y, Some(sum / hits));
            }
        }
    }
    fb
}

#[cfg(test)]
mod tests {
    use super::*;
    use math::vec3;

    fn filled_subpixels(fb: &RgbBuf) -> usize {
        (0..fb.height)
            .flat_map(|y| (0..fb.width).map(move |x| (x, y)))
            .filter(|(x, y)| fb.get(*x, *y).is_some())
            .count()
    }

    fn test_book() -> Book {
        Book {
            id: Some(1),
            title: Some("Station Eleven".into()),
            ..Book::default()
        }
    }

    /// Also the only check of the scene-unit to height-ratio factor (half-height 0.75); a wrong
    /// factor would change every spine's thickness by a third with nothing failing.
    #[test]
    fn the_engines_shape_reproduces_the_renderers_old_arithmetic() {
        fn historical(book: &Book, cover: &Cover) -> Vec3 {
            let h = scene::HALF_HEIGHT;
            let width = h * cover.aspect.clamp(0.55, 0.85);
            let pages = book.page_count.unwrap_or(320).clamp(48, 1400) as f32;
            let depth = (0.045 + pages / 9000.0).clamp(0.05, 0.20);
            vec3(width, h, depth)
        }
        let cover = texture::procedural_cover("Station Eleven");
        for pages in [None, Some(1), Some(48), Some(320), Some(1408), Some(9999)] {
            let book = Book {
                page_count: pages,
                ..test_book()
            };
            let old = historical(&book, &cover);
            let new = Model::new(&book, &cover).half;
            for (a, b, axis) in [
                (old.x, new.x, "width"),
                (old.y, new.y, "height"),
                (old.z, new.z, "depth"),
            ] {
                assert!(
                    (a - b).abs() < 1e-6,
                    "{axis} moved for {pages:?}: {a} -> {b}"
                );
            }
        }
        let paperback = Model::new(
            &Book {
                page_count: Some(320),
                ..test_book()
            },
            &cover,
        );
        assert!((paperback.half.x - scene::DEFAULT_HALF_EXTENTS.x).abs() < 1e-6);
        assert!((paperback.half.y - scene::DEFAULT_HALF_EXTENTS.y).abs() < 1e-6);
    }

    #[test]
    fn a_measured_cover_is_not_decoded_to_find_its_aspect() {
        let cover = texture::procedural_cover("Station Eleven");
        let measured = Book {
            cover_width: Some(170),
            cover_height: Some(200),
            ..test_book()
        };
        let stored = Model::new(&measured, &cover).half.x;
        let decoded = Model::new(&test_book(), &cover).half.x;
        assert!(
            (stored - scene::HALF_HEIGHT * 0.85).abs() < 1e-6,
            "the stored 170x200 should decide the width, got {stored}"
        );
        assert!(
            (stored - decoded).abs() > 1e-3,
            "the two inputs must disagree or this test proves nothing"
        );
    }

    #[test]
    fn an_unmeasured_cover_still_falls_back_to_the_decoded_image() {
        let cover = texture::procedural_cover("Station Eleven");
        let half_measured = Book {
            cover_width: Some(170),
            cover_height: None,
            ..test_book()
        };
        assert_eq!(
            Model::new(&half_measured, &cover).half.x,
            Model::new(&test_book(), &cover).half.x,
        );
    }

    #[test]
    fn a_zero_page_count_no_longer_draws_the_thinnest_possible_book() {
        let cover = texture::procedural_cover("Station Eleven");
        let with = |pages| {
            Model::new(
                &Book {
                    page_count: pages,
                    ..test_book()
                },
                &cover,
            )
            .half
            .z
        };
        assert_eq!(with(Some(0)), with(None), "zero is absence, not a length");
        assert!(with(Some(0)) > with(Some(48)));
    }

    #[test]
    fn renders_a_silhouette_with_empty_margins() {
        let book = test_book();
        let cover = texture::procedural_cover("Station Eleven");
        let model = Model::new(&book, &cover);
        let fb = render(60, 40, &model, &cover, RenderParams::default());
        assert_eq!(
            (fb.width, fb.height),
            (120, 160),
            "eight subpixels per cell"
        );
        assert!(fb.get(60, 80).is_some(), "centre should be the book");
        assert!(fb.get(0, 0).is_none(), "corner should be empty");
        let filled = filled_subpixels(&fb);
        let total = fb.width as usize * fb.height as usize;
        assert!(
            filled > total / 25 && filled < total / 2,
            "silhouette covered {filled}/{total}"
        );
    }

    #[test]
    fn missing_cover_falls_back_without_touching_the_disk() {
        let mut scene = Scene::new("database/images");
        let mut book = test_book();
        book.cover_path = Some("/nonexistent/cover.jpg".into());
        let fb = scene.frame(&book, 40, 30, RenderParams::default());
        assert!(
            filled_subpixels(fb) > 200,
            "fallback cover rendered nothing"
        );
    }

    #[test]
    fn frames_are_cached_until_the_pose_moves() {
        let mut scene = Scene::new("database/images");
        let book = test_book();
        let params = RenderParams::default();
        scene.frame(&book, 40, 30, params);
        let first = scene.frame.as_ref().unwrap().0.yaw_q;
        scene.frame(&book, 40, 30, params);
        assert_eq!(scene.frame.as_ref().unwrap().0.yaw_q, first);

        let moved = RenderParams {
            pose: Pose {
                yaw: params.pose.yaw + 0.5,
                ..params.pose
            },
            ..params
        };
        scene.frame(&book, 40, 30, moved);
        assert_ne!(scene.frame.as_ref().unwrap().0.yaw_q, first);
    }

    #[test]
    fn switching_glyph_family_invalidates_the_cached_frame() {
        let mut scene = Scene::new("database/images");
        let book = test_book();
        let octant = RenderParams {
            glyphs: GlyphSet::Octant,
            ..RenderParams::default()
        };
        let tall = scene.frame(&book, 40, 30, octant).height;
        let quadrant = RenderParams {
            glyphs: GlyphSet::Quadrant,
            ..octant
        };
        let short = scene.frame(&book, 40, 30, quadrant).height;
        assert_eq!(tall, 30 * 4);
        assert_eq!(short, 30 * 2, "cache served a frame of the wrong height");
    }

    #[test]
    #[ignore = "timing, not correctness; run on a release build"]
    fn glyph_cost() {
        let cover = texture::procedural_cover("Station Eleven");
        let model = Model::new(&test_book(), &cover);
        println!(
            "{:<22} {:>9} {:>12} {:>10}",
            "case", "cells", "framebuffer", "trace"
        );
        for (label, cols, rows, set, ss) in [
            ("book rect, octant", 50u16, 26u16, GlyphSet::Octant, 3u8),
            ("book rect, quadrant", 50, 26, GlyphSet::Quadrant, 3),
            ("book rect, octant ss4", 50, 26, GlyphSet::Octant, 4),
            ("full pane, octant", 120, 40, GlyphSet::Octant, 3),
        ] {
            let params = RenderParams {
                glyphs: set,
                ss,
                ..RenderParams::default()
            };
            let mut best = std::time::Duration::MAX;
            for _ in 0..5 {
                let start = std::time::Instant::now();
                let fb = render(cols, rows, &model, &cover, params);
                let each = start.elapsed();
                std::hint::black_box(&fb);
                best = best.min(each);
            }
            println!(
                "{label:<22} {:>9} {:>12} {:>10?}",
                format!("{cols}x{rows}"),
                format!("{}x{}", cols as u32 * 2, rows as u32 * set.cell_h() as u32),
                best
            );
        }
        println!("(budget is 50ms/frame at the 20fps tick)");
    }
}
