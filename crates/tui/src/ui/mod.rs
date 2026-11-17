pub mod apikey;
pub mod book;
pub mod calibre;
pub mod device;
pub mod goodreads;
pub mod help;
pub mod home;
pub mod input;
pub mod library;
pub mod menu;
pub mod search;
pub mod settings;
pub mod textedit;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Padding, Paragraph, Wrap};

use crate::app::{App, Screen};
use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookLayout {
    Split(PaneOrientation),
    Compact,
}

/// Rotation order is clockwise: Top, Right, Bottom, Left.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneOrientation {
    BookTop,
    BookRight,
    BookBottom,
    BookLeft,
}

impl PaneOrientation {
    const ALL: [PaneOrientation; 4] = [
        PaneOrientation::BookTop,
        PaneOrientation::BookRight,
        PaneOrientation::BookBottom,
        PaneOrientation::BookLeft,
    ];

    pub fn rotate_cw(self, steps: u8) -> PaneOrientation {
        let base = Self::ALL.iter().position(|o| *o == self).unwrap_or(0);
        Self::ALL[(base + steps as usize) % 4]
    }

    fn is_vertical_divider(self) -> bool {
        matches!(self, PaneOrientation::BookLeft | PaneOrientation::BookRight)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutPrefs {
    pub panel: bool,
    /// Quarter-turns clockwise on top of the aspect default (0..=3).
    pub rotation: u8,
    /// Fraction of the split axis added to the default divider position.
    pub divider_bias: f32,
}

impl Default for LayoutPrefs {
    fn default() -> Self {
        Self {
            panel: true,
            rotation: 0,
            divider_bias: 0.0,
        }
    }
}

const MIN_SPLIT_WIDTH: u16 = 26;
const MIN_SPLIT_HEIGHT: u16 = 8;

const BOOK_MAX_COLS: u16 = 50;
const PANEL_MIN: u16 = 18;
/// Chosen so `BOOK_MAX_COLS + PANEL_MAX_COLS` is exactly the width at which `split_widths`
/// returns `(50, 48)`: the two caps must agree.
const PANEL_MAX_COLS: u16 = 48;
/// Stacked layouts give the panel the whole width; height is left uncapped on purpose.
const STACK_MAX_COLS: u16 = 84;
const LIST_MIN_COLS: u16 = 34;
const LIST_MAX_COLS: u16 = 100;
const LIST_MIN_ROWS: u16 = 7;
/// Columns a list row needs around it: two borders, the highlight gutter, one padding each side.
const LIST_CHROME: u16 = 6;
pub const BOOK_MAX_ROWS: u16 = 26;

const MIN_PANE: u16 = 6;
const DIVIDER_MIN_FRAC: f32 = 0.18;
const DIVIDER_MAX_FRAC: f32 = 0.82;
pub const DIVIDER_STEP: f32 = 0.06;

pub fn book_layout(area: Rect, rotation: u8) -> BookLayout {
    if area.height < MIN_SPLIT_HEIGHT || area.width < MIN_SPLIT_WIDTH {
        BookLayout::Compact
    } else {
        BookLayout::Split(effective_orientation(area, rotation))
    }
}

pub fn effective_orientation(area: Rect, rotation: u8) -> PaneOrientation {
    aspect_default(area).rotate_cw(rotation)
}

fn aspect_default(area: Rect) -> PaneOrientation {
    if portrait(area) {
        PaneOrientation::BookTop
    } else {
        PaneOrientation::BookRight
    }
}

/// True when the pane is taller than wide; a cell is ~1 wide : 2 tall, so height is doubled.
fn portrait(area: Rect) -> bool {
    area.height as u32 * 2 > area.width as u32
}

pub fn split_widths(width: u16) -> (u16, u16) {
    let object = BOOK_MAX_COLS.min(width.saturating_sub(PANEL_MIN)).max(1);
    (object, width - object)
}

pub fn stacked_object_height(height: u16) -> u16 {
    (height / 2).clamp(1, BOOK_MAX_ROWS)
}

/// The rect the book view occupies inside `area`, width-capped and centred.
/// Orientation comes from the uncapped pane; deriving it from the capped rect would be circular.
pub fn content_block(area: Rect, o: PaneOrientation) -> Rect {
    let max = if o.is_vertical_divider() {
        BOOK_MAX_COLS + PANEL_MAX_COLS
    } else {
        STACK_MAX_COLS
    };
    let width = area.width.min(max);
    Rect {
        x: area.x + (area.width - width) / 2,
        width,
        ..area
    }
}

/// The object's and panel's rects: the object slides toward centre while `split_rects` still
/// sizes against `content_block`. Horizontal dividers pass straight through.
pub fn book_rects(area: Rect, o: PaneOrientation, bias: f32) -> (Rect, Rect, Borders) {
    let (object, panel, border) = split_rects(content_block(area, o), o, bias);
    if !o.is_vertical_divider() {
        return (object, panel, border);
    }

    let w = object.width;
    let rest = area.width.saturating_sub(w);
    let floor = PANEL_MIN.min(rest);
    let ideal = area.x + rest / 2;

    match o {
        PaneOrientation::BookLeft => {
            let x = ideal.min(area.x + rest - floor).max(area.x);
            let pw = (area.x + area.width - (x + w)).min(PANEL_MAX_COLS);
            let object = Rect {
                x,
                width: w,
                ..area
            };
            let panel = Rect {
                x: x + w,
                width: pw,
                ..area
            };
            (object, panel, Borders::LEFT)
        }
        _ => {
            let x = ideal.max(area.x + floor).min(area.x + rest);
            let pw = (x - area.x).min(PANEL_MAX_COLS);
            let object = Rect {
                x,
                width: w,
                ..area
            };
            let panel = Rect {
                x: x - pw,
                width: pw,
                ..area
            };
            (object, panel, Borders::RIGHT)
        }
    }
}

pub fn list_box(area: Rect, content: u16, rows: u16) -> Rect {
    let width = content
        .saturating_add(LIST_CHROME)
        .clamp(LIST_MIN_COLS, LIST_MAX_COLS);
    let height = rows.saturating_add(2).max(LIST_MIN_ROWS);
    centered(area, width, height)
}

/// Clip for a shelf row's detail, so one long error cannot set the box width, yet wide enough
/// that the action hint after a title survives.
pub(crate) const DETAIL_MAX: usize = 72;

/// `maybe “<title>” (71%) — <hint>`. The title is clipped to what the hint leaves, so the
/// hint is never the part truncated.
pub(crate) fn candidate_hint(title: &str, score: f64, hint: &str) -> String {
    let pct = format!("{:.0}", score * 100.0);
    let fixed = format!("maybe “” ({pct}%) — {hint}").chars().count();
    // Floor so a long hint still leaves some title; the caller's own clip bounds the result.
    let budget = DETAIL_MAX.saturating_sub(fixed).max(8);
    format!(
        "maybe “{}” ({pct}%) — {hint}",
        clip(title.to_string(), budget)
    )
}

pub(crate) fn clip(s: String, max: usize) -> String {
    if s.chars().count() <= max {
        return s;
    }
    // `max - 1`, so the ellipsis is inside the budget.
    s.chars()
        .take(max - 1)
        .chain(std::iter::once('…'))
        .collect()
}

/// One shelf row's frame: shrink-wrapped box with the key bar in the bottom border, or the
/// empty-state paragraph. Returns `false` when it drew the empty state.
pub(crate) fn shelf_frame(
    f: &mut Frame,
    area: Rect,
    title: String,
    keys: Line<'static>,
    rows: &[Line<'static>],
    empty_hint: &str,
) -> Option<(Rect, Block<'static>)> {
    let widest = rows
        .iter()
        .map(|l| l.width() as u16)
        .max()
        .unwrap_or(empty_hint.chars().count() as u16)
        .max(title.chars().count() as u16)
        .max(keys.width() as u16);
    let area = list_box(area, widest, rows.len() as u16);
    f.render_widget(ratatui::widgets::Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::dim())
        .padding(Padding::horizontal(1))
        .title(Span::styled(title, theme::accent()))
        .title_bottom(keys.centered());

    if rows.is_empty() {
        let inner = block.inner(area);
        f.render_widget(block, area);
        // A `Block` on a 1×1 pane has no inside, and `Paragraph` into a zero rect panics.
        if inner.width > 0 && inner.height > 0 {
            f.render_widget(
                Paragraph::new(empty_hint.to_string())
                    .style(theme::dim())
                    .wrap(Wrap { trim: true }),
                inner,
            );
        }
        return None;
    }
    Some((area, block))
}

pub(crate) fn link_picker(f: &mut Frame, picker: &mut crate::app::LinkPicker, area: Rect) {
    let selected = picker.state.selected();
    let rows: Vec<Line> = picker
        .candidates
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let title = if Some(i) == selected {
                theme::primary().patch(theme::selected())
            } else {
                theme::primary()
            };
            Line::from(vec![
                Span::styled(c.title.clone(), title),
                Span::styled(format!("  {:.0}%", c.score * 100.0), theme::dim()),
            ])
        })
        .collect();
    let widest = rows.iter().map(|r| r.width() as u16).max().unwrap_or(20);
    let title = format!(" link “{}” to ", picker.title);
    let width = widest
        .max(title.chars().count() as u16)
        .saturating_add(LIST_CHROME);
    let box_area = centered(area, width, rows.len() as u16 + 2);
    f.render_widget(ratatui::widgets::Clear, box_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::accent())
        .padding(Padding::horizontal(1))
        .title(Span::styled(title, theme::accent()))
        .title_bottom(
            Line::from(vec![
                Span::styled(" enter", theme::key()),
                Span::styled(" link  ", theme::dim()),
                Span::styled("esc", theme::key()),
                Span::styled(" leave it ", theme::dim()),
            ])
            .centered(),
        );
    let items: Vec<ListItem> = rows.into_iter().map(ListItem::new).collect();
    let list = List::new(items).block(block).highlight_symbol("› ");
    f.render_stateful_widget(list, box_area, &mut picker.state);
}

pub fn split_rects(area: Rect, o: PaneOrientation, bias: f32) -> (Rect, Rect, Borders) {
    if o.is_vertical_divider() {
        let default = split_widths(area.width).0 as f32 / area.width as f32;
        let bw = biased_span(area.width, default, bias);
        let pw = area.width - bw;
        match o {
            PaneOrientation::BookLeft => {
                let book = Rect { width: bw, ..area };
                let panel = Rect {
                    x: area.x + bw,
                    width: pw,
                    ..area
                };
                (book, panel, Borders::LEFT)
            }
            _ => {
                let panel = Rect { width: pw, ..area };
                let book = Rect {
                    x: area.x + pw,
                    width: bw,
                    ..area
                };
                (book, panel, Borders::RIGHT)
            }
        }
    } else {
        let default = stacked_object_height(area.height) as f32 / area.height as f32;
        let bh = biased_span(area.height, default, bias);
        let ph = area.height - bh;
        match o {
            PaneOrientation::BookTop => {
                let book = Rect { height: bh, ..area };
                let panel = Rect {
                    y: area.y + bh,
                    height: ph,
                    ..area
                };
                (book, panel, Borders::TOP)
            }
            _ => {
                let panel = Rect { height: ph, ..area };
                let book = Rect {
                    y: area.y + ph,
                    height: bh,
                    ..area
                };
                (book, panel, Borders::BOTTOM)
            }
        }
    }
}

fn biased_span(axis: u16, default_frac: f32, bias: f32) -> u16 {
    let frac = (default_frac + bias).clamp(DIVIDER_MIN_FRAC, DIVIDER_MAX_FRAC);
    let span = (frac * axis as f32).round() as u16;
    span.clamp(MIN_PANE, axis.saturating_sub(MIN_PANE).max(1))
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let status_line = confirm_prompt(app).or_else(|| app.status.clone());
    let status_h = if status_line.is_some() { 1 } else { 0 };
    let [body, status] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(status_h)]).areas(area);

    // Drawn first, under everything: screens `Clear` their content boxes, because a `Block`
    // only styles cells it does not draw.
    if app.ambient_visible() {
        let glyphs = app.params.glyphs;
        app.ambient.draw(f.buffer_mut(), body, glyphs);
    }

    match app.screen {
        Screen::Home => home::draw(f, app, body),
        Screen::Menu => menu::draw(f, app, body),
        Screen::Library => library::draw(f, app, body),
        Screen::Book => book::draw(f, app, body),
        Screen::Search => search::draw(f, app, body),
        Screen::Settings => settings::draw(f, app, body),
        Screen::Device => device::draw(f, app, body),
        Screen::Calibre => calibre::draw(f, app, body),
        Screen::Goodreads => goodreads::draw(f, app, body),
    }

    if app.help {
        help::render(f, body, app.screen);
    }

    if let Some(msg) = &status_line {
        let style = if app.confirm.is_some() {
            theme::accent()
        } else {
            theme::dim()
        };
        f.render_widget(Paragraph::new(msg.as_str()).style(style), status);
    }

    if let Some(inp) = &app.input {
        let box_area = centered(body, 50.min(body.width), 3);
        f.render_widget(ratatui::widgets::Clear, box_area);
        input::render(f, box_area, inp.prompt, &inp.state);
    }

    if let Some(draft) = &app.note_editor {
        let w = 54.min(body.width);
        let h = 10.min(body.height).max(3);
        let box_area = centered(body, w, h);
        textedit::render(f, box_area, draft.title(), &draft.editor);
    }

    if let Some(modal) = &app.api_key {
        apikey::render(f, body, modal);
    }
}

fn confirm_prompt(app: &App) -> Option<String> {
    app.confirm.as_ref().map(|c| match c {
        crate::app::Confirm::RemoveBook { title, .. } => format!("remove {title}?  y / n"),
        crate::app::Confirm::DeleteNote(n) => format!("delete “{}”?  y / n", n.title),
        crate::app::Confirm::DiscardDraft => "discard note?  y / n".to_string(),
        crate::app::Confirm::SearchOnline(q) => {
            format!("nothing in the library matching “{q}” — search online?  y / n")
        }
        crate::app::Confirm::OverwriteConversion { output, .. } => {
            format!("{} already exists — overwrite it?  y / n", output.display())
        }
    })
}

pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn landscape_puts_the_section_pane_on_the_left() {
        assert_eq!(
            book_layout(Rect::new(0, 0, 120, 40), 0),
            BookLayout::Split(PaneOrientation::BookRight)
        );
        assert_eq!(
            book_layout(Rect::new(0, 0, 60, 20), 0),
            BookLayout::Split(PaneOrientation::BookRight)
        );
    }

    #[test]
    fn portrait_defaults_to_book_top() {
        assert_eq!(
            book_layout(Rect::new(0, 0, 80, 50), 0),
            BookLayout::Split(PaneOrientation::BookTop)
        );
        assert_eq!(
            book_layout(Rect::new(0, 0, 30, 40), 0),
            BookLayout::Split(PaneOrientation::BookTop)
        );
    }

    #[test]
    fn rotation_advances_clockwise_from_the_aspect_default() {
        let a = Rect::new(0, 0, 120, 40);
        assert_eq!(effective_orientation(a, 0), PaneOrientation::BookRight);
        assert_eq!(effective_orientation(a, 1), PaneOrientation::BookBottom);
        assert_eq!(effective_orientation(a, 2), PaneOrientation::BookLeft);
        assert_eq!(effective_orientation(a, 3), PaneOrientation::BookTop);
        assert_eq!(effective_orientation(a, 4 % 4), PaneOrientation::BookRight);
    }

    #[test]
    fn small_panes_are_compact() {
        assert_eq!(book_layout(Rect::new(0, 0, 25, 30), 0), BookLayout::Compact);
        assert_eq!(book_layout(Rect::new(0, 0, 120, 6), 0), BookLayout::Compact);
    }

    #[test]
    fn split_caps_the_book_and_grows_the_panel() {
        assert_eq!(split_widths(200), (BOOK_MAX_COLS, 200 - BOOK_MAX_COLS));
        assert_eq!(split_widths(120), (BOOK_MAX_COLS, 120 - BOOK_MAX_COLS));
        assert_eq!(split_widths(26), (26 - PANEL_MIN, PANEL_MIN));
    }

    #[test]
    fn stacked_object_band_is_capped() {
        assert_eq!(stacked_object_height(30), 15);
        assert_eq!(stacked_object_height(120), BOOK_MAX_ROWS);
    }

    #[test]
    fn split_rects_place_panes_and_the_divider_rule() {
        let a = Rect::new(0, 0, 120, 40);
        let (book, panel, border) = split_rects(a, PaneOrientation::BookLeft, 0.0);
        assert_eq!(book.x, 0);
        assert_eq!(panel.x, book.width);
        assert_eq!(book.width + panel.width, 120);
        assert_eq!(border, Borders::LEFT);
        let (book, panel, border) = split_rects(a, PaneOrientation::BookRight, 0.0);
        assert_eq!(panel.x, 0);
        assert_eq!(book.x, panel.width);
        assert_eq!(border, Borders::RIGHT);
        let (book, panel, border) = split_rects(a, PaneOrientation::BookTop, 0.0);
        assert_eq!(book.y, 0);
        assert_eq!(panel.y, book.height);
        assert_eq!(border, Borders::TOP);
        let (book, panel, border) = split_rects(a, PaneOrientation::BookBottom, 0.0);
        assert_eq!(panel.y, 0);
        assert_eq!(book.y, panel.height);
        assert_eq!(border, Borders::BOTTOM);
    }

    #[test]
    fn divider_bias_slides_the_split_within_bounds() {
        let a = Rect::new(0, 0, 120, 40);
        let base = split_rects(a, PaneOrientation::BookLeft, 0.0).0.width;
        let grown = split_rects(a, PaneOrientation::BookLeft, 0.2).0.width;
        let shrunk = split_rects(a, PaneOrientation::BookLeft, -0.2).0.width;
        assert!(grown > base, "positive bias grows the object");
        assert!(shrunk < base, "negative bias shrinks it");
        let huge = split_rects(a, PaneOrientation::BookLeft, 5.0).0.width;
        assert!(huge <= 120 - MIN_PANE);
        let tiny = split_rects(a, PaneOrientation::BookLeft, -5.0).0.width;
        assert!(tiny >= MIN_PANE);
    }

    #[test]
    fn content_block_caps_and_centres() {
        let a = Rect::new(0, 0, 200, 40);
        let block = content_block(a, PaneOrientation::BookLeft);
        assert_eq!(block.width, BOOK_MAX_COLS + PANEL_MAX_COLS);
        assert_eq!(block.x, (200 - 98) / 2);
        assert_eq!(block.height, 40, "height is never capped");

        let (object, panel, _) = split_rects(block, PaneOrientation::BookLeft, 0.0);
        assert_eq!(object.width, BOOK_MAX_COLS);
        assert_eq!(panel.width, PANEL_MAX_COLS);
    }

    #[test]
    fn content_block_is_inert_on_a_narrow_pane() {
        for width in [26u16, 60, 80, 98] {
            let a = Rect::new(0, 0, width, 30);
            let block = content_block(a, PaneOrientation::BookLeft);
            assert_eq!(block, a, "the cap bit at {width} columns");
        }
    }

    #[test]
    fn a_stacked_layout_is_capped_by_line_length() {
        let a = Rect::new(0, 0, 200, 60);
        let block = content_block(a, PaneOrientation::BookTop);
        assert_eq!(block.width, STACK_MAX_COLS);
        assert_eq!(block.x, (200 - STACK_MAX_COLS) / 2);
        assert_eq!(block.height, 60);
    }

    /// The cap binds at the default divider only: a slid divider on a cramped pane may exceed
    /// `PANEL_MAX_COLS`.
    #[test]
    fn the_content_block_bounds_both_panes() {
        for width in (26..=400).step_by(7) {
            for o in PaneOrientation::ALL {
                let area = Rect::new(0, 0, width, 40);
                let block = content_block(area, o);
                assert!(
                    block.x >= area.x && block.x + block.width <= area.x + area.width,
                    "the block escaped the pane at {width}/{o:?}"
                );

                let (_, panel, _) = split_rects(block, o, 0.0);
                if o.is_vertical_divider() {
                    assert!(
                        panel.width <= PANEL_MAX_COLS,
                        "panel {} > cap at {width}/{o:?}",
                        panel.width
                    );
                }

                for bias in [-1.0f32, -0.3, 0.0, 0.3, 1.0] {
                    let (object, panel, _) = split_rects(block, o, bias);
                    for pane in [object, panel] {
                        assert!(pane.width > 0 && pane.height > 0);
                        assert!(
                            pane.x >= block.x && pane.x + pane.width <= block.x + block.width,
                            "a pane escaped the block at {width}/{o:?}/{bias}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_book_sits_at_the_centre_of_a_wide_pane() {
        let a = Rect::new(0, 0, 180, 44);
        let (object, panel, border) = book_rects(a, PaneOrientation::BookLeft, 0.0);
        assert_eq!((object.x, object.width), (65, BOOK_MAX_COLS));
        assert_eq!(panel.x, 115);
        assert_eq!(panel.width, PANEL_MAX_COLS);
        assert_eq!(border, Borders::LEFT);

        let (mirrored, panel, border) = book_rects(a, PaneOrientation::BookRight, 0.0);
        assert_eq!(
            mirrored, object,
            "the book jumped when the panel swapped side"
        );
        assert_eq!(panel.x + panel.width, object.x);
        assert_eq!(border, Borders::RIGHT);

        for width in (86..=400).step_by(7) {
            for o in [PaneOrientation::BookLeft, PaneOrientation::BookRight] {
                let a = Rect::new(0, 0, width, 40);
                let (object, _, _) = book_rects(a, o, 0.0);
                let off = (2 * object.x + object.width).abs_diff(width);
                assert!(off <= 1, "book off centre by {off} at {width}/{o:?}");
            }
        }
    }

    #[test]
    fn a_cramped_pane_slides_the_book_only_as_far_as_the_panel_allows() {
        let a = Rect::new(0, 0, 60, 30);
        for o in PaneOrientation::ALL {
            assert_eq!(
                book_rects(a, o, 0.0),
                split_rects(content_block(a, o), o, 0.0),
                "the cramped layout changed at {o:?}"
            );
        }

        let (object, panel, _) =
            book_rects(Rect::new(0, 0, 80, 30), PaneOrientation::BookLeft, 0.0);
        assert_eq!(object.x, 12);
        assert_eq!(panel.width, PANEL_MIN);
    }

    #[test]
    fn a_stacked_layout_keeps_its_half_and_half_spacing() {
        for width in [40u16, 98, 200] {
            for height in [12u16, 30, 50, 90] {
                for o in [PaneOrientation::BookTop, PaneOrientation::BookBottom] {
                    for bias in [-0.3f32, 0.0, 0.3] {
                        let a = Rect::new(0, 0, width, height);
                        assert_eq!(
                            book_rects(a, o, bias),
                            split_rects(content_block(a, o), o, bias),
                            "a stacked layout moved at {width}x{height}/{o:?}/{bias}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn centring_the_book_keeps_both_panes_alive() {
        for width in (26..=400).step_by(7) {
            for o in PaneOrientation::ALL {
                for bias in [-1.0f32, -0.3, 0.0, 0.3, 1.0] {
                    let area = Rect::new(0, 0, width, 40);
                    let (object, panel, _) = book_rects(area, o, bias);
                    let case = format!("{width}/{o:?}/{bias}");
                    for pane in [object, panel] {
                        assert!(pane.width > 0 && pane.height > 0, "empty pane at {case}");
                        assert!(
                            pane.x >= area.x && pane.x + pane.width <= area.x + area.width,
                            "a pane escaped the window at {case}"
                        );
                    }
                    if o.is_vertical_divider() {
                        assert!(
                            object.x + object.width <= panel.x || panel.x + panel.width <= object.x,
                            "the panes overlap at {case}"
                        );
                    } else {
                        assert!(
                            object.y + object.height <= panel.y
                                || panel.y + panel.height <= object.y,
                            "the panes overlap at {case}"
                        );
                    }
                    if o.is_vertical_divider() {
                        let afforded = PANEL_MIN.min(width - object.width);
                        assert!(
                            panel.width >= afforded,
                            "panel {} starved at {case}",
                            panel.width
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_list_box_shrink_wraps_its_contents() {
        let area = Rect::new(0, 0, 200, 50);
        let r = list_box(area, 40, 3);
        assert_eq!(r.width, 40 + LIST_CHROME);
        assert_eq!(r.height, LIST_MIN_ROWS, "3 rows is under the floor");
        assert_eq!(r.x, (200 - (40 + LIST_CHROME)) / 2);
        assert_eq!(r.y, (50 - LIST_MIN_ROWS) / 2);

        let r = list_box(area, 40, 20);
        assert_eq!(r.height, 22);
        assert_eq!(r.y, (50 - 22) / 2);
    }

    #[test]
    fn a_list_box_stays_between_its_floor_and_its_ceiling() {
        let area = Rect::new(0, 0, 200, 50);
        assert_eq!(list_box(area, 2, 0).width, LIST_MIN_COLS);
        assert_eq!(list_box(area, 400, 5).width, LIST_MAX_COLS);
        let r = list_box(area, 40, 500);
        assert_eq!(r.height, 50);
        assert_eq!(r.y, 0);
    }

    #[test]
    fn a_list_box_survives_a_pane_smaller_than_its_floor() {
        for (w, h) in [(1u16, 1u16), (4, 2), (20, 8), (30, 6)] {
            let area = Rect::new(0, 0, w, h);
            let r = list_box(area, 60, 40);
            assert!(r.width <= w && r.height <= h);
            assert!(r.x + r.width <= w && r.y + r.height <= h);
        }
    }

    #[test]
    fn centering_clamps_to_the_available_area() {
        let r = centered(Rect::new(0, 0, 10, 4), 40, 20);
        assert_eq!(r, Rect::new(0, 0, 10, 4));
        let r = centered(Rect::new(0, 0, 20, 10), 10, 4);
        assert_eq!(r, Rect::new(5, 3, 10, 4));
    }

    #[test]
    fn a_candidate_hint_is_bounded_and_keeps_its_move() {
        for len in [0usize, 1, 5, 30, 60, 200] {
            let title = "x".repeat(len);
            for hint in ["l to link, n if not", "no Goodreads id, so n if not"] {
                let line = candidate_hint(&title, 0.715, hint);
                assert!(
                    line.chars().count() <= DETAIL_MAX,
                    "len {len} / {hint:?} overflowed: {} chars",
                    line.chars().count()
                );
                assert!(line.ends_with(hint), "the move was clipped: {line}");
                assert!(line.contains("(72%)"), "the score went missing: {line}");
            }
        }
    }

    #[test]
    fn clip_counts_its_own_ellipsis() {
        assert_eq!(clip("abcdef".into(), 6), "abcdef");
        let clipped = clip("abcdefgh".into(), 6);
        assert_eq!(clipped.chars().count(), 6);
        assert!(clipped.ends_with('…'));
    }
}
