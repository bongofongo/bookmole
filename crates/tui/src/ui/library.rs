use bookmole::{Book, Progress, names, sort};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Padding};

use crate::app::App;
use crate::theme;

/// Applied in the frontend, not by `ORDER BY`: `refresh_library` fetches a fixed recency page and
/// orders it in Rust, so changing the sort reorders the same books rather than swapping which show.
/// Variant order is the `s` cycle order, starting at the default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Sort {
    #[default]
    Recent,
    /// Leading articles are not stripped: a row reading "The Overstory" between S and T entries looks
    /// like a bug, whatever cataloguing says.
    Title,
    Author,
    Year,
}

impl Sort {
    const ALL: [Sort; 4] = [Sort::Recent, Sort::Title, Sort::Author, Sort::Year];

    pub fn label(self) -> &'static str {
        match self {
            Sort::Recent => "recent",
            Sort::Title => "title",
            Sort::Author => "author",
            Sort::Year => "year",
        }
    }

    pub fn from_label(s: &str) -> Option<Sort> {
        Sort::ALL.into_iter().find(|o| o.label() == s)
    }

    pub fn next(self) -> Sort {
        let i = Sort::ALL.iter().position(|o| *o == self).unwrap_or(0);
        Sort::ALL[(i + 1) % Sort::ALL.len()]
    }

    /// `Recent` is a no-op, since the engine already returns that order. The other sorts are stable,
    /// so ties keep recency order underneath.
    pub fn apply(self, books: &mut [Book]) {
        match self {
            Sort::Recent => {}
            // `sort::title_key`, not `display_title`: the engine files under `sort_title` and this pane must
            // not disagree; likewise `names::sort_key` for authors.
            Sort::Title => books.sort_by_key(|b| sort::title_key(b).to_lowercase()),
            Sort::Author => books.sort_by_key(|b| names::sort_key(&b.authors)),
            Sort::Year => books.sort_by_key(|b| match b.publish_year {
                Some(y) => (0, std::cmp::Reverse(y)),
                None => (1, std::cmp::Reverse(0)),
            }),
        }
    }
}

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let selected = app.library_state.selected();
    let rows: Vec<Line> = app
        .library
        .iter()
        .enumerate()
        .map(|(i, b)| row(b, Some(i) == selected))
        .collect();
    // A narrowed list says so, so a short list does not look like lost books; the sort is always
    // named, including the default, so a rearranged list never goes unexplained.
    let title = match &app.library_filter {
        Some(q) => format!(
            " library · “{q}” · by {} · {} ",
            app.library_sort.label(),
            app.library.len()
        ),
        None => format!(
            " library · by {} · {} ",
            app.library_sort.label(),
            app.library.len()
        ),
    };

    let keys = key_bar(app.library_filter.is_some());
    let widest = rows.iter().map(|l| l.width() as u16).max().unwrap_or(0);
    let area = super::list_box(
        area,
        widest
            .max(title.chars().count() as u16)
            .max(keys.width() as u16),
        rows.len() as u16,
    );
    f.render_widget(ratatui::widgets::Clear, area);

    let items: Vec<ListItem> = rows.into_iter().map(ListItem::new).collect();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::dim())
        .padding(Padding::horizontal(1))
        .title(Span::styled(title, theme::accent()))
        .title_bottom(keys.centered());
    let list = List::new(items).block(block).highlight_symbol("› ");
    f.render_stateful_widget(list, area, &mut app.library_state);
}

fn row(b: &Book, selected: bool) -> Line<'static> {
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
    if let Some(year) = b.publish_year {
        spans.push(Span::styled(format!("  {year}"), theme::dim()));
    }
    let tag = progress_tag(b);
    if !tag.is_empty() {
        spans.push(Span::styled(format!("  {tag}"), theme::accent()));
    }
    Line::from(spans)
}

fn key_bar(filtered: bool) -> Line<'static> {
    Line::from(vec![
        Span::styled(" enter", theme::key()),
        Span::styled(" open  ", theme::dim()),
        Span::styled("/", theme::key()),
        Span::styled(" find  ", theme::dim()),
        Span::styled("s", theme::key()),
        Span::styled(" sort  ", theme::dim()),
        Span::styled("d", theme::key()),
        Span::styled(" remove  ", theme::dim()),
        Span::styled("esc", theme::key()),
        Span::styled(if filtered { " all " } else { " back " }, theme::dim()),
    ])
}

pub fn progress_tag(b: &Book) -> String {
    tag(Progress::of_book(b))
}

pub fn tag(p: Progress) -> String {
    match p {
        Progress::Finished => "done".to_string(),
        Progress::Untouched => String::new(),
        p => match (p.percent(), p.page()) {
            (Some(pct), _) => format!("{pct}%"),
            (None, Some(page)) => format!("p.{page}"),
            // Kept rather than `unreachable!`: a panic in a draw call wrecks the user's pane.
            (None, None) => String::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book(current: Option<i64>, total: Option<i64>, finished: bool) -> Book {
        Book {
            current_page: current,
            page_count: total,
            finished,
            ..Book::default()
        }
    }

    #[test]
    fn progress_tag_covers_every_state() {
        assert_eq!(progress_tag(&book(Some(50), Some(200), false)), "25%");
        assert_eq!(progress_tag(&book(Some(50), None, false)), "p.50");
        assert_eq!(progress_tag(&book(None, Some(200), false)), "");
        assert_eq!(progress_tag(&book(Some(10), Some(10), true)), "done");
    }

    #[test]
    fn progress_tag_survives_a_zero_page_count() {
        assert_eq!(progress_tag(&book(Some(50), Some(0), false)), "p.50");
    }

    #[test]
    fn the_device_percentage_reaches_this_list_too() {
        let mut b = book(None, None, false);
        b.ko_percent = Some(0.43);
        assert_eq!(progress_tag(&b), "43%");
    }

    fn titled(title: &str, author: &str, year: Option<i64>) -> Book {
        Book {
            title: Some(title.into()),
            authors: if author.is_empty() {
                Vec::new()
            } else {
                vec![author.into()]
            },
            publish_year: year,
            ..Book::default()
        }
    }

    fn titles(books: &[Book]) -> Vec<&str> {
        books.iter().map(|b| b.display_title()).collect()
    }

    #[test]
    fn every_order_orders_what_it_says_it_does() {
        let original = vec![
            titled("Station Eleven", "Emily St. John Mandel", Some(2014)),
            titled("A Wizard of Earthsea", "Ursula K. Le Guin", Some(1968)),
            titled("Middlemarch", "George Eliot", Some(1871)),
            titled("The Overstory", "Richard Powers", Some(2018)),
        ];

        let mut books = original.clone();
        Sort::Recent.apply(&mut books);
        assert_eq!(titles(&books), titles(&original));

        let mut books = original.clone();
        Sort::Title.apply(&mut books);
        assert_eq!(
            titles(&books),
            vec![
                "A Wizard of Earthsea",
                "Middlemarch",
                "Station Eleven",
                "The Overstory"
            ]
        );

        let mut books = original.clone();
        Sort::Author.apply(&mut books);
        assert_eq!(
            titles(&books),
            vec![
                "Middlemarch",
                "A Wizard of Earthsea",
                "Station Eleven",
                "The Overstory"
            ]
        );

        let mut books = original.clone();
        Sort::Year.apply(&mut books);
        assert_eq!(
            titles(&books),
            vec![
                "The Overstory",
                "Station Eleven",
                "A Wizard of Earthsea",
                "Middlemarch"
            ]
        );
    }

    #[test]
    fn a_missing_author_or_year_sinks_rather_than_floats() {
        let mut books = vec![
            titled("No One", "", None),
            titled("Middlemarch", "George Eliot", Some(2017)),
        ];
        Sort::Author.apply(&mut books);
        assert_eq!(titles(&books), vec!["Middlemarch", "No One"]);

        let mut books = vec![
            titled("No One", "", None),
            titled("Middlemarch", "George Eliot", Some(2017)),
        ];
        Sort::Year.apply(&mut books);
        assert_eq!(titles(&books), vec!["Middlemarch", "No One"]);
    }

    #[test]
    fn a_tie_keeps_the_order_it_arrived_in() {
        let mut books = vec![
            titled("Second", "George Eliot", Some(2017)),
            titled("First", "George Eliot", Some(2017)),
        ];
        Sort::Author.apply(&mut books);
        assert_eq!(titles(&books), vec!["Second", "First"]);
        Sort::Year.apply(&mut books);
        assert_eq!(titles(&books), vec!["Second", "First"]);
    }

    #[test]
    fn the_cycle_is_closed_and_every_label_round_trips() {
        let mut seen = Vec::new();
        let mut s = Sort::default();
        for _ in 0..Sort::ALL.len() {
            seen.push(s);
            assert_eq!(Sort::from_label(s.label()), Some(s));
            s = s.next();
        }
        assert_eq!(s, Sort::default(), "the cycle does not close");
        assert_eq!(seen, Sort::ALL.to_vec());
        assert_eq!(Sort::from_label("by-author"), None);
    }
}
