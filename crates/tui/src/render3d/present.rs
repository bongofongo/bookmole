//! The book presentation seam: [`BookPresenter`] with glyph and kitty-pixel backends.
//! Callers pass borrows read out of `App` first; `present_book` needs disjoint field borrows.

use std::io::Write;

use bookmole::Book;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Color;

use super::raster::{Quality, Target};
use super::{Caps, Model, RenderMode, RenderParams, Scene, blit, kitty, raster};
use crate::perf;

pub trait BookPresenter {
    fn draw_book(&mut self, f: &mut Frame, area: Rect, book: &Book, params: RenderParams);
}

pub struct GlyphPresenter<'a> {
    scene: &'a mut Scene,
}

impl<'a> GlyphPresenter<'a> {
    pub fn new(scene: &'a mut Scene) -> Self {
        GlyphPresenter { scene }
    }
}

impl BookPresenter for GlyphPresenter<'_> {
    fn draw_book(&mut self, f: &mut Frame, area: Rect, book: &Book, params: RenderParams) {
        let fb = {
            let _t = perf::scope(perf::Stage::Trace);
            self.scene.frame(book, area.width, area.height, params)
        };
        perf::note_rect(area.width, area.height);
        let _t = perf::scope(perf::Stage::Blit);
        blit::blit(fb, area, f.buffer_mut(), params.glyphs);
    }
}

const FINE_Q: f32 = 512.0;

const MOTION_Q: f32 = 32.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RichKey {
    cover: u64,
    yaw_q: i32,
    pitch_q: i32,
    px_w: u32,
    px_h: u32,
    cols: u16,
    rows: u16,
    /// Position matters as well as size: a moved-only frame is re-sent as cheap insurance against
    /// the same fragility as a resize.
    x: u16,
    y: u16,
    quality: Quality,
}

fn cover_hash(book: &Book) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    book.id.hash(&mut h);
    book.cover_path.hash(&mut h);
    book.display_title().hash(&mut h);
    // A back-fill repaints the spine without moving the path, so the accent must be in the key.
    book.cover_accent_rgb().hash(&mut h);
    book.page_count.hash(&mut h);
    h.finish()
}

pub struct RichState {
    caps: Caps,
    id: u32,
    sent: Option<RichKey>,
    last_pose: Option<(i32, i32)>,
    span: (u16, u16),
    placeholders: Vec<String>,
    out: Box<dyn Write + Send>,
}

impl Default for RichState {
    fn default() -> Self {
        RichState {
            caps: Caps::default(),
            id: kitty::image_id(),
            sent: None,
            last_pose: None,
            span: (0, 0),
            placeholders: Vec::new(),
            out: Box::new(std::io::stdout()),
        }
    }
}

impl RichState {
    pub fn set_caps(&mut self, caps: Caps) {
        self.caps = caps;
    }

    pub fn count_bytes(&mut self, meter: perf::Meter) {
        self.out = Box::new(perf::CountingWriter::new(
            std::io::stdout(),
            meter,
            perf::ByteClass::Image,
        ));
    }

    #[cfg(test)]
    pub fn with_writer(caps: Caps, out: Box<dyn Write + Send>) -> Self {
        RichState {
            caps,
            out,
            ..RichState::default()
        }
    }

    /// Must not clear `last_pose` (settle detection) or the placeholder table (costly to rebuild).
    fn hide_image(&mut self) {
        if self.sent.is_some() {
            let esc = kitty::delete(self.id, self.caps.in_tmux);
            let _ = self.out.write_all(esc.as_bytes());
            let _ = self.out.flush();
            kitty::forget_live();
        }
        self.sent = None;
    }

    /// Resets the frame on a terminal resize: capped stacked layouts often leave the object rect
    /// unchanged, so the transmit cache would miss the resize and the image would stay lost.
    pub fn resized(&mut self) {
        self.hide_image();
    }

    pub fn drop_image(&mut self) {
        self.hide_image();
        self.last_pose = None;
        self.span = (0, 0);
        self.placeholders.clear();
    }

    fn ensure_placeholders(&mut self, cols: u16, rows: u16) -> bool {
        if cols > kitty::MAX_SPAN || rows > kitty::MAX_SPAN {
            return false;
        }
        if self.span == (cols, rows) && !self.placeholders.is_empty() {
            return true;
        }
        self.placeholders.clear();
        self.placeholders.reserve(cols as usize * rows as usize);
        for r in 0..rows {
            for c in 0..cols {
                match kitty::placeholder_symbol(r, c) {
                    Some(s) => self.placeholders.push(s),
                    None => return false,
                }
            }
        }
        self.span = (cols, rows);
        true
    }
}

/// Escapes go straight to the writer inside `draw_book`; that is safe because `Terminal::draw`
/// only flushes afterwards, so the image lands before the cells that reference it.
pub struct RichPresenter<'a> {
    scene: &'a mut Scene,
    state: &'a mut RichState,
    hybrid: bool,
}

impl<'a> RichPresenter<'a> {
    pub fn new(scene: &'a mut Scene, state: &'a mut RichState) -> Self {
        RichPresenter {
            scene,
            state,
            hybrid: true,
        }
    }

    pub fn always(scene: &'a mut Scene, state: &'a mut RichState) -> Self {
        RichPresenter {
            scene,
            state,
            hybrid: false,
        }
    }

    fn draw_pixels(
        &mut self,
        f: &mut Frame,
        area: Rect,
        book: &Book,
        params: RenderParams,
    ) -> bool {
        if !self.state.caps.supports_pixels() {
            return false;
        }

        // The fine pose decides moving vs parked; `params.moving` is authoritative when supplied, since
        // a parked book stops redrawing and never yields the second draw this needs.
        let fine = (
            (params.pose.yaw * FINE_Q).round() as i32,
            (params.pose.pitch * FINE_Q).round() as i32,
        );
        let moving = params
            .moving
            .unwrap_or_else(|| self.state.last_pose != Some(fine));
        self.state.last_pose = Some(fine);
        let quality = if moving {
            Quality::Motion
        } else {
            Quality::Settle
        };

        // Hybrid rule: no images while animating. Hide the image first, while ratatui is still
        // only mutating its buffer.
        if self.hybrid && quality == Quality::Motion {
            self.state.hide_image();
            perf::note(perf::FrameNote {
                quality: perf::Quality::Motion,
                cols: area.width,
                rows: area.height,
                fell_back: true,
                ..perf::FrameNote::default()
            });
            return false;
        }

        if !self.state.ensure_placeholders(area.width, area.height) {
            return false;
        }

        // The coarse pose keys the cache while moving, throttling retransmits; a settled pose uses
        // the fine key so the crisp frame is never deduped away.
        let key_pose = if quality == Quality::Settle {
            fine
        } else {
            (
                (params.pose.yaw * MOTION_Q).round() as i32,
                (params.pose.pitch * MOTION_Q).round() as i32,
            )
        };

        let target = raster::target_for(
            area.width,
            area.height,
            self.state.caps.cell_px,
            quality,
            self.state.caps.image_wire,
        );
        let key = RichKey {
            cover: cover_hash(book),
            yaw_q: key_pose.0,
            pitch_q: key_pose.1,
            px_w: target.width,
            px_h: target.height,
            cols: area.width,
            rows: area.height,
            x: area.x,
            y: area.y,
            quality,
        };

        let mut transmitted = false;
        if self.state.sent != Some(key) {
            if self
                .state
                .sent
                .is_some_and(|s| (s.cols, s.rows) != (key.cols, key.rows))
            {
                self.state.hide_image();
            }
            if !self.transmit(book, target, params, area) {
                return false;
            }
            self.state.sent = Some(key);
            transmitted = true;
        }
        perf::note(perf::FrameNote {
            quality: match quality {
                Quality::Motion => perf::Quality::Motion,
                Quality::Settle => perf::Quality::Settle,
            },
            cols: area.width,
            rows: area.height,
            px_w: target.width,
            px_h: target.height,
            transmitted,
            fell_back: false,
        });

        self.place(f, area);
        true
    }

    fn transmit(&mut self, book: &Book, target: Target, params: RenderParams, area: Rect) -> bool {
        let texels = target.width.clamp(24, 2048);
        let img = {
            let _t = perf::scope(perf::Stage::Trace);
            let cover = self.scene.cover(book, texels);
            let model = Model::new(book, cover);
            raster::render_rgba(target, &model, cover, params)
        };
        let esc = {
            let _t = perf::scope(perf::Stage::Encode);
            kitty::transmit(
                &img,
                self.state.id,
                area.width,
                area.height,
                self.state.caps.in_tmux,
                self.state.caps.image_wire,
            )
        };
        if esc.is_empty() {
            return false;
        }
        let _t = perf::scope(perf::Stage::Transmit);
        if self.state.out.write_all(esc.as_bytes()).is_err() || self.state.out.flush().is_err() {
            return false;
        }
        kitty::register_live(self.state.id, self.state.caps.in_tmux);
        true
    }

    fn place(&mut self, f: &mut Frame, area: Rect) {
        let fg = kitty::id_color(self.state.id);
        let buf = f.buffer_mut();
        for row in 0..area.height {
            for col in 0..area.width {
                let symbol =
                    &self.state.placeholders[row as usize * area.width as usize + col as usize];
                buf[(area.x + col, area.y + row)]
                    .set_symbol(symbol)
                    .set_fg(fg)
                    .set_bg(Color::Reset);
            }
        }
    }
}

impl BookPresenter for RichPresenter<'_> {
    fn draw_book(&mut self, f: &mut Frame, area: Rect, book: &Book, params: RenderParams) {
        if self.draw_pixels(f, area, book, params) {
            return;
        }
        GlyphPresenter::new(self.scene).draw_book(f, area, book, params);
    }
}

pub fn presenter_for<'a>(
    mode: RenderMode,
    scene: &'a mut Scene,
    rich: &'a mut RichState,
) -> Box<dyn BookPresenter + 'a> {
    match mode {
        RenderMode::Glyph => Box::new(GlyphPresenter::new(scene)),
        RenderMode::Rich => Box::new(RichPresenter::new(scene, rich)),
        RenderMode::RichAlways => Box::new(RichPresenter::always(scene, rich)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::SPIN_SPEED;
    use crate::render3d::caps::Passthrough;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Sink {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).to_string()
        }
        fn len(&self) -> usize {
            self.0.lock().unwrap().len()
        }
    }

    fn capable() -> Caps {
        Caps {
            kitty_graphics: true,
            image_wire: crate::render3d::ImageWire::Zlib,
            cell_px: (20, 38),
            cell_px_measured: true,
            in_tmux: false,
            passthrough: Passthrough::NotTmux,
        }
    }

    fn book() -> Book {
        Book {
            id: Some(1),
            title: Some("Station Eleven".into()),
            ..Book::default()
        }
    }

    fn scene() -> Scene {
        Scene::new(std::path::PathBuf::from("images"))
    }

    fn draw_at(
        state: &mut RichState,
        scene: &mut Scene,
        w: u16,
        h: u16,
        moving: bool,
    ) -> ratatui::buffer::Buffer {
        draw_with(state, scene, w, h, params(moving), true)
    }

    fn params(moving: bool) -> RenderParams {
        RenderParams {
            moving: Some(moving),
            ..RenderParams::default()
        }
    }

    fn draw_with(
        state: &mut RichState,
        scene: &mut Scene,
        w: u16,
        h: u16,
        params: RenderParams,
        hybrid: bool,
    ) -> ratatui::buffer::Buffer {
        let mut term = Terminal::new(TestBackend::new(w, h)).expect("backend");
        let area = Rect::new(0, 0, w, h);
        term.draw(|f| {
            let mut p = if hybrid {
                RichPresenter::new(scene, state)
            } else {
                RichPresenter::always(scene, state)
            };
            p.draw_book(f, area, &book(), params);
        })
        .expect("draw");
        term.backend().buffer().clone()
    }

    fn placeholder_count(buf: &ratatui::buffer::Buffer, w: u16, h: u16) -> usize {
        (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .filter(|&(x, y)| buf[(x, y)].symbol().starts_with(kitty::PLACEHOLDER))
            .count()
    }

    #[test]
    fn a_parked_book_gets_an_image_and_placeholder_cells() {
        let sink = Sink::default();
        let mut state = RichState::with_writer(capable(), Box::new(sink.clone()));
        let mut sc = scene();
        let buf = draw_at(&mut state, &mut sc, 20, 10, false);

        let esc = sink.text();
        assert!(esc.contains("a=T"), "no image was transmitted");
        assert!(esc.contains("U=1"), "placement was not virtual");
        assert!(esc.contains("c=20,r=10"), "wrong cell span");

        let fg = kitty::id_color(state.id);
        for y in 0..10 {
            for x in 0..20 {
                let cell = &buf[(x, y)];
                assert!(
                    cell.symbol().starts_with(kitty::PLACEHOLDER),
                    "cell {x},{y} is not a placeholder: {:?}",
                    cell.symbol()
                );
                assert_eq!(cell.fg, fg, "cell {x},{y} lost the image id");
            }
        }
    }

    #[test]
    fn a_moving_book_sends_no_image_bytes_at_all() {
        let sink = Sink::default();
        let mut state = RichState::with_writer(capable(), Box::new(sink.clone()));
        let mut sc = scene();
        let mut p = params(true);

        for _ in 0..60 {
            p.pose.yaw += SPIN_SPEED;
            draw_with(&mut state, &mut sc, 20, 10, p, true);
        }
        assert_eq!(
            sink.len(),
            0,
            "the spin put {} bytes of image on the wire",
            sink.len()
        );
    }

    #[test]
    fn a_moving_book_draws_glyphs_instead() {
        let sink = Sink::default();
        let mut state = RichState::with_writer(capable(), Box::new(sink.clone()));
        let mut sc = scene();
        let buf = draw_at(&mut state, &mut sc, 20, 10, true);

        assert_eq!(placeholder_count(&buf, 20, 10), 0, "placed a phantom image");
        let painted = (0..10)
            .flat_map(|y| (0..20).map(move |x| (x, y)))
            .filter(|&(x, y)| buf[(x, y)].symbol() != " ")
            .count();
        assert!(painted > 0, "the moving book was not drawn at all");
    }

    #[test]
    fn parking_transmits_exactly_one_image() {
        let sink = Sink::default();
        let mut state = RichState::with_writer(capable(), Box::new(sink.clone()));
        let mut sc = scene();

        draw_at(&mut state, &mut sc, 20, 10, true);
        assert_eq!(sink.len(), 0, "moving frame was not free");

        draw_at(&mut state, &mut sc, 20, 10, false);
        let after_park = sink.len();
        assert!(after_park > 0, "parking never produced the crisp frame");

        draw_at(&mut state, &mut sc, 20, 10, false);
        draw_at(&mut state, &mut sc, 20, 10, false);
        assert_eq!(sink.len(), after_park, "a still book kept retransmitting");
    }

    #[test]
    fn a_libghostty_host_is_never_sent_a_compressed_frame() {
        // The settle frame is the one libghostty crashes on and the only one the hybrid transmits, so
        // `Raw` must reach the escape through four call sites.
        let caps = Caps {
            image_wire: crate::render3d::ImageWire::Raw,
            ..capable()
        };
        let sink = Sink::default();
        let mut state = RichState::with_writer(caps, Box::new(sink.clone()));
        let mut sc = scene();

        draw_at(&mut state, &mut sc, 20, 10, false);
        let sent = sink.text();
        assert!(sent.contains("a=T"), "no image was transmitted");
        assert!(!sent.contains("o=z"), "compressed payload reached the host");

        let raw_cap = raster::RAW_SETTLE_MAX_PX;
        let t = raster::target_for(20, 10, caps.cell_px, Quality::Settle, caps.image_wire);
        assert!(
            (t.width as u64 * t.height as u64) <= raw_cap as u64,
            "{}x{} is past the raw budget",
            t.width,
            t.height
        );
    }

    #[test]
    fn resuming_motion_deletes_the_image_before_the_glyphs_land() {
        let sink = Sink::default();
        let mut state = RichState::with_writer(capable(), Box::new(sink.clone()));
        let mut sc = scene();

        draw_at(&mut state, &mut sc, 20, 10, false);
        let before = sink.len();
        draw_at(&mut state, &mut sc, 20, 10, true);

        let tail = &sink.text()[before..];
        assert!(
            tail.contains(&format!("a=d,d=I,i={}", state.id)),
            "resuming the spin left the image on screen: {tail:?}"
        );
    }

    #[test]
    fn a_park_after_motion_still_transmits() {
        let sink = Sink::default();
        let mut state = RichState::with_writer(capable(), Box::new(sink.clone()));
        let mut sc = scene();

        for cycle in 0..3 {
            draw_at(&mut state, &mut sc, 20, 10, true);
            let before = sink.len();
            draw_at(&mut state, &mut sc, 20, 10, false);
            assert!(
                sink.len() > before,
                "cycle {cycle}: parking produced no image"
            );
        }
    }

    #[test]
    fn an_incapable_terminal_emits_nothing_and_falls_back_to_glyphs() {
        let sink = Sink::default();
        let mut state = RichState::with_writer(Caps::default(), Box::new(sink.clone()));
        let mut sc = scene();
        let buf = draw_at(&mut state, &mut sc, 20, 10, false);

        assert_eq!(
            sink.len(),
            0,
            "escapes leaked to a terminal that can't take them"
        );
        assert_eq!(
            placeholder_count(&buf, 20, 10),
            0,
            "placed an image with no protocol for it"
        );
    }

    #[test]
    fn a_resize_retransmits_with_the_new_span() {
        let sink = Sink::default();
        let mut state = RichState::with_writer(capable(), Box::new(sink.clone()));
        let mut sc = scene();
        draw_at(&mut state, &mut sc, 20, 10, false);
        draw_at(&mut state, &mut sc, 30, 12, false);
        let esc = sink.text();
        assert!(esc.contains("c=20,r=10"), "first span missing");
        assert!(esc.contains("c=30,r=12"), "resize did not re-place");
    }

    #[test]
    fn the_pose_inference_still_works_without_an_explicit_motion_flag() {
        let sink = Sink::default();
        let mut state = RichState::with_writer(capable(), Box::new(sink.clone()));
        let mut sc = scene();
        let p = RenderParams::default();
        assert!(p.moving.is_none());

        draw_with(&mut state, &mut sc, 20, 10, p, true);
        let after_first = sink.len();
        draw_with(&mut state, &mut sc, 20, 10, p, true);
        assert!(sink.len() > after_first, "inference never settled");
    }

    #[test]
    fn a_spinning_book_retransmits_well_below_the_tick_rate() {
        // `RichAlways` must stay bad but survivable: retransmitting every tick or losing the throttle
        // would invalidate the latency calibration.
        const SPIN: f32 = SPIN_SPEED;
        let sink = Sink::default();
        let mut state = RichState::with_writer(capable(), Box::new(sink.clone()));
        let mut sc = scene();
        let mut p = params(true);

        let ticks = 60;
        let mut transmits = 0;
        for _ in 0..ticks {
            p.pose.yaw += SPIN;
            let before = sink.len();
            draw_with(&mut state, &mut sc, 20, 10, p, false);
            if sink.len() > before {
                transmits += 1;
            }
        }
        assert!(
            transmits <= ticks / 2,
            "{transmits} transmits in {ticks} ticks — the spin throttle is gone"
        );
        assert!(transmits > 0, "the spin stopped transmitting entirely");
    }

    #[test]
    fn oversized_rects_fall_back_rather_than_clip() {
        let sink = Sink::default();
        let mut state = RichState::with_writer(capable(), Box::new(sink.clone()));
        assert!(!state.ensure_placeholders(kitty::MAX_SPAN + 1, 10));
        assert!(state.ensure_placeholders(kitty::MAX_SPAN, 10));
    }

    #[test]
    fn dropping_the_image_emits_a_delete_and_forgets_it() {
        let sink = Sink::default();
        let mut state = RichState::with_writer(capable(), Box::new(sink.clone()));
        let mut sc = scene();
        draw_at(&mut state, &mut sc, 20, 10, false);
        state.drop_image();
        assert!(
            sink.text().contains(&format!("a=d,d=I,i={}", state.id)),
            "leaving rich mode must delete the image or it ghosts under the glyphs"
        );
        let after = sink.len();
        state.drop_image();
        assert_eq!(sink.len(), after);
    }

    #[test]
    fn every_rect_size_draws_without_panicking() {
        let sink = Sink::default();
        let mut state = RichState::with_writer(capable(), Box::new(sink.clone()));
        let mut sc = scene();
        for (w, h) in [(1u16, 1u16), (1, 20), (20, 1), (3, 2), (40, 20)] {
            for moving in [true, false] {
                draw_at(&mut state, &mut sc, w, h, moving);
            }
        }
    }
}
