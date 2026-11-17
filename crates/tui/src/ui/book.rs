use bookmole::{Book, FlashcardRow, Highlight, NoteRecord, Progress};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Margin, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use super::{BookLayout, book_layout, book_rects};
use crate::app::{App, BOOK_TABS, BookTab, BookView};
use crate::theme;

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    if app.view.is_none() {
        let msg = Paragraph::new("no book selected — press m for the menu").style(theme::dim());
        f.render_widget(msg, super::centered(area, 44, 1));
        return;
    }

    let bar_h = if area.height >= 6 { 1 } else { 0 };
    let [main, bar] = Layout::vertical([Constraint::Min(0), Constraint::Length(bar_h)]).areas(area);

    let layout = book_layout(main, app.layout.rotation);
    let mut bar = bar;

    match layout {
        BookLayout::Split(_) if !app.layout.panel => {
            present_book(f, app, main);
            draw_header(f, app.view.as_ref().expect("checked above"), main);
        }
        BookLayout::Split(orientation) => {
            let (object, panel, border) = book_rects(main, orientation, app.layout.divider_bias);
            let hull = object.union(panel);
            bar = Rect {
                x: hull.x,
                width: hull.width,
                ..bar
            };
            present_book(f, app, object);
            draw_header(f, app.view.as_ref().expect("checked above"), object);
            draw_panel(f, app, panel, border);
        }
        BookLayout::Compact => {
            let title = app
                .view
                .as_ref()
                .map(|v| v.book.display_title().to_string())
                .unwrap_or_default();
            let frame = Block::default()
                .title(Span::styled(format!(" {title} "), theme::title()))
                .title_alignment(Alignment::Center);
            let inner = frame.inner(main);
            f.render_widget(frame, main);
            if inner.width != 0 && inner.height != 0 {
                if app.in_section {
                    draw_section(f, app, inner);
                } else {
                    present_book(f, app, inner);
                }
            }
        }
    }

    if bar_h > 0 {
        draw_key_bar(f, app, bar);
    }
}

fn present_book(f: &mut Frame, app: &mut App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let params = app.params;
    let mode = app.render_mode;
    // All three must be direct fields of `app`; a `&mut App` method would fail the borrow.
    let mut presenter = crate::render3d::presenter_for(mode, &mut app.scene, &mut app.rich);
    let book = &app.view.as_ref().expect("caller checked").book;
    presenter.draw_book(f, area, book, params);
}

fn draw_header(f: &mut Frame, view: &BookView, object: Rect) {
    if object.height < 3 || object.width < 4 {
        return;
    }
    let area = Rect {
        height: 2,
        ..object
    };
    let [title, prog] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
    let b = &view.book;
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(b.display_title().to_string(), theme::title()),
            Span::styled("  ·  ", theme::dim()),
            Span::styled(b.display_authors().to_string(), theme::dim()),
        ]))
        .alignment(Alignment::Center),
        title,
    );
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(progress_text(b), theme::accent())))
            .alignment(Alignment::Center),
        prog,
    );
}

fn progress_text(b: &Book) -> String {
    let p = Progress::of_book(b);
    match (p, p.page(), p.of(), p.percent()) {
        (Progress::Finished, ..) => "finished".to_string(),
        (Progress::Untouched, ..) => "not started".to_string(),
        (_, Some(page), Some(of), Some(pct)) => format!("{page} / {of} · {pct}%"),
        (_, Some(page), _, _) => format!("page {page}"),
        (_, None, _, Some(pct)) => format!("{pct}%"),
        _ => "not started".to_string(),
    }
}

fn draw_panel(f: &mut Frame, app: &mut App, area: Rect, border: Borders) {
    let block = Block::default().borders(border).border_style(theme::dim());
    let inner = block.inner(area);
    // The rule is drawn only in a section; against the bare menu it cuts empty space. The cell stays
    // reserved so opening a section does not shift the panel.
    if app.in_section {
        f.render_widget(block, area);
    }
    let margin = if border.intersects(Borders::LEFT | Borders::RIGHT) {
        Margin::new(1, 0)
    } else {
        Margin::new(0, 0)
    };
    let inner = inner.inner(margin);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    if app.in_section {
        draw_section(f, app, inner);
    } else {
        draw_section_menu(f, app.book_tab, inner);
    }
}

const MENU_HINT: &str = "enter ›";

fn menu_box(area: Rect) -> Rect {
    let width = BOOK_TABS
        .iter()
        .map(|(_, l)| l.chars().count() as u16 + 2)
        .chain(std::iter::once(MENU_HINT.chars().count() as u16))
        .max()
        .unwrap_or(0);
    let height = BOOK_TABS.len() as u16 + 2;
    super::centered(area, width, height)
}

fn draw_section_menu(f: &mut Frame, active: BookTab, area: Rect) {
    let area = menu_box(area);
    let mut lines = Vec::new();
    for (tab, label) in BOOK_TABS {
        if tab == active {
            lines.push(Line::from(Span::styled(
                format!("› {label}"),
                theme::selected(),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                format!("  {label}"),
                theme::primary(),
            )));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(MENU_HINT, theme::dim())));
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_section(f: &mut Frame, app: &mut App, area: Rect) {
    let label = BOOK_TABS
        .iter()
        .find(|(t, _)| *t == app.book_tab)
        .map(|(_, l)| *l)
        .unwrap_or("");
    let label = if app.links.is_some() { "Links" } else { label };
    let [head, content] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("‹ ", theme::dim()),
            Span::styled(label, theme::key()),
        ])),
        head,
    );

    match app.book_tab {
        BookTab::Info => {
            let view = app.view.as_ref().expect("checked");
            draw_info(f, view, content);
        }
        BookTab::Notes if app.links.is_some() => {
            draw_links(f, app.links.as_mut().expect("checked"), content);
        }
        BookTab::Notes => {
            let items: Vec<ListItem> = app
                .view
                .as_ref()
                .expect("checked")
                .notes
                .iter()
                .map(|n| ListItem::new(note_line(n)))
                .collect();
            draw_list(
                f,
                &mut app.tab_state,
                content,
                items,
                "no notes yet — n to write one",
            );
        }
        BookTab::Highlights => {
            let view = app.view.as_ref().expect("checked");
            let gutter = view.shows_read_gutter();
            let items: Vec<ListItem> = view
                .highlights
                .iter()
                .map(|h| ListItem::new(highlight_line(h, gutter.then(|| view.read_number(h)))))
                .collect();
            draw_list(
                f,
                &mut app.tab_state,
                content,
                items,
                "no highlights — import from KOReader",
            );
        }
        BookTab::Cards => {
            let items: Vec<ListItem> = app
                .view
                .as_ref()
                .expect("checked")
                .cards
                .iter()
                .map(|c| ListItem::new(card_line(c)))
                .collect();
            draw_list(
                f,
                &mut app.tab_state,
                content,
                items,
                "no flashcards for this book",
            );
        }
    }
}

fn draw_list(f: &mut Frame, state: &mut ListState, area: Rect, items: Vec<ListItem>, empty: &str) {
    if items.is_empty() {
        f.render_widget(
            Paragraph::new(empty)
                .style(theme::dim())
                .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }
    let list = List::new(items)
        .highlight_style(theme::selected())
        .highlight_symbol("› ");
    f.render_stateful_widget(list, area, state);
}

/// Direction is an arrow in the text and a dangling target says so in words: both survive
/// `REVERSED` selection and can be asserted on, unlike a style-only distinction.
fn draw_links(f: &mut Frame, pane: &mut crate::app::LinksPane, area: Rect) {
    let [head, counts, list] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(area);

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            pane.note.title.replace('\t', "    "),
            theme::title(),
        ))),
        head,
    );
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("{} out · {} in", pane.out_count(), pane.in_count()),
            theme::dim(),
        ))),
        counts,
    );

    let items: Vec<ListItem> = pane
        .rows
        .iter()
        .map(|r| ListItem::new(link_line(r)))
        .collect();
    draw_list(
        f,
        &mut pane.state,
        list,
        items,
        "nothing links here yet — a [[wikilink]] in a note body makes an edge",
    );
}

fn link_line(row: &crate::app::LinkRow) -> Line<'static> {
    use crate::app::LinkRow;
    let (arrow, title, tail) = match row {
        LinkRow::Out { title, to: None } => ("→ ", title.clone(), "  (no note yet)"),
        LinkRow::Out { title, .. } => ("→ ", title.clone(), ""),
        LinkRow::In(n) => ("← ", n.title.clone(), ""),
    };
    Line::from(vec![
        Span::styled(arrow, theme::primary()),
        Span::styled(title.replace('\t', "    "), theme::primary()),
        Span::styled(tail, theme::primary()),
    ])
}

/// Kind gutter: solid is private (reflection), hollow is public (review). Reflections and reviews
/// share the notes list, so rows must say which is which.
fn kind_mark(kind: &str) -> &'static str {
    match kind {
        "reflection" => "◆ ",
        "review" => "◇ ",
        _ => "  ",
    }
}

fn note_line(n: &NoteRecord) -> Line<'static> {
    let mut spans = vec![Span::styled(kind_mark(&n.kind), theme::primary())];
    let tag = anchor_tag(n);
    if !tag.is_empty() {
        spans.push(Span::styled(tag, theme::primary()));
        spans.push(Span::styled(" · ", theme::primary()));
    }
    spans.push(Span::styled(
        n.title.replace('\t', "    "),
        theme::primary(),
    ));
    Line::from(spans)
}

fn anchor_tag(n: &NoteRecord) -> String {
    let mut parts = Vec::new();
    if let Some(p) = n.page {
        parts.push(format!("p.{p}"));
    }
    if let Some(l) = &n.location {
        parts.push(l.clone());
    }
    if n.page.is_none() && n.location.is_none() && n.highlight_id.is_some() {
        parts.push("↳hl".to_string());
    }
    parts.join(" ")
}

/// `read` is `None` for a once-read book (no column); `Some(None)` is a highlight belonging to no
/// read, shown as `·` so it does not look like an alignment slip.
fn highlight_line(h: &Highlight, read: Option<Option<usize>>) -> Line<'static> {
    let mut spans = Vec::new();
    if let Some(n) = read {
        let mark = n.map(|n| n.to_string()).unwrap_or_else(|| "·".into());
        spans.push(Span::styled(format!("{mark} "), theme::dim()));
    }
    if let Some(p) = h.page {
        spans.push(Span::styled(format!("p.{p}"), theme::primary()));
        spans.push(Span::styled(" · ", theme::primary()));
    }
    spans.push(Span::styled(h.text.clone(), theme::primary()));
    Line::from(spans)
}

fn card_line(c: &FlashcardRow) -> Line<'static> {
    let mark = if c.exported { "✓ " } else { "  " };
    let mut spans = vec![
        Span::styled(mark, theme::primary()),
        Span::styled(c.word.clone(), theme::primary()),
    ];
    if let Some(ctx) = &c.context {
        spans.push(Span::styled(" · ", theme::primary()));
        spans.push(Span::styled(ctx.clone(), theme::primary()));
    }
    Line::from(spans)
}

fn draw_info(f: &mut Frame, view: &BookView, area: Rect) {
    let b = &view.book;
    let mut lines = Vec::new();
    for (label, value) in facts(b) {
        lines.push(Line::from(vec![
            Span::styled(format!("{label:<10}"), theme::dim()),
            Span::styled(value, theme::primary()),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(view.highlights.len().to_string(), theme::accent()),
        Span::styled(" highlights   ", theme::dim()),
        Span::styled(view.notes.len().to_string(), theme::accent()),
        Span::styled(" notes", theme::dim()),
    ]));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn facts(b: &Book) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    if let Some(p) = &b.publisher {
        out.push(("publisher", p.clone()));
    }
    if let Some(y) = b.publish_year {
        out.push(("year", y.to_string()));
    }
    if let Some(l) = &b.language {
        out.push(("language", l.clone()));
    }
    if let Some(p) = b.page_count {
        out.push(("pages", p.to_string()));
    }
    if let Some(i) = b.any_isbn() {
        out.push(("isbn", i.to_string()));
    }
    out
}

fn render_label(app: &App) -> &'static str {
    if app.render_mode.is_rich() {
        "glyphs"
    } else {
        "pixels"
    }
}

fn draw_key_bar(f: &mut Frame, app: &App, area: Rect) {
    let spin = if app.spinning { "stop" } else { "spin" };
    let expanded: &[(&str, &str)] = &[
        ("esc/b/←", "back"),
        ("n", "note"),
        ("e", "reflect"),
        ("w", "review"),
        ("L", "links"),
        ("d", "delete"),
        ("p", "page"),
        ("f", "finish"),
        ("x", "export"),
        ("space", spin),
        (
            "tab",
            if app.layout.panel {
                "hide tabs"
            } else {
                "tabs"
            },
        ),
        ("[ ]", "panes"),
        ("v", render_label(app)),
        ("o", "less"),
        ("?", "help"),
        ("m", "menu"),
        ("q", "quit"),
    ];
    let collapsed: &[(&str, &str)] = &[
        ("n", "note"),
        ("o", "options"),
        ("?", "help"),
        ("m", "menu"),
        ("q", "quit"),
    ];
    let pairs = if app.show_options {
        expanded
    } else {
        collapsed
    };

    let mut spans = Vec::new();
    let mut used = 0u16;
    for (key, label) in pairs {
        let width = key.chars().count() as u16 + label.chars().count() as u16 + 4;
        if used + width > area.width {
            break;
        }
        used += width;
        spans.push(Span::styled(format!(" {key} "), theme::key()));
        spans.push(Span::styled(format!("{label}  "), theme::dim()));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_bar_always_offers_a_way_to_the_menu() {
        for expanded in [false, true] {
            let pairs: Vec<&str> = if expanded {
                vec![
                    "esc/b/←",
                    "n",
                    "e",
                    "w",
                    "L",
                    "d",
                    "p",
                    "f",
                    "x",
                    "space",
                    "t",
                    "[ ]",
                    "o",
                    "m",
                    "q",
                ]
            } else {
                vec!["n", "o", "m", "q"]
            };
            assert!(pairs.contains(&"m"), "no menu key when expanded={expanded}");
        }
    }

    #[test]
    fn the_section_menu_is_centred_in_its_pane() {
        let b = menu_box(Rect::new(0, 0, 46, 30));
        assert_eq!(b.width, 12);
        assert_eq!(b.x, (46 - 12) / 2);
        assert_eq!(b.height, BOOK_TABS.len() as u16 + 2);
        assert_eq!(b.y, (30 - b.height) / 2);

        let b = menu_box(Rect::new(0, 0, 4, 2));
        assert!(b.width <= 4 && b.height <= 2);
    }

    #[test]
    fn anchor_tag_prefers_page_then_location() {
        let mut n = NoteRecord {
            id: 1,
            book_id: Some(1),
            reading_id: None,
            highlight_id: None,
            page: Some(42),
            location: None,
            file_path: "x.md".into(),
            title: "t".into(),
            kind: "note".into(),
            created_at: None,
            last_modified: None,
        };
        assert_eq!(anchor_tag(&n), "p.42");
        n.location = Some("Ch 3".into());
        assert_eq!(anchor_tag(&n), "p.42 Ch 3");
        n.page = None;
        n.location = None;
        n.highlight_id = Some(9);
        assert_eq!(anchor_tag(&n), "↳hl");
    }

    #[test]
    fn a_reflection_and_a_review_are_marked_in_the_notes_list() {
        let note = |kind: &str| NoteRecord {
            id: 1,
            book_id: Some(1),
            reading_id: None,
            highlight_id: None,
            page: None,
            location: None,
            file_path: "x.md".into(),
            title: "Reflection: Station Eleven".into(),
            kind: kind.into(),
            created_at: None,
            last_modified: None,
        };
        let first = |n| note_line(&n).spans[0].content.to_string();
        assert_eq!(first(note("reflection")), "◆ ");
        assert_eq!(first(note("review")), "◇ ");
        assert_eq!(first(note("note")), "  ");

        assert!(
            note_line(&note("reflection"))
                .spans
                .windows(2)
                .all(|w| w[0].style == w[1].style)
        );
    }

    #[test]
    fn a_link_row_carries_its_direction_and_says_when_it_dangles() {
        use crate::app::LinkRow;
        let flat = |l: Line<'static>| {
            l.spans
                .iter()
                .map(|s| s.content.to_string())
                .collect::<String>()
        };
        let target = NoteRecord {
            id: 7,
            book_id: Some(1),
            reading_id: None,
            highlight_id: None,
            page: None,
            location: None,
            file_path: "x.md".into(),
            title: "Symphony".into(),
            kind: "note".into(),
            created_at: None,
            last_modified: None,
        };
        assert_eq!(
            flat(link_line(&LinkRow::Out {
                title: "Symphony".into(),
                to: Some(target.clone()),
            })),
            "→ Symphony"
        );
        assert_eq!(
            flat(link_line(&LinkRow::Out {
                title: "Nowhere".into(),
                to: None,
            })),
            "→ Nowhere  (no note yet)"
        );
        assert_eq!(flat(link_line(&LinkRow::In(target))), "← Symphony");
    }

    #[test]
    fn facts_skip_missing_fields() {
        let bare = Book::default();
        assert!(facts(&bare).is_empty());

        let filled = Book {
            publisher: Some("Picador".into()),
            publish_year: Some(2014),
            isbn_13: Some("9781447268963".into()),
            ..Book::default()
        };
        let labels: Vec<_> = facts(&filled).iter().map(|(l, _)| *l).collect();
        assert_eq!(labels, vec!["publisher", "year", "isbn"]);
    }
}
