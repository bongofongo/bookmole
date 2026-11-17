//! The pixel raster: the same cuboid, traced into real RGBA instead of block
//! glyphs.
//!
//! This lives alongside the TUI's glyph raster (`render3d::render`) rather than
//! replacing it, on purpose.
//! The glyph path's coverage rule (`hits * 2 >= samples` — a majority of rays
//! claims the whole subpixel) is *correct for glyphs*: the terminal background
//! is unknown, so partial coverage can't be blended, and the glyph chooser
//! recovers sub-cell detail from the binary mask. Applied to a pixel image that
//! same rule gives a hard, stair-stepped silhouette. Kitty composites RGBA over
//! the cell background, so here we can and should carry real alpha.
//!
//! Keeping the two rasters separate also means `render` is untouched, so
//! `--dump-frame` stays byte-identical *by construction* rather than by
//! promise. What the two share is all the physics: [`scene::camera_origin`],
//! [`scene::primary_ray`], [`scene::shade`] — which is why the physics moved
//! into this crate with it, and the glyph raster calls back across.
//!
//! ## Antialiasing without paying for it
//!
//! Supersampling every pixel is unaffordable at native cell resolution — a
//! 50x26 rect at 20x38 px/cell is a megapixel, and 9 rays each is 9M rays a
//! frame at 20fps. But the only pixels that *need* more rays are the ones on
//! the silhouette, and a silhouette's perimeter grows as O(sqrt(N)) against the
//! area's O(N). So: one ray per pixel, then re-trace just the boundary at full
//! supersampling. On a megapixel frame that is a few thousand extra pixels —
//! under 1% — for edges that are properly smooth.

use crate::math::Vec3;
use crate::texture::Cover;
use crate::{Model, RenderParams, scene};

/// Straight (non-premultiplied) 8-bit RGBA, the layout kitty's `f=32` wants.
/// A GPU image type that wants the same straight alpha in BGRA order needs
/// only channels 0 and 2 swapped, and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaBuf {
    pub width: u32,
    pub height: u32,
    pub px: Vec<u8>,
}

impl RgbaBuf {
    pub fn new(width: u32, height: u32) -> RgbaBuf {
        RgbaBuf {
            width,
            height,
            px: vec![0; width as usize * height as usize * 4],
        }
    }

    /// The RGBA quad at `(x, y)`. Only tests read individual pixels — every
    /// consumer takes the whole buffer at once. Public, not `#[cfg(test)]`,
    /// because the TUI's raster tests read it from across the crate boundary.
    pub fn get(&self, x: u32, y: u32) -> [u8; 4] {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [self.px[i], self.px[i + 1], self.px[i + 2], self.px[i + 3]]
    }
}

/// How much detail this frame gets.
///
/// A spinning book retraces every tick and has to fit inside the 50ms budget
/// *including* compressing and pushing the bytes down a pty; a parked one can
/// afford everything. Crisp when still, capped when moving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    Motion,
    Settle,
}

impl Quality {
    /// Rays per axis. Motion leans on the edge pass alone; a settled frame can
    /// afford a denser edge sample.
    pub fn edge_ss(self) -> u8 {
        match self {
            Quality::Motion => 3,
            Quality::Settle => 4,
        }
    }
}

/// The pixel grid one rich frame fills.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub width: u32,
    pub height: u32,
    /// Cell rows the image spans. Feeds [`scene::fill_for`], so it is the cell
    /// count and *not* anything derived from the pixel dimensions — that is
    /// what keeps the book's absolute size identical to the glyph path's.
    ///
    /// A window frontend has no cells. For it `rows` is only the dial on
    /// `fill_for`: anything up to 18 gives the largest fill the rig allows
    /// (0.70 of the frame's shorter axis, fitted over a whole turn), and the
    /// frame's pixel size then decides the book's size on screen.
    pub rows: u16,
    pub quality: Quality,
}

/// Trace the book into straight RGBA. Uncovered pixels are fully transparent,
/// which is how the pane keeps the user's terminal background — the pixel-path
/// equivalent of the glyph path emitting `Color::Reset`.
pub fn render_rgba(target: Target, model: &Model, cover: &Cover, params: RenderParams) -> RgbaBuf {
    let (w, h) = (target.width, target.height);
    let mut out = RgbaBuf::new(w, h);
    if w == 0 || h == 0 || target.rows == 0 {
        return out;
    }

    let rot = params.pose.rotation();
    let aspect = w as f32 / h as f32;
    let origin = scene::camera_origin(aspect, params.pose.pitch, model.half, target.rows);
    let (fw, fh) = (w as f32, h as f32);
    let n = (w as usize) * (h as usize);
    let ss = params.ss.max(target.quality.edge_ss()) as u32;
    let samples = (ss * ss) as f32;

    // Rows per worker. The two passes are split into horizontal bands over
    // scoped threads — `shade` is a pure function of `&Cover`, so the bands
    // share everything immutably and need no synchronisation beyond the two
    // scope joins. Threading is what brings a motion frame from ~15ms down
    // inside the 20fps budget, with the compress + write still to pay for.
    let bands = std::thread::available_parallelism()
        .map(|p| p.get().min(8))
        .unwrap_or(1)
        .min(h as usize)
        .max(1);
    let band_rows = (h as usize).div_ceil(bands);

    // Pass 1: one ray through each pixel centre.
    let mut hits: Vec<Option<Vec3>> = vec![None; n];
    std::thread::scope(|s| {
        for (b, chunk) in hits.chunks_mut(band_rows * w as usize).enumerate() {
            let y0 = (b * band_rows) as u32;
            s.spawn(move || {
                for (row, line) in chunk.chunks_mut(w as usize).enumerate() {
                    let v = ((y0 + row as u32) as f32 + 0.5) / fh;
                    for (x, px) in line.iter_mut().enumerate() {
                        let u = (x as f32 + 0.5) / fw;
                        let dir = scene::primary_ray(u, v, aspect);
                        *px = scene::shade(origin, dir, rot, model.half, cover);
                    }
                }
            });
        }
    });

    // Pass 2: a pixel sits on the silhouette if any 4-neighbour disagrees with
    // it about being on the book. Off-buffer counts as a miss, so a book that
    // runs to the edge of the frame still gets its border antialiased. Bands
    // read across their own boundaries, which is why pass 1 had to finish
    // first — `hits` is shared read-only from here on.
    let hits = &hits;
    std::thread::scope(|s| {
        for (b, chunk) in out.px.chunks_mut(band_rows * w as usize * 4).enumerate() {
            let y0 = (b * band_rows) as u32;
            s.spawn(move || {
                for (row, line) in chunk.chunks_mut(w as usize * 4).enumerate() {
                    let y = y0 + row as u32;
                    for x in 0..w {
                        let i = y as usize * w as usize + x as usize;
                        let inside = hits[i].is_some();
                        let neighbour = |dx: i64, dy: i64| -> bool {
                            let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                            if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                                return false;
                            }
                            hits[ny as usize * w as usize + nx as usize].is_some()
                        };
                        let edge = [(-1, 0), (1, 0), (0, -1), (0, 1)]
                            .iter()
                            .any(|&(dx, dy)| neighbour(dx, dy) != inside);

                        let (rgb, alpha) = if !edge {
                            match hits[i] {
                                Some(rgb) => (rgb, 1.0),
                                None => (Vec3::ZERO, 0.0),
                            }
                        } else {
                            let mut sum = Vec3::ZERO;
                            let mut covered = 0.0f32;
                            for sy in 0..ss {
                                for sx in 0..ss {
                                    let u = (x as f32 + (sx as f32 + 0.5) / ss as f32) / fw;
                                    let v = (y as f32 + (sy as f32 + 0.5) / ss as f32) / fh;
                                    let dir = scene::primary_ray(u, v, aspect);
                                    if let Some(c) =
                                        scene::shade(origin, dir, rot, model.half, cover)
                                    {
                                        sum = sum + c;
                                        covered += 1.0;
                                    }
                                }
                            }
                            if covered > 0.0 {
                                // Unpremultiplied: colour is the mean of the
                                // rays that hit, alpha carries coverage.
                                (sum / covered, covered / samples)
                            } else {
                                (Vec3::ZERO, 0.0)
                            }
                        };
                        write_px(line, x as usize, rgb, alpha);
                    }
                }
            });
        }
    });

    out
}

/// Quantise one straight-alpha pixel into `line` at pixel index `x`.
fn write_px(line: &mut [u8], x: usize, rgb: Vec3, alpha: f32) {
    let q = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
    line[x * 4] = q(rgb.x);
    line[x * 4 + 1] = q(rgb.y);
    line[x * 4 + 2] = q(rgb.z);
    line[x * 4 + 3] = q(alpha);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::texture;

    /// The reference paperback, through the same constructor a frontend uses.
    fn fixture() -> (Model, Cover) {
        let cover = texture::procedural_cover("Station Eleven");
        (Model::from_proportions(cover.aspect, 0.12), cover)
    }

    fn target(width: u32, height: u32, rows: u16) -> Target {
        Target {
            width,
            height,
            rows,
            quality: Quality::Settle,
        }
    }

    // The TUI's `render3d::raster` tests drive this function through its cell
    // budgets and are the proof the move changed nothing. These assert the
    // same properties with no terminal anywhere in the picture, which is the
    // shape a window frontend calls it in.

    #[test]
    fn a_frame_has_a_book_in_the_middle_and_clear_corners() {
        let (model, cover) = fixture();
        let buf = render_rgba(
            target(160, 200, 18),
            &model,
            &cover,
            RenderParams::default(),
        );
        assert_eq!(buf.px.len(), 160 * 200 * 4);
        assert_eq!(buf.get(80, 100)[3], 255, "the centre is the book");
        assert_eq!(buf.get(0, 0), [0, 0, 0, 0], "a corner is clear");
    }

    #[test]
    fn the_edge_is_antialiased_and_clear_pixels_carry_no_colour() {
        let (model, cover) = fixture();
        let buf = render_rgba(
            target(120, 150, 18),
            &model,
            &cover,
            RenderParams::default(),
        );
        let mut partial = 0;
        for px in buf.px.as_chunks::<4>().0 {
            match px[3] {
                0 => assert_eq!(&px[..3], &[0, 0, 0], "colour leaked into a clear pixel"),
                255 => {}
                _ => partial += 1,
            }
        }
        assert!(partial > 0, "no antialiased edge pixels at all");
    }

    #[test]
    fn a_degenerate_target_is_an_empty_buffer_not_a_panic() {
        let (model, cover) = fixture();
        for (w, h, rows) in [(0, 10, 18), (10, 0, 18), (10, 10, 0), (1, 1, 1)] {
            let buf = render_rgba(target(w, h, rows), &model, &cover, RenderParams::default());
            assert_eq!(buf.px.len(), w as usize * h as usize * 4);
        }
    }

    /// What one still frame costs a window frontend, end to end: decode and
    /// prescale the cover, trace at `Quality::Settle`, swap to BGRA (the
    /// channel order many GPU image types take) — the work a GUI does off its
    /// UI thread. The widths are typical window columns: 240, 360 and 680 pt,
    /// each at 2x, in a 4:5 frame. Median of five.
    ///
    /// Only meaningful on a **release** build. Point it at a real cover:
    ///
    /// `BOOKMOLE_BOOK3D_COVER=dev-data/database/images/dev-0003.png \
    ///  cargo test --release -p bookmole-book3d -- --ignored --nocapture trace_cost`
    #[test]
    #[ignore = "timing, not correctness; run on a release build"]
    fn trace_cost() {
        let path = std::env::var_os("BOOKMOLE_BOOK3D_COVER").map(std::path::PathBuf::from);
        if path.is_none() {
            eprintln!("SKIPPED: BOOKMOLE_BOOK3D_COVER unset; timing the procedural plate");
        }
        let accent = texture::accent_for(Some([120, 60, 40]), "Station Eleven");
        let model = Model::from_proportions(2.0 / 3.0, 0.12);
        println!(
            "{:>7} {:>11} {:>8} {:>10} {:>10} {:>10}",
            "pt@2x", "pixels", "MP", "median", "min", "max"
        );
        for (pt, w, h) in [(240u32, 480u32, 600u32), (360, 720, 900), (680, 1360, 1700)] {
            let mut runs: Vec<std::time::Duration> = (0..5)
                .map(|_| {
                    let t = std::time::Instant::now();
                    let cover = path
                        .as_deref()
                        .and_then(|p| texture::load_cover(p, w.clamp(24, 2048), accent))
                        .unwrap_or_else(|| texture::procedural_cover("Station Eleven"));
                    let mut buf =
                        render_rgba(target(w, h, 18), &model, &cover, RenderParams::default());
                    for px in buf.px.as_chunks_mut::<4>().0 {
                        px.swap(0, 2);
                    }
                    std::hint::black_box(&buf);
                    t.elapsed()
                })
                .collect();
            runs.sort();
            println!(
                "{pt:>7} {:>11} {:>8.2} {:>10.2?} {:>10.2?} {:>10.2?}",
                format!("{w}x{h}"),
                (w as f64 * h as f64) / 1.0e6,
                runs[2],
                runs[0],
                runs[4]
            );
        }
    }
}
