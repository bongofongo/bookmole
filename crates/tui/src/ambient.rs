//! A slow, quiet animation behind non-book screens. Never runs on the book screen; stays sparse
//! (the blit cost depends on it); redraws at `AMBIENT_FPS`, not the tick rate.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::render3d::blit::{GlyphSet, RgbBuf, blit_sparse};
use crate::render3d::math::{Vec3, vec3};
use crate::theme;

/// Ambient redraw rate; faster frames would be indistinguishable after quantization.
const AMBIENT_FPS: f32 = 4.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Motif {
    #[default]
    Off,
    Motes,
    Contours,
}

impl Motif {
    pub const ALL: [Motif; 3] = [Motif::Off, Motif::Motes, Motif::Contours];

    pub fn label(self) -> &'static str {
        match self {
            Motif::Off => "off",
            Motif::Motes => "motes",
            Motif::Contours => "contours",
        }
    }

    /// Unknown values are not errors; callers fall back to `Motif::Off`.
    pub fn from_label(s: &str) -> Option<Motif> {
        Motif::ALL.into_iter().find(|m| m.label() == s)
    }

    pub fn next(self) -> Motif {
        let i = Motif::ALL.iter().position(|m| *m == self).unwrap_or(0);
        Motif::ALL[(i + 1) % Motif::ALL.len()]
    }
}

#[derive(Debug, Clone, Default)]
pub struct Ambient {
    pub motif: Motif,
    /// Wrapped so a long session keeps f32 precision.
    clock: f32,
    bucket: u32,
}

/// Long enough that the seam goes unnoticed, short enough to keep f32 subpixel precision.
const CLOCK_WRAP: f32 = 3600.0;

impl Ambient {
    pub fn new(motif: Motif) -> Ambient {
        Ambient {
            motif,
            clock: 0.0,
            bucket: 0,
        }
    }

    /// True only when the quantized frame changed; that keeps 20fps ticks from becoming redraws.
    pub fn advance(&mut self, dt: f32) -> bool {
        if self.motif == Motif::Off {
            return false;
        }
        self.clock = (self.clock + dt) % CLOCK_WRAP;
        let bucket = (self.clock * AMBIENT_FPS) as u32;
        if bucket == self.bucket {
            return false;
        }
        self.bucket = bucket;
        true
    }

    #[cfg(test)]
    pub fn frame_index(&self) -> u32 {
        self.bucket
    }

    pub fn draw(&self, buf: &mut Buffer, area: Rect, glyphs: GlyphSet) {
        if self.motif == Motif::Off || area.width == 0 || area.height == 0 {
            return;
        }
        // Snap to the quantized frame so the drawn frame matches what `advance` reported.
        let t = self.bucket as f32 / AMBIENT_FPS;
        let fb = match self.motif {
            Motif::Off => return,
            Motif::Motes => motes(area, glyphs, t),
            Motif::Contours => contours(area, glyphs, t),
        };
        blit_sparse(&fb, area, buf, glyphs);
    }
}

const MID: Vec3 = vec3(0.5, 0.5, 0.5);

/// The accent pulled toward mid-tone: the terminal background is unknown, so brightness cannot
/// fade to invisible; motifs vanish by dropping coverage instead.
fn ink(level: f32) -> Vec3 {
    let rgb = theme::accent_rgb();
    let accent = vec3(
        ((rgb >> 16) & 0xFF) as f32 / 255.0,
        ((rgb >> 8) & 0xFF) as f32 / 255.0,
        (rgb & 0xFF) as f32 / 255.0,
    );
    let tinted = MID + (accent - MID) * 0.35;
    let depth = 0.85 + 0.30 * level.clamp(0.0, 1.0);
    MID + (tinted - MID) * depth
}

/// Deterministic, so a resize or screen change does not reshuffle the field.
fn hash(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^= x >> 16;
    x
}

fn hash01(x: u32) -> f32 {
    hash(x) as f32 / u32::MAX as f32
}

fn hash2(x: i32, y: i32) -> f32 {
    hash01((x as u32).wrapping_mul(0x9E37_79B9) ^ (y as u32).wrapping_mul(0x85EB_CA6B))
}

fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

fn noise(x: f32, y: f32) -> f32 {
    let (ix, iy) = (x.floor(), y.floor());
    let (fx, fy) = (smoothstep(x - ix), smoothstep(y - iy));
    let (ix, iy) = (ix as i32, iy as i32);
    let a = hash2(ix, iy);
    let b = hash2(ix + 1, iy);
    let c = hash2(ix, iy + 1);
    let d = hash2(ix + 1, iy + 1);
    let top = a + (b - a) * fx;
    let bottom = c + (d - c) * fx;
    top + (bottom - top) * fy
}

fn fbm(x: f32, y: f32) -> f32 {
    noise(x, y) * 0.65 + noise(x * 2.1 + 11.0, y * 2.1 + 7.0) * 0.35
}

fn buffer_for(area: Rect, glyphs: GlyphSet) -> RgbBuf {
    RgbBuf::new(area.width * 2, area.height * glyphs.cell_h())
}

fn mote_count(area: Rect) -> u32 {
    ((area.width as u32 * area.height as u32) / 70).clamp(12, 160)
}

fn motes(area: Rect, glyphs: GlyphSet, t: f32) -> RgbBuf {
    let mut fb = buffer_for(area, glyphs);
    let (w, h) = (fb.width as f32, fb.height as f32);
    if w <= 0.0 || h <= 0.0 {
        return fb;
    }

    for i in 0..mote_count(area) {
        let seed = i.wrapping_mul(3);
        let vx = 0.15 + hash01(seed) * 0.35;
        let vy = 0.10 + hash01(seed + 1) * 0.30;
        let x = (hash01(seed + 2) * w + t * vx) % w;
        let y = (hash01(seed + 3) * h + h - (t * vy) % h) % h;

        // Wink by coverage, not brightness; see `ink`.
        let period = 9.0 + hash01(seed + 4) * 14.0;
        let phase = ((t / period) + hash01(seed + 5)) % 1.0;
        let wave = (phase * std::f32::consts::TAU).sin();
        if wave <= 0.0 {
            continue;
        }
        fb.set(x as u16, y as u16, Some(ink(wave)));
    }
    fb
}

const LEVELS: f32 = 5.0;

/// Low on purpose: long lazy curves; raising it fills the screen with scribble.
const FREQ: f32 = 0.014;

const DRIFT: f32 = 0.010;

/// Half-width of a contour line in subpixels; divided by the gradient so lines keep one weight.
const LINE_HALF_WIDTH: f32 = 0.7;

/// Cheap pre-filter: subpixels farther than this from any level skip the gradient evaluations.
const NEAR_LEVEL: f32 = 0.25;

/// One flat ink, deliberately: ratatui only skips byte-identical cells, and a colour ramp makes
/// every lit cell's averaged colour drift each frame, repainting the terminal. Re-measure with
/// the ignored `ambient_wire_rate` sweep before changing.
const FLAT_INK: f32 = 0.75;

fn contours(area: Rect, glyphs: GlyphSet, t: f32) -> RgbBuf {
    let mut fb = buffer_for(area, glyphs);
    // A subpixel's height:width ratio is `4 / cell_h`; scale the vertical frequency by it or the
    // curves come out stretched.
    let fy = FREQ * (4.0 / glyphs.cell_h() as f32);

    let field = |x: f32, y: f32| fbm(x * FREQ, y * fy + t * DRIFT);

    for y in 0..fb.height {
        for x in 0..fb.width {
            let (px, py) = (x as f32, y as f32);
            let f = field(px, py);
            let v = f * LEVELS;
            let off = v - v.round();
            if off.abs() > NEAR_LEVEL {
                continue;
            }

            let gx = field(px + 1.0, py) - f;
            let gy = field(px, py + 1.0) - f;
            let grad = ((gx * gx + gy * gy).sqrt() * LEVELS).max(1e-6);
            if (off / grad).abs() < LINE_HALF_WIDTH {
                fb.set(x, y, Some(ink(FLAT_INK)));
            }
        }
    }
    fb
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(motif: Motif, area: Rect, glyphs: GlyphSet, t: f32) -> (usize, usize) {
        let fb = match motif {
            Motif::Off => return (0, 0),
            Motif::Motes => motes(area, glyphs, t),
            Motif::Contours => contours(area, glyphs, t),
        };
        let total = fb.width as usize * fb.height as usize;
        let on = (0..fb.height)
            .flat_map(|y| (0..fb.width).map(move |x| (x, y)))
            .filter(|(x, y)| fb.get(*x, *y).is_some())
            .count();
        (on, total)
    }

    fn coverage(motif: Motif, area: Rect, glyphs: GlyphSet, t: f32) -> f32 {
        let (on, total) = lit(motif, area, glyphs, t);
        on as f32 / total as f32
    }

    #[test]
    fn the_ambient_clock_only_asks_for_a_redraw_at_its_own_rate() {
        let mut a = Ambient::new(Motif::Motes);
        let redraws = (0..20).filter(|_| a.advance(0.05)).count();
        assert_eq!(redraws, AMBIENT_FPS as usize);
    }

    #[test]
    fn the_clock_wraps_without_losing_a_beat() {
        let mut a = Ambient::new(Motif::Motes);
        a.clock = CLOCK_WRAP - 0.02;
        a.bucket = (a.clock * AMBIENT_FPS) as u32;
        assert!(a.advance(0.05));
        assert!(a.clock < 1.0, "clock should have wrapped, got {}", a.clock);
    }

    #[test]
    fn off_leaves_the_buffer_untouched() {
        let area = Rect::new(0, 0, 40, 12);
        let mut buf = Buffer::empty(area);
        for x in 0..area.width {
            buf[(x, 0)].set_symbol("X");
        }
        let before = buf.clone();

        let mut a = Ambient::new(Motif::Off);
        assert!(!a.advance(1.0));
        a.draw(&mut buf, area, GlyphSet::Octant);
        assert_eq!(buf, before);
    }

    #[test]
    fn both_motifs_stay_sparse() {
        let area = Rect::new(0, 0, 200, 50);
        for motif in [Motif::Motes, Motif::Contours] {
            for glyphs in [GlyphSet::Quadrant, GlyphSet::Octant] {
                for frame in 0..200 {
                    let t = frame as f32 / AMBIENT_FPS;
                    let c = coverage(motif, area, glyphs, t);
                    assert!(
                        c < 0.15,
                        "{motif:?}/{glyphs:?} lit {:.1}% of subpixels at t={t}",
                        c * 100.0
                    );
                }
            }
        }
    }

    #[test]
    fn both_motifs_actually_draw_something() {
        let area = Rect::new(0, 0, 200, 50);
        for motif in [Motif::Motes, Motif::Contours] {
            for frame in 0..40 {
                let t = frame as f32 * 0.7;
                let (on, _) = lit(motif, area, GlyphSet::Octant, t);
                assert!(on >= 8, "{motif:?} lit only {on} subpixels at t={t}");
            }
        }
    }

    #[test]
    fn motifs_survive_degenerate_areas() {
        for (w, h) in [(1, 1), (2, 1), (1, 40), (0, 0)] {
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(Rect::new(0, 0, w.max(1), h.max(1)));
            for motif in [Motif::Motes, Motif::Contours] {
                Ambient::new(motif).draw(&mut buf, area, GlyphSet::Octant);
            }
        }
    }

    #[test]
    fn the_motif_cycle_returns_to_where_it_started() {
        let mut m = Motif::Off;
        for _ in 0..Motif::ALL.len() {
            m = m.next();
        }
        assert_eq!(m, Motif::Off);
        assert_eq!(Motif::from_label("contours"), Some(Motif::Contours));
        assert_eq!(Motif::from_label("nonsense"), None);
        for motif in Motif::ALL {
            assert_eq!(Motif::from_label(motif.label()), Some(motif));
        }
    }
}
