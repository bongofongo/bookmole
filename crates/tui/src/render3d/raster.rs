//! The pixel raster's terminal half.
//!
//! The trace itself — [`render_rgba`], [`RgbaBuf`], [`Target`], [`Quality`] —
//! lives in `bookmole_book3d::raster` so a
//! window frontend can draw the same book without linking a terminal. What
//! stays here is what only a terminal cares about: the wire budgets a frame is
//! sized to, and fitting a rect of cells into them. The module doc that used to
//! open this file — why there are two rasters, and how the silhouette is
//! antialiased for under 1% — moved with the trace and is still true.
//!
//! [`render_rgba`] here is the TUI's call into it: same signature as before
//! the move, so `present.rs` and `--bench-rich` did not change, and it hands
//! the trace exactly the two fields it ever read (`pose`, `ss`).

pub use bookmole_book3d::raster::{Quality, RgbaBuf, Target};

use super::kitty::ImageWire;
use super::texture::Cover;
use super::{Model, RenderParams};

/// Pixel ceiling while the pose is changing.
///
/// Bytes on the wire are the binding constraint, and they are *not* the
/// constraint our own timings show: a motion frame traces and encodes in a few
/// ms and the pty never blocks. The cost lands inside the terminal, which has
/// to decompress and re-upload the whole texture every frame — and tmux has to
/// re-parse it on the way. Measured with a real (photographic) cover on a 50x26
/// rect at 20fps:
///
/// | budget | render | KB/frame | MB/s |
/// |---|---|---|---|
/// | 420K | 651x644 | 347K | 7.1 |
/// | 200K | 449x444 | 170K | 3.5 |
/// | 120K | 348x344 | 105K | 2.2 |
///
/// 7.1 MB/s of continuous texture replacement is ~140x what the glyph path
/// sends and made the whole terminal unusable. Note a procedural cover
/// compresses several times better than a real one — do not re-derive this
/// number from a synthetic fixture.
pub const MOTION_MAX_PX: u32 = 120_000;
/// Pixel ceiling for a settled pose, where the cost is paid once.
pub const SETTLE_MAX_PX: u32 = 1_200_000;

/// The same two ceilings for an uncompressed payload ([`ImageWire::Raw`]).
///
/// Both are the zlib budget divided by six, which is not a round number chosen
/// for tidiness: a smooth product render measures well under one base64 byte
/// per pixel through zlib, against 5.33 raw. The budget here has always been
/// *bytes* wearing a pixel count as its unit, so dropping the compression
/// without dropping the pixel count would fire a 6.4 MB settle frame at a
/// decoder that has just been established as fragile about large images.
pub const RAW_MOTION_MAX_PX: u32 = MOTION_MAX_PX / 6;
pub const RAW_SETTLE_MAX_PX: u32 = SETTLE_MAX_PX / 6;

/// The pixel ceiling for `quality` on `wire`.
///
/// Was `Quality::max_px` before the move; a free function now, because
/// `Quality` lives in `bookmole-book3d` and the wire format does not.
pub fn max_px(quality: Quality, wire: ImageWire) -> u32 {
    match (quality, wire) {
        (Quality::Motion, ImageWire::Zlib) => MOTION_MAX_PX,
        (Quality::Settle, ImageWire::Zlib) => SETTLE_MAX_PX,
        (Quality::Motion, ImageWire::Raw) => RAW_MOTION_MAX_PX,
        (Quality::Settle, ImageWire::Raw) => RAW_SETTLE_MAX_PX,
    }
}

/// Native cell resolution, scaled down uniformly until it fits the tier's
/// budget. Uniform scaling means `width / height` stays exactly the cell rect's
/// physical aspect, which is also the box kitty stretches the image into — so
/// unlike the glyph path there is no `cols : rows*2` approximation here.
pub fn target_for(
    cols: u16,
    rows: u16,
    cell_px: (u16, u16),
    quality: Quality,
    wire: ImageWire,
) -> Target {
    let native_w = (cols as u32 * cell_px.0 as u32).max(1);
    let native_h = (rows as u32 * cell_px.1 as u32).max(1);
    let total = native_w as u64 * native_h as u64;
    let cap = max_px(quality, wire) as u64;
    let (width, height) = if total <= cap {
        (native_w, native_h)
    } else {
        // Floor, not round: rounding both axes up can carry the product just
        // past the cap (814x516 = 420_024 against a 420_000 budget), and the
        // budget is the thing that keeps the spin inside its frame time.
        let scale = (cap as f64 / total as f64).sqrt();
        (
            ((native_w as f64 * scale).floor() as u32).max(1),
            ((native_h as f64 * scale).floor() as u32).max(1),
        )
    };
    Target {
        width,
        height,
        rows,
        quality,
    }
}

/// Trace the book into straight RGBA. Uncovered pixels are fully transparent,
/// which is how the pane keeps the user's terminal background — the pixel-path
/// equivalent of the glyph path emitting `Color::Reset`.
pub fn render_rgba(target: Target, model: &Model, cover: &Cover, params: RenderParams) -> RgbaBuf {
    bookmole_book3d::render_rgba(
        target,
        &bookmole_book3d::Model { half: model.half },
        cover,
        bookmole_book3d::RenderParams {
            pose: params.pose,
            ss: params.ss,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render3d::texture;
    use bookmole::Book;

    fn fixture() -> (Model, Cover) {
        let book = Book {
            id: Some(1),
            title: Some("Station Eleven".into()),
            ..Book::default()
        };
        let cover = texture::procedural_cover(book.display_title());
        (Model::new(&book, &cover), cover)
    }

    #[test]
    fn native_resolution_is_exact_under_the_cap() {
        let t = target_for(40, 20, (20, 38), Quality::Settle, ImageWire::Zlib);
        assert_eq!((t.width, t.height), (800, 760));
        assert_eq!(t.rows, 20, "rows stays the cell count, not a pixel count");
    }

    #[test]
    fn oversized_rects_scale_down_but_keep_their_aspect() {
        // 120x40 cells at 20x38px is 2400x1520 = 3.6MP, well over both caps.
        let native = 2400.0f64 / 1520.0;
        for q in [Quality::Motion, Quality::Settle] {
            let t = target_for(120, 40, (20, 38), q, ImageWire::Zlib);
            let total = t.width as u64 * t.height as u64;
            assert!(
                total <= max_px(q, ImageWire::Zlib) as u64,
                "{q:?} exceeded its budget"
            );
            // Comfortably within the cap, not scaled to nothing.
            assert!(
                total > max_px(q, ImageWire::Zlib) as u64 / 2,
                "{q:?} scaled too far down"
            );
            let got = t.width as f64 / t.height as f64;
            assert!(
                (got - native).abs() / native < 0.01,
                "{q:?} distorted the aspect: {got} vs {native}"
            );
        }
    }

    #[test]
    fn motion_is_cheaper_than_settle() {
        let m = target_for(120, 40, (20, 38), Quality::Motion, ImageWire::Zlib);
        let s = target_for(120, 40, (20, 38), Quality::Settle, ImageWire::Zlib);
        assert!(m.width * m.height < s.width * s.height);
    }

    #[test]
    fn degenerate_rects_never_produce_a_zero_dimension() {
        for (c, r) in [(1u16, 1u16), (1, 40), (40, 1)] {
            let t = target_for(c, r, (20, 38), Quality::Motion, ImageWire::Zlib);
            assert!(t.width >= 1 && t.height >= 1, "{c}x{r} collapsed");
            let (model, cover) = fixture();
            // Must not panic at any degenerate size — a render panic in the
            // book view wrecks the user's pane.
            let _ = render_rgba(t, &model, &cover, RenderParams::default());
        }
    }

    #[test]
    fn zero_rows_render_to_an_empty_buffer() {
        let (model, cover) = fixture();
        let t = Target {
            width: 8,
            height: 8,
            rows: 0,
            quality: Quality::Motion,
        };
        let buf = render_rgba(t, &model, &cover, RenderParams::default());
        assert!(buf.px.iter().all(|&b| b == 0));
    }

    #[test]
    fn interior_pixels_are_opaque_or_fully_clear() {
        let (model, cover) = fixture();
        let t = target_for(30, 16, (8, 16), Quality::Settle, ImageWire::Zlib);
        let buf = render_rgba(t, &model, &cover, RenderParams::default());
        let alphas: Vec<u8> = (0..buf.height)
            .flat_map(|y| (0..buf.width).map(move |x| (x, y)))
            .map(|(x, y)| buf.get(x, y)[3])
            .collect();
        assert!(alphas.contains(&255), "nothing was drawn");
        assert!(alphas.contains(&0), "no transparent margin");
    }

    #[test]
    fn the_silhouette_is_antialiased() {
        // The whole point of the edge pass: partial coverage must exist, or the
        // image is just the glyph path's hard mask at higher resolution.
        let (model, cover) = fixture();
        let t = target_for(30, 16, (8, 16), Quality::Settle, ImageWire::Zlib);
        let buf = render_rgba(t, &model, &cover, RenderParams::default());
        let partial = (0..buf.height)
            .flat_map(|y| (0..buf.width).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let a = buf.get(x, y)[3];
                a > 0 && a < 255
            })
            .count();
        assert!(partial > 0, "no antialiased edge pixels at all");
    }

    #[test]
    fn transparent_pixels_carry_no_colour() {
        // Straight (unpremultiplied) alpha still wants clear pixels to be
        // black: a stray colour in a fully transparent pixel shows up as a halo
        // wherever a terminal composites naively.
        let (model, cover) = fixture();
        let t = target_for(20, 10, (8, 16), Quality::Motion, ImageWire::Zlib);
        let buf = render_rgba(t, &model, &cover, RenderParams::default());
        for y in 0..buf.height {
            for x in 0..buf.width {
                let [r, g, b, a] = buf.get(x, y);
                if a == 0 {
                    assert_eq!((r, g, b), (0, 0, 0), "colour leaked at {x},{y}");
                }
            }
        }
    }

    /// Not an assertion — a measuring stick for the pixel caps above, which are
    /// only meaningful against a **release** build (debug is ~30x slower).
    ///
    /// `cargo test --release -p bookmole-tui -- --ignored --nocapture raster_cost`
    #[test]
    #[ignore = "timing, not correctness; run on a release build"]
    fn raster_cost() {
        let (model, cover) = fixture();
        let params = RenderParams::default();
        for (label, cols, rows, cell, q) in [
            (
                "book rect, motion",
                50u16,
                26u16,
                (20u16, 38u16),
                Quality::Motion,
            ),
            ("book rect, settle", 50, 26, (20, 38), Quality::Settle),
            ("full pane, motion", 120, 40, (20, 38), Quality::Motion),
            ("full pane, settle", 120, 40, (20, 38), Quality::Settle),
        ] {
            let t = target_for(cols, rows, cell, q, ImageWire::Zlib);
            let start = std::time::Instant::now();
            let reps = 5;
            for _ in 0..reps {
                let _ = render_rgba(t, &model, &cover, params);
            }
            let each = start.elapsed() / reps;
            println!(
                "{label:<20} {}x{} ({:.2} MP) -> {:?}",
                t.width,
                t.height,
                (t.width as f64 * t.height as f64) / 1.0e6,
                each
            );
        }
    }

    /// Where a rich frame's time actually goes. The trace is only one term;
    /// zlib, base64 and the bytes pushed at the pty are the others, and on the
    /// wire is where a spinning book gets into trouble.
    ///
    /// `cargo test --release -p bookmole-tui -- --ignored --nocapture frame_budget`
    #[test]
    #[ignore = "timing, not correctness; run on a release build"]
    fn frame_budget() {
        use crate::render3d::kitty;
        let (model, cover) = fixture();
        let params = RenderParams::default();
        println!(
            "{:<22} {:>11} {:>9} {:>9} {:>9} {:>10} {:>9}",
            "case", "pixels", "trace", "encode", "total", "bytes", "at 20fps"
        );
        for (label, cols, rows, q) in [
            ("book rect, motion", 50u16, 26u16, Quality::Motion),
            ("book rect, settle", 50, 26, Quality::Settle),
            ("full pane, motion", 120, 40, Quality::Motion),
        ] {
            let t = target_for(cols, rows, (20, 38), q, ImageWire::Zlib);
            let t0 = std::time::Instant::now();
            let img = render_rgba(t, &model, &cover, params);
            let trace = t0.elapsed();
            let t1 = std::time::Instant::now();
            let esc = kitty::transmit(&img, 1, cols, rows, true, ImageWire::Zlib);
            let encode = t1.elapsed();
            let rate = esc.len() as f64 * 20.0 / 1.0e6;
            println!(
                "{label:<22} {:>11} {:>9.2?} {:>9.2?} {:>9.2?} {:>9}K {:>7.1}MB/s",
                format!("{}x{}", t.width, t.height),
                trace,
                encode,
                trace + encode,
                esc.len() / 1024,
                rate
            );
        }
    }

    /// Bytes-on-the-wire against pixel budget, using a **real** cover if one is
    /// present. This matters: a photographic cover compresses several times
    /// worse than the procedural plate, and the wire rate is what a terminal
    /// actually chokes on.
    ///
    /// Set `BOOKMOLE_PERF_LOG` to also append each row to the frame log, in
    /// the same JSONL shape `--bench-render` writes — these *are* frames, just
    /// synthetic ones, so keeping one format means one set of tools reads both.
    ///
    /// `cargo test --release -p bookmole-tui -- --ignored --nocapture wire_rate`
    #[test]
    #[ignore = "timing, not correctness; run on a release build"]
    fn wire_rate() {
        use crate::perf;
        use crate::render3d::{kitty, texture};
        // No book is in hand here — the file is found by walking the images
        // dir — so the accent is the unmeasured one this stand-in book would
        // get. It tints the spine and nothing else; what this sweep measures is
        // how a photographic *cover* compresses.
        let book = Book::default();
        let accent = texture::accent_for(book.cover_accent_rgb(), book.display_title());
        // Tests run from the crate dir, so try the workspace root too.
        let real = ["database/images", "../../database/images"]
            .iter()
            .filter_map(|d| std::fs::read_dir(d).ok())
            .flat_map(|d| d.filter_map(|e| e.ok()).map(|e| e.path()))
            .find(|p| p.extension().is_some_and(|x| x == "jpg"))
            .and_then(|p| texture::load_cover(&p, 1024, accent));
        let (model, cover) = match real {
            Some(c) => (Model::new(&book, &c), c),
            None => {
                println!("no real cover found; skipping");
                return;
            }
        };
        let params = RenderParams::default();
        let meter = perf::Meter::new();
        // Note the cwd: cargo runs a test binary from the *package* root, so a
        // relative path here lands under `crates/tui/`, not the workspace root.
        // The Makefile passes an absolute path for that reason.
        let mut log = std::env::var_os("BOOKMOLE_PERF_LOG")
            .map(std::path::PathBuf::from)
            .and_then(|p| match perf::Recorder::open(&p, meter.clone()) {
                Ok(r) => Some(r),
                Err(e) => {
                    println!("perf log {}: {e} (continuing without it)", p.display());
                    None
                }
            });
        println!(
            "{:>10} {:>12} {:>9} {:>10} {:>12}",
            "budget", "pixels", "trace", "KB/frame", "MB/s @20fps"
        );
        for budget in [60_000u32, 120_000, 200_000, 420_000, 1_200_000] {
            let scale = (budget as f64 / (50.0 * 20.0 * 26.0 * 38.0))
                .sqrt()
                .min(1.0);
            let t = Target {
                width: ((50.0 * 20.0 * scale) as u32).max(1),
                height: ((26.0 * 38.0 * scale) as u32).max(1),
                rows: 26,
                quality: Quality::Motion,
            };
            let t0 = std::time::Instant::now();
            let img = render_rgba(t, &model, &cover, params);
            let trace = t0.elapsed();
            let esc = kitty::transmit(&img, 1, 50, 26, true, ImageWire::Zlib);
            if let Some(rec) = log.as_mut() {
                rec.frame_begin();
                perf::record(perf::Stage::Trace, trace);
                perf::note(perf::FrameNote {
                    quality: perf::Quality::Motion,
                    cols: 50,
                    rows: 26,
                    px_w: t.width,
                    px_h: t.height,
                    transmitted: true,
                    fell_back: false,
                });
                // Count the escape without writing it anywhere: the sweep is
                // about how many bytes a budget *would* put on the wire.
                use std::io::Write;
                let _ = perf::CountingWriter::new(
                    std::io::sink(),
                    meter.clone(),
                    perf::ByteClass::Image,
                )
                .write_all(esc.as_bytes());
                rec.frame_end(perf::FrameCtx {
                    mode: "sweep",
                    moving: true,
                    term_cols: 50,
                    term_rows: 26,
                    draw: trace,
                    rtt_kitty: None,
                    rtt_tmux: None,
                });
            }
            println!(
                "{:>9}K {:>12} {:>9.2?} {:>9}K {:>11.1}",
                budget / 1000,
                format!("{}x{}", t.width, t.height),
                trace,
                esc.len() / 1024,
                esc.len() as f64 * 20.0 / 1.0e6
            );
        }
    }

    #[test]
    fn the_book_keeps_its_absolute_size_across_rasters() {
        // Both paths must route through `fill_for(rows)`, so the same rect
        // yields the same silhouette coverage whichever raster drew it. This is
        // the regression guard for "the book got huge in the new renderer".
        let (model, cover) = fixture();
        let params = RenderParams::default();
        let (cols, rows) = (40u16, 20u16);

        let glyph = super::super::render(cols, rows, &model, &cover, params);
        let glyph_frac = {
            let total = glyph.width as f32 * glyph.height as f32;
            let hit = (0..glyph.height)
                .flat_map(|y| (0..glyph.width).map(move |x| (x, y)))
                .filter(|(x, y)| glyph.get(*x, *y).is_some())
                .count() as f32;
            hit / total
        };

        let t = target_for(cols, rows, (8, 16), Quality::Settle, ImageWire::Zlib);
        let rich = render_rgba(t, &model, &cover, params);
        let rich_frac = {
            let total = (rich.width * rich.height) as f32;
            let hit = (0..rich.height)
                .flat_map(|y| (0..rich.width).map(move |x| (x, y)))
                .filter(|&(x, y)| rich.get(x, y)[3] >= 128)
                .count() as f32;
            hit / total
        };

        assert!(
            (glyph_frac - rich_frac).abs() < 0.03,
            "coverage diverged: glyph {glyph_frac:.3} vs rich {rich_frac:.3}"
        );
    }
}
