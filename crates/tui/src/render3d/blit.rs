//! Block-glyph presentation: quadrant (2x2) or octant (2x4, needs a Unicode 16 font) subpixels per
//! cell.
//! Each cell is a two-color quantization of its subpixels, with coverage forcing the split at
//! edges.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use super::math::Vec3;

/// Indexed by coverage mask: bit 0 top-left, 1 top-right, 2 bottom-left, 3 bottom-right.
const QUADRANTS: [&str; 16] = [
    " ", "▘", "▝", "▀", "▖", "▌", "▞", "▛", "▗", "▚", "▐", "▜", "▄", "▙", "▟", "█",
];

/// Indexed by 2x4 coverage mask in reading order (bit 0 top-left, bit 7 bottom-right).
/// Generated from the Unicode 16 database; do not hand-edit, `octant_masks_round_trip` guards it.
#[rustfmt::skip]
const OCTANTS: [&str; 256] = [
    "\u{20}", "\u{1CEA7}", "\u{1CEAB}", "\u{1FB82}", "\u{1CD00}", "\u{2598}", "\u{1CD01}", "\u{1CD02}",
    "\u{1CD03}", "\u{1CD04}", "\u{259D}", "\u{1CD05}", "\u{1CD06}", "\u{1CD07}", "\u{1CD08}", "\u{2580}",
    "\u{1CD09}", "\u{1CD0A}", "\u{1CD0B}", "\u{1CD0C}", "\u{1FBE6}", "\u{1CD0D}", "\u{1CD0E}", "\u{1CD0F}",
    "\u{1CD10}", "\u{1CD11}", "\u{1CD12}", "\u{1CD13}", "\u{1CD14}", "\u{1CD15}", "\u{1CD16}", "\u{1CD17}",
    "\u{1CD18}", "\u{1CD19}", "\u{1CD1A}", "\u{1CD1B}", "\u{1CD1C}", "\u{1CD1D}", "\u{1CD1E}", "\u{1CD1F}",
    "\u{1FBE7}", "\u{1CD20}", "\u{1CD21}", "\u{1CD22}", "\u{1CD23}", "\u{1CD24}", "\u{1CD25}", "\u{1CD26}",
    "\u{1CD27}", "\u{1CD28}", "\u{1CD29}", "\u{1CD2A}", "\u{1CD2B}", "\u{1CD2C}", "\u{1CD2D}", "\u{1CD2E}",
    "\u{1CD2F}", "\u{1CD30}", "\u{1CD31}", "\u{1CD32}", "\u{1CD33}", "\u{1CD34}", "\u{1CD35}", "\u{1FB85}",
    "\u{1CEA3}", "\u{1CD36}", "\u{1CD37}", "\u{1CD38}", "\u{1CD39}", "\u{1CD3A}", "\u{1CD3B}", "\u{1CD3C}",
    "\u{1CD3D}", "\u{1CD3E}", "\u{1CD3F}", "\u{1CD40}", "\u{1CD41}", "\u{1CD42}", "\u{1CD43}", "\u{1CD44}",
    "\u{2596}", "\u{1CD45}", "\u{1CD46}", "\u{1CD47}", "\u{1CD48}", "\u{258C}", "\u{1CD49}", "\u{1CD4A}",
    "\u{1CD4B}", "\u{1CD4C}", "\u{259E}", "\u{1CD4D}", "\u{1CD4E}", "\u{1CD4F}", "\u{1CD50}", "\u{259B}",
    "\u{1CD51}", "\u{1CD52}", "\u{1CD53}", "\u{1CD54}", "\u{1CD55}", "\u{1CD56}", "\u{1CD57}", "\u{1CD58}",
    "\u{1CD59}", "\u{1CD5A}", "\u{1CD5B}", "\u{1CD5C}", "\u{1CD5D}", "\u{1CD5E}", "\u{1CD5F}", "\u{1CD60}",
    "\u{1CD61}", "\u{1CD62}", "\u{1CD63}", "\u{1CD64}", "\u{1CD65}", "\u{1CD66}", "\u{1CD67}", "\u{1CD68}",
    "\u{1CD69}", "\u{1CD6A}", "\u{1CD6B}", "\u{1CD6C}", "\u{1CD6D}", "\u{1CD6E}", "\u{1CD6F}", "\u{1CD70}",
    "\u{1CEA0}", "\u{1CD71}", "\u{1CD72}", "\u{1CD73}", "\u{1CD74}", "\u{1CD75}", "\u{1CD76}", "\u{1CD77}",
    "\u{1CD78}", "\u{1CD79}", "\u{1CD7A}", "\u{1CD7B}", "\u{1CD7C}", "\u{1CD7D}", "\u{1CD7E}", "\u{1CD7F}",
    "\u{1CD80}", "\u{1CD81}", "\u{1CD82}", "\u{1CD83}", "\u{1CD84}", "\u{1CD85}", "\u{1CD86}", "\u{1CD87}",
    "\u{1CD88}", "\u{1CD89}", "\u{1CD8A}", "\u{1CD8B}", "\u{1CD8C}", "\u{1CD8D}", "\u{1CD8E}", "\u{1CD8F}",
    "\u{2597}", "\u{1CD90}", "\u{1CD91}", "\u{1CD92}", "\u{1CD93}", "\u{259A}", "\u{1CD94}", "\u{1CD95}",
    "\u{1CD96}", "\u{1CD97}", "\u{2590}", "\u{1CD98}", "\u{1CD99}", "\u{1CD9A}", "\u{1CD9B}", "\u{259C}",
    "\u{1CD9C}", "\u{1CD9D}", "\u{1CD9E}", "\u{1CD9F}", "\u{1CDA0}", "\u{1CDA1}", "\u{1CDA2}", "\u{1CDA3}",
    "\u{1CDA4}", "\u{1CDA5}", "\u{1CDA6}", "\u{1CDA7}", "\u{1CDA8}", "\u{1CDA9}", "\u{1CDAA}", "\u{1CDAB}",
    "\u{2582}", "\u{1CDAC}", "\u{1CDAD}", "\u{1CDAE}", "\u{1CDAF}", "\u{1CDB0}", "\u{1CDB1}", "\u{1CDB2}",
    "\u{1CDB3}", "\u{1CDB4}", "\u{1CDB5}", "\u{1CDB6}", "\u{1CDB7}", "\u{1CDB8}", "\u{1CDB9}", "\u{1CDBA}",
    "\u{1CDBB}", "\u{1CDBC}", "\u{1CDBD}", "\u{1CDBE}", "\u{1CDBF}", "\u{1CDC0}", "\u{1CDC1}", "\u{1CDC2}",
    "\u{1CDC3}", "\u{1CDC4}", "\u{1CDC5}", "\u{1CDC6}", "\u{1CDC7}", "\u{1CDC8}", "\u{1CDC9}", "\u{1CDCA}",
    "\u{1CDCB}", "\u{1CDCC}", "\u{1CDCD}", "\u{1CDCE}", "\u{1CDCF}", "\u{1CDD0}", "\u{1CDD1}", "\u{1CDD2}",
    "\u{1CDD3}", "\u{1CDD4}", "\u{1CDD5}", "\u{1CDD6}", "\u{1CDD7}", "\u{1CDD8}", "\u{1CDD9}", "\u{1CDDA}",
    "\u{2584}", "\u{1CDDB}", "\u{1CDDC}", "\u{1CDDD}", "\u{1CDDE}", "\u{2599}", "\u{1CDDF}", "\u{1CDE0}",
    "\u{1CDE1}", "\u{1CDE2}", "\u{259F}", "\u{1CDE3}", "\u{2586}", "\u{1CDE4}", "\u{1CDE5}", "\u{2588}",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlyphSet {
    Quadrant,
    Octant,
}

const MAX_SUBPIXELS: usize = 8;

impl GlyphSet {
    pub fn cell_h(self) -> u16 {
        match self {
            GlyphSet::Quadrant => 2,
            GlyphSet::Octant => 4,
        }
    }

    pub fn subpixels(self) -> usize {
        2 * self.cell_h() as usize
    }

    fn glyph(self, mask: usize) -> &'static str {
        match self {
            GlyphSet::Quadrant => QUADRANTS[mask],
            GlyphSet::Octant => OCTANTS[mask],
        }
    }
}

/// `None` is "no book here" and lets the terminal background show through.
#[derive(Debug, Clone)]
pub struct RgbBuf {
    pub width: u16,
    pub height: u16,
    px: Vec<Option<Vec3>>,
}

impl RgbBuf {
    pub fn new(width: u16, height: u16) -> RgbBuf {
        RgbBuf {
            width,
            height,
            px: vec![None; width as usize * height as usize],
        }
    }

    pub fn set(&mut self, x: u16, y: u16, color: Option<Vec3>) {
        if x < self.width && y < self.height {
            self.px[y as usize * self.width as usize + x as usize] = color;
        }
    }

    pub fn get(&self, x: u16, y: u16) -> Option<Vec3> {
        if x >= self.width || y >= self.height {
            return None;
        }
        self.px[y as usize * self.width as usize + x as usize]
    }

    fn cell(&self, col: u16, row: u16, set: GlyphSet) -> ([Option<Vec3>; MAX_SUBPIXELS], usize) {
        let rows = set.cell_h();
        let (x0, y0) = (col * 2, row * rows);
        let mut out = [None; MAX_SUBPIXELS];
        let mut n = 0;
        for dy in 0..rows {
            for dx in 0..2 {
                out[n] = self.get(x0 + dx, y0 + dy);
                n += 1;
            }
        }
        (out, n)
    }
}

struct CellPlan {
    symbol: &'static str,
    fg: Option<Vec3>,
    bg: Option<Vec3>,
}

fn mean(samples: &[Option<Vec3>], mask: usize) -> Option<Vec3> {
    let mut sum = Vec3::ZERO;
    let mut n = 0.0;
    for (i, s) in samples.iter().enumerate() {
        if mask & (1 << i) != 0
            && let Some(c) = s
        {
            sum = sum + *c;
            n += 1.0;
        }
    }
    (n > 0.0).then(|| sum / n)
}

fn error(samples: &[Option<Vec3>], mask: usize, center: Option<Vec3>) -> f32 {
    let Some(center) = center else { return 0.0 };
    let mut err = 0.0;
    for (i, s) in samples.iter().enumerate() {
        if mask & (1 << i) != 0
            && let Some(c) = s
        {
            let d = *c - center;
            err += d.dot(d);
        }
    }
    err
}

fn plan(samples: &[Option<Vec3>], set: GlyphSet) -> CellPlan {
    let full = (1usize << samples.len()) - 1;
    let covered: usize = samples
        .iter()
        .enumerate()
        .filter(|(_, s)| s.is_some())
        .map(|(i, _)| 1usize << i)
        .sum();

    if covered == 0 {
        return CellPlan {
            symbol: " ",
            fg: None,
            bg: None,
        };
    }
    if covered != full {
        return CellPlan {
            symbol: set.glyph(covered),
            fg: mean(samples, covered),
            bg: None,
        };
    }

    // Seeded with the solid block so a flat cell, and any tie, stays solid.
    let mut best = (error(samples, full, mean(samples, full)), full);
    for mask in 1..full {
        let inverse = !mask & full;
        let err = error(samples, mask, mean(samples, mask))
            + error(samples, inverse, mean(samples, inverse));
        if err < best.0 {
            best = (err, mask);
        }
    }
    let mask = best.1;
    let inverse = !mask & full;
    CellPlan {
        symbol: set.glyph(mask),
        fg: mean(samples, mask),
        bg: mean(samples, inverse),
    }
}

fn to_color(c: Option<Vec3>) -> Color {
    match c {
        Some(c) => {
            let ch = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
            Color::Rgb(ch(c.x), ch(c.y), ch(c.z))
        }
        None => Color::Reset,
    }
}

/// `fb` must be `area.width * 2` wide and `area.height * set.cell_h()` tall; the rest is ignored.
pub fn blit(fb: &RgbBuf, area: Rect, buf: &mut Buffer, set: GlyphSet) {
    for row in 0..area.height {
        for col in 0..area.width {
            let (cell, n) = fb.cell(col, row, set);
            let p = plan(&cell[..n], set);
            buf[(area.x + col, area.y + row)]
                .set_symbol(p.symbol)
                .set_fg(to_color(p.fg))
                .set_bg(to_color(p.bg));
        }
    }
}

/// A layer over a sparse framebuffer: empty cells are left untouched and the split is dictated by
/// coverage alone, skipping the costly error search `blit` runs. The book keeps using `blit`.
pub fn blit_sparse(fb: &RgbBuf, area: Rect, buf: &mut Buffer, set: GlyphSet) {
    for row in 0..area.height {
        for col in 0..area.width {
            let (cell, n) = fb.cell(col, row, set);
            let samples = &cell[..n];
            let covered: usize = samples
                .iter()
                .enumerate()
                .filter(|(_, s)| s.is_some())
                .map(|(i, _)| 1usize << i)
                .sum();
            if covered == 0 {
                continue;
            }
            // Background is deliberately not set, so the terminal's own shows through.
            buf[(area.x + col, area.y + row)]
                .set_symbol(set.glyph(covered))
                .set_fg(to_color(mean(samples, covered)));
        }
    }
}

pub fn to_ansi(fb: &RgbBuf, set: GlyphSet) -> String {
    let rows = fb.height / set.cell_h();
    let cols = fb.width / 2;
    let mut out = String::new();
    for row in 0..rows {
        for col in 0..cols {
            let (cell, n) = fb.cell(col, row, set);
            let p = plan(&cell[..n], set);
            match p.fg {
                Some(c) => out.push_str(&format!(
                    "\x1b[38;2;{};{};{}m",
                    byte(c.x),
                    byte(c.y),
                    byte(c.z)
                )),
                None => out.push_str("\x1b[39m"),
            }
            match p.bg {
                Some(c) => out.push_str(&format!(
                    "\x1b[48;2;{};{};{}m",
                    byte(c.x),
                    byte(c.y),
                    byte(c.z)
                )),
                None => out.push_str("\x1b[49m"),
            }
            out.push_str(p.symbol);
        }
        out.push_str("\x1b[0m\n");
    }
    out
}

fn byte(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Cells are planned as `blit` would and drawn at true terminal proportions, so quantization stays
/// visible.
pub fn to_png(fb: &RgbBuf, set: GlyphSet, path: &std::path::Path) -> std::io::Result<()> {
    let (cols, rows) = (fb.width / 2, fb.height / set.cell_h());
    let sub_h = 4 / set.cell_h() as u32;
    let img = image::RgbImage::from_fn(cols as u32 * 2, rows as u32 * 4, |x, y| {
        let (col, row) = ((x / 2) as u16, (y / 4) as u16);
        let (cell, n) = fb.cell(col, row, set);
        let p = plan(&cell[..n], set);
        let (sub_col, sub_row) = (x % 2, (y % 4) / sub_h);
        let index = sub_row * 2 + sub_col;
        let lit = (1usize << index) & mask_of(p.symbol, set) != 0;
        match if lit { p.fg } else { p.bg } {
            Some(c) => image::Rgb([byte(c.x), byte(c.y), byte(c.z)]),
            None => {
                let v = if (x / 8 + y / 16) % 2 == 0 { 60 } else { 75 };
                image::Rgb([v, v, v])
            }
        }
    });
    img.save(path)
        .map_err(|e| std::io::Error::other(e.to_string()))
}

fn mask_of(symbol: &str, set: GlyphSet) -> usize {
    (0..1usize << set.subpixels())
        .find(|&m| set.glyph(m) == symbol)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render3d::math::vec3;

    const RED: Vec3 = vec3(1.0, 0.0, 0.0);
    const BLUE: Vec3 = vec3(0.0, 0.0, 1.0);

    fn planned(samples: &[Option<Vec3>], set: GlyphSet) -> (&'static str, Color, Color) {
        let p = plan(samples, set);
        (p.symbol, to_color(p.fg), to_color(p.bg))
    }

    #[test]
    fn an_empty_cell_leaves_the_terminal_alone() {
        assert_eq!(
            planned(&[None; 4], GlyphSet::Quadrant),
            (" ", Color::Reset, Color::Reset)
        );
        assert_eq!(
            planned(&[None; 8], GlyphSet::Octant),
            (" ", Color::Reset, Color::Reset)
        );
    }

    #[test]
    fn quadrant_coverage_dictates_the_glyph_at_the_silhouette_edge() {
        let (sym, fg, bg) = planned(&[Some(RED), None, None, None], GlyphSet::Quadrant);
        assert_eq!(sym, "▘");
        assert_eq!(fg, Color::Rgb(255, 0, 0));
        assert_eq!(bg, Color::Reset);

        let (sym, ..) = planned(&[Some(RED), None, Some(RED), None], GlyphSet::Quadrant);
        assert_eq!(sym, "▌");
        let (sym, ..) = planned(&[Some(RED), Some(RED), Some(RED), None], GlyphSet::Quadrant);
        assert_eq!(sym, "▛");
    }

    #[test]
    fn a_full_quadrant_cell_splits_where_the_colors_actually_differ() {
        let (sym, fg, bg) = planned(
            &[Some(RED), Some(RED), Some(BLUE), Some(BLUE)],
            GlyphSet::Quadrant,
        );
        assert_eq!(sym, "▀");
        assert_eq!(fg, Color::Rgb(255, 0, 0));
        assert_eq!(bg, Color::Rgb(0, 0, 255));

        let (sym, fg, bg) = planned(
            &[Some(RED), Some(BLUE), Some(RED), Some(BLUE)],
            GlyphSet::Quadrant,
        );
        assert_eq!(sym, "▌");
        assert_eq!(fg, Color::Rgb(255, 0, 0));
        assert_eq!(bg, Color::Rgb(0, 0, 255));
    }

    #[test]
    fn a_flat_cell_becomes_a_solid_block_in_either_family() {
        let (sym, fg, _) = planned(&[Some(RED); 4], GlyphSet::Quadrant);
        assert_eq!(sym, "█");
        assert_eq!(fg, Color::Rgb(255, 0, 0));
        let (sym, fg, _) = planned(&[Some(RED); 8], GlyphSet::Octant);
        assert_eq!(sym, "█");
        assert_eq!(fg, Color::Rgb(255, 0, 0));
    }

    #[test]
    fn octant_coverage_uses_the_finer_grid() {
        let mut cell = [None; 8];
        cell[0] = Some(RED);
        cell[1] = Some(RED);
        let (sym, fg, bg) = planned(&cell, GlyphSet::Octant);
        assert_eq!(sym, "\u{1FB82}", "upper one quarter");
        assert_eq!(fg, Color::Rgb(255, 0, 0));
        assert_eq!(bg, Color::Reset);

        let mut cell = [None; 8];
        for c in cell.iter_mut().take(4) {
            *c = Some(RED);
        }
        assert_eq!(planned(&cell, GlyphSet::Octant).0, "▀");
    }

    /// Every octant mask maps to a glyph that maps back to the same mask.
    #[test]
    fn octant_masks_round_trip() {
        for mask in 0..256usize {
            let sym = GlyphSet::Octant.glyph(mask);
            assert!(!sym.is_empty(), "mask {mask:#04x} has no glyph");
            assert_eq!(
                mask_of(sym, GlyphSet::Octant),
                mask,
                "mask {mask:#04x} → {sym:?} did not round-trip"
            );
        }
    }

    #[test]
    fn octant_table_uses_every_dedicated_code_point_once() {
        use std::collections::HashSet;
        let syms: HashSet<_> = (0..256).map(|m| GlyphSet::Octant.glyph(m)).collect();
        assert_eq!(syms.len(), 256, "duplicate glyphs in the octant table");
    }

    #[test]
    fn blit_maps_subpixels_onto_one_cell() {
        let mut fb = RgbBuf::new(4, 2);
        fb.set(0, 0, Some(RED));
        fb.set(1, 0, Some(RED));
        fb.set(0, 1, Some(BLUE));
        fb.set(1, 1, Some(BLUE));
        let area = Rect::new(0, 0, 2, 1);
        let mut buf = Buffer::empty(area);
        blit(&fb, area, &mut buf, GlyphSet::Quadrant);

        assert_eq!(buf[(0, 0)].symbol(), "▀");
        assert_eq!(buf[(0, 0)].fg, Color::Rgb(255, 0, 0));
        assert_eq!(buf[(0, 0)].bg, Color::Rgb(0, 0, 255));
        assert_eq!(buf[(1, 0)].symbol(), " ");
    }

    #[test]
    fn blit_sparse_composites_instead_of_clobbering() {
        let mut fb = RgbBuf::new(4, 2);
        fb.set(0, 0, Some(RED));

        let area = Rect::new(0, 0, 2, 1);
        let mut buf = Buffer::empty(area);
        for x in 0..2 {
            buf[(x, 0)].set_symbol("X").set_bg(Color::Rgb(9, 9, 9));
        }
        blit_sparse(&fb, area, &mut buf, GlyphSet::Quadrant);

        assert_eq!(buf[(0, 0)].symbol(), "▘");
        assert_eq!(buf[(0, 0)].fg, Color::Rgb(255, 0, 0));
        assert_eq!(buf[(0, 0)].bg, Color::Rgb(9, 9, 9));
        assert_eq!(buf[(1, 0)].symbol(), "X");
        assert_eq!(buf[(1, 0)].bg, Color::Rgb(9, 9, 9));
    }

    #[test]
    fn blit_sparse_takes_the_coverage_split_on_a_full_cell() {
        // A fully covered cell must not trigger the error search: solid block, ink is the mean.
        let mut fb = RgbBuf::new(2, 2);
        for (x, y) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            fb.set(x, y, Some(if y == 0 { RED } else { BLUE }));
        }
        let area = Rect::new(0, 0, 1, 1);
        let mut buf = Buffer::empty(area);
        blit_sparse(&fb, area, &mut buf, GlyphSet::Quadrant);

        assert_eq!(buf[(0, 0)].symbol(), "█");
        assert_eq!(buf[(0, 0)].fg, Color::Rgb(128, 0, 128));
    }

    #[test]
    fn ansi_dump_emits_one_line_per_cell_row() {
        let mut fb = RgbBuf::new(6, 4);
        fb.set(1, 1, Some(vec3(0.5, 0.5, 0.5)));
        let text = to_ansi(&fb, GlyphSet::Quadrant);
        assert_eq!(text.lines().count(), 2);
        assert!(text.contains("128"));
        assert_eq!(to_ansi(&fb, GlyphSet::Octant).lines().count(), 1);
    }
}
