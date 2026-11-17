//! The home screen: one row per open reading, most-recently-touched first, as a shelf.
//! Shows no count of anything: a tally of what you have not done is task-completion framing.

use bookmole::{Book, Progress, Reading};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Padding, Paragraph};

use crate::app::App;
use crate::theme;

const EMPTY: &str = "nothing open right now — / to find a book, m for the menu";

const TITLE: &str = " reading ";

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let selected = app.reading_state.selected();
    let rows: Vec<Line> = app
        .reading
        .iter()
        .enumerate()
        .map(|(i, (b, r))| row(b, r, Some(i) == selected))
        .collect();
    let keys = key_bar(app.reading.is_empty());

    let widest = rows
        .iter()
        .map(|l| l.width() as u16)
        .max()
        .unwrap_or(EMPTY.chars().count() as u16)
        .max(TITLE.chars().count() as u16)
        .max(keys.width() as u16);
    let area = super::list_box(area, widest, rows.len() as u16);
    f.render_widget(ratatui::widgets::Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::dim())
        .padding(Padding::horizontal(1))
        .title(Span::styled(TITLE, theme::accent()))
        .title_bottom(keys.centered());

    if rows.is_empty() {
        let inner = block.inner(area);
        f.render_widget(block, area);
        if inner.width == 0 || inner.height == 0 {
            return;
        }
        f.render_widget(Paragraph::new(EMPTY).style(theme::dim()), inner);
        return;
    }

    let items: Vec<ListItem> = rows.into_iter().map(ListItem::new).collect();
    let list = List::new(items).block(block).highlight_symbol("› ");
    f.render_stateful_widget(list, area, &mut app.reading_state);
}

fn row(b: &Book, r: &Reading, selected: bool) -> Line<'static> {
    let title_style = if selected {
        theme::title().patch(theme::selected())
    } else {
        theme::title()
    };
    let mut spans = vec![Span::styled(b.display_title().to_string(), title_style)];
    spans.push(Span::styled(
        format!("  {}", b.display_authors()),
        theme::dim(),
    ));
    let tag = super::library::tag(Progress::of_reading(r, b.page_count));
    if !tag.is_empty() {
        spans.push(Span::styled(format!("  {tag}"), theme::accent()));
    }
    Line::from(spans)
}

fn key_bar(empty: bool) -> Line<'static> {
    let mut spans = Vec::new();
    if !empty {
        spans.extend([
            Span::styled(" enter", theme::key()),
            Span::styled(" open  ", theme::dim()),
            Span::styled("e", theme::key()),
            Span::styled(" reflect  ", theme::dim()),
            Span::styled("w", theme::key()),
            Span::styled(" review  ", theme::dim()),
        ]);
    }
    spans.extend([
        Span::styled(if empty { " /" } else { "/" }, theme::key()),
        Span::styled(" find  ", theme::dim()),
        Span::styled("m", theme::key()),
        Span::styled(" menu ", theme::dim()),
    ]);
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Modifier;

    fn reading(page: Option<i64>, ko_percent: Option<f64>) -> Reading {
        Reading {
            id: 1,
            book_id: 1,
            started_at: None,
            finished_at: None,
            status: "reading".into(),
            source: "manual".into(),
            current_page: page,
            ko_status: None,
            ko_percent,
            ko_rating: None,
            created_at: 0,
            last_modified: 0,
            put_down_at: None,
        }
    }

    fn book(current: Option<i64>, total: Option<i64>) -> Book {
        Book {
            title: Some("Station Eleven".into()),
            current_page: current,
            page_count: total,
            ..Book::default()
        }
    }

    #[test]
    fn a_page_beats_the_devices_percentage() {
        assert_eq!(
            super::super::library::tag(Progress::of_reading(
                &reading(Some(50), Some(0.9)),
                Some(200)
            )),
            "25%"
        );
    }

    #[test]
    fn the_devices_percentage_fills_in_for_a_missing_page() {
        assert_eq!(
            super::super::library::tag(Progress::of_reading(&reading(None, Some(0.42)), Some(200))),
            "42%"
        );
        assert_eq!(
            super::super::library::tag(Progress::of_reading(&reading(None, None), None)),
            ""
        );
    }

    #[test]
    fn selection_reverses_the_title_only() {
        let line = row(&book(Some(50), Some(200)), &reading(Some(50), None), true);
        let reversed: Vec<bool> = line
            .spans
            .iter()
            .map(|s| s.style.add_modifier.contains(Modifier::REVERSED))
            .collect();
        assert_eq!(reversed, vec![true, false, false]);
    }

    #[test]
    fn nothing_on_the_screen_counts_anything() {
        assert!(!TITLE.chars().any(|c| c.is_ascii_digit()));
        assert!(!EMPTY.chars().any(|c| c.is_ascii_digit()));
        assert!(EMPTY.contains('/') && EMPTY.contains('m'));
    }
}
