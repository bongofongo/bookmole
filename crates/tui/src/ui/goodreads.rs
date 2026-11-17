//! A Goodreads CSV previewed before it is applied, and where the library is exported from.
//! Not a shelf: the file is matched as a whole; undecided rows sort last and `s` applies all.

use bookmole::{GoodreadsMatch, TextOutcome};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem};

use crate::app::{App, GoodreadsPreview, GoodreadsPreviewRow};
use crate::theme;

const HINT: &str = "/ to read a Goodreads export · x to write one from your library";

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let (title, rows) = match &app.goodreads {
        None => (" goodreads ".to_string(), Vec::new()),
        Some(p) => {
            let selected = p.state.selected();
            let rows: Vec<Line> = p
                .rows
                .iter()
                .enumerate()
                .map(|(i, r)| row(r, Some(i) == selected))
                .collect();
            (title_for(p), rows)
        }
    };

    let Some((area, block)) = super::shelf_frame(f, area, title, key_bar(), &rows, HINT) else {
        return;
    };

    let items: Vec<ListItem> = rows.into_iter().map(ListItem::new).collect();
    let list = List::new(items).block(block).highlight_symbol("› ");
    if let Some(p) = &mut app.goodreads {
        f.render_stateful_widget(list, area, &mut p.state);
    }

    if let Some(picker) = &mut app.link_picker {
        super::link_picker(f, picker, area);
    }
}

fn title_for(p: &GoodreadsPreview) -> String {
    let name = p
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.path.display().to_string());
    if p.dry_run {
        format!(" goodreads · {name} · preview ")
    } else {
        format!(" goodreads · {name} · brought across ")
    }
}

fn row(r: &GoodreadsPreviewRow, selected: bool) -> Line<'static> {
    let title_style = if selected {
        theme::title().patch(theme::selected())
    } else {
        theme::title()
    };
    match r {
        GoodreadsPreviewRow::Book {
            title, matched_by, ..
        } => {
            let mut spans = vec![
                Span::styled("  ", theme::dim()),
                Span::styled(
                    format!("{:<12}", match_label(*matched_by)),
                    match_style(*matched_by),
                ),
                Span::styled(title.clone(), title_style),
            ];
            let detail = super::clip(book_detail(r), super::DETAIL_MAX);
            if !detail.is_empty() {
                spans.push(Span::styled(format!("  {detail}"), theme::dim()));
            }
            Line::from(spans)
        }
        GoodreadsPreviewRow::Unmatched {
            row,
            title,
            authors,
            candidates,
            external_id,
        } => {
            let mut spans = vec![
                Span::styled("  ", theme::dim()),
                Span::styled(format!("{:<12}", "maybe"), theme::accent()),
                Span::styled(title.clone(), title_style),
            ];
            if !authors.is_empty() {
                spans.push(Span::styled(
                    format!("  {}", authors.join(", ")),
                    theme::dim(),
                ));
            }
            spans.push(Span::styled(format!("  row {row}"), theme::dim()));
            let detail = match (candidates.first(), external_id) {
                (Some(c), Some(_)) => {
                    super::candidate_hint(&c.title, c.score, "l to link, n if not")
                }
                (Some(c), None) => {
                    super::candidate_hint(&c.title, c.score, "no Goodreads id, so n if not")
                }
                (None, _) => "nothing here looks like it — n brings it in".to_string(),
            };
            spans.push(Span::styled(
                format!("  {}", super::clip(detail, super::DETAIL_MAX)),
                theme::dim(),
            ));
            Line::from(spans)
        }
    }
}

fn book_detail(r: &GoodreadsPreviewRow) -> String {
    let GoodreadsPreviewRow::Book {
        readings,
        shelves,
        rating,
        review,
        private_notes,
        ..
    } = r
    else {
        return String::new();
    };
    let mut parts = Vec::new();
    if *readings > 0 {
        parts.push(match readings {
            1 => "1 reading".to_string(),
            n => format!("{n} readings"),
        });
    }
    if *shelves > 0 {
        parts.push(match shelves {
            1 => "1 shelf".to_string(),
            n => format!("{n} shelves"),
        });
    }
    if let Some(r) = rating {
        parts.push(format!("rated {r}"));
    }
    match review {
        TextOutcome::Written => parts.push("review".to_string()),
        TextOutcome::KeptOurs => parts.push("review kept as yours".to_string()),
        _ => {}
    }
    match private_notes {
        TextOutcome::Written => parts.push("private notes".to_string()),
        TextOutcome::KeptOurs => parts.push("notes kept as yours".to_string()),
        _ => {}
    }
    if parts.is_empty() {
        parts.push("nothing new".to_string());
    }
    parts.join(", ")
}

fn match_label(m: GoodreadsMatch) -> &'static str {
    match m {
        GoodreadsMatch::ExternalId => "linked",
        GoodreadsMatch::Isbn => "by isbn",
        GoodreadsMatch::Title => "by title",
        GoodreadsMatch::New => "new",
    }
}

fn match_style(m: GoodreadsMatch) -> ratatui::style::Style {
    match m {
        GoodreadsMatch::New => theme::accent(),
        _ => theme::dim(),
    }
}

fn key_bar() -> Line<'static> {
    Line::from(vec![
        Span::styled(" s", theme::key()),
        Span::styled(" bring across  ", theme::dim()),
        Span::styled("l", theme::key()),
        Span::styled(" link  ", theme::dim()),
        Span::styled("n", theme::key()),
        Span::styled(" as new  ", theme::dim()),
        Span::styled("x", theme::key()),
        Span::styled(" export  ", theme::dim()),
        Span::styled("/", theme::key()),
        Span::styled(" file  ", theme::dim()),
        Span::styled("r", theme::key()),
        Span::styled(" reread  ", theme::dim()),
        Span::styled("m", theme::key()),
        Span::styled(" menu ", theme::dim()),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use bookmole::MatchCandidate;
    use ratatui::style::Modifier;

    fn book(matched_by: GoodreadsMatch) -> GoodreadsPreviewRow {
        GoodreadsPreviewRow::Book {
            title: "Middlemarch".into(),
            matched_by,
            readings: 1,
            shelves: 2,
            rating: Some(5.0),
            review: TextOutcome::Written,
            private_notes: TextOutcome::Absent,
        }
    }

    fn unmatched(
        external_id: Option<&str>,
        candidates: Vec<MatchCandidate>,
    ) -> GoodreadsPreviewRow {
        GoodreadsPreviewRow::Unmatched {
            row: 4,
            title: "Kokoro".into(),
            authors: vec!["Natsume Sōseki".into()],
            external_id: external_id.map(str::to_string),
            candidates,
        }
    }

    #[test]
    fn selection_reverses_the_title_only() {
        let line = row(&book(GoodreadsMatch::Isbn), true);
        let reversed: Vec<bool> = line
            .spans
            .iter()
            .map(|s| s.style.add_modifier.contains(Modifier::REVERSED))
            .collect();
        assert_eq!(reversed, vec![false, false, true, false]);
    }

    #[test]
    fn a_matched_row_says_what_it_would_gain() {
        assert_eq!(
            book_detail(&book(GoodreadsMatch::Isbn)),
            "1 reading, 2 shelves, rated 5, review"
        );
        let nothing = GoodreadsPreviewRow::Book {
            title: "Middlemarch".into(),
            matched_by: GoodreadsMatch::ExternalId,
            readings: 0,
            shelves: 0,
            rating: None,
            review: TextOutcome::Unchanged,
            private_notes: TextOutcome::Absent,
        };
        assert_eq!(book_detail(&nothing), "nothing new");
    }

    #[test]
    fn prose_kept_as_ours_is_said_out_loud() {
        let kept = GoodreadsPreviewRow::Book {
            title: "Middlemarch".into(),
            matched_by: GoodreadsMatch::Isbn,
            readings: 0,
            shelves: 0,
            rating: None,
            review: TextOutcome::KeptOurs,
            private_notes: TextOutcome::KeptOurs,
        };
        let detail = book_detail(&kept);
        assert!(detail.contains("review kept as yours"), "{detail}");
        assert!(detail.contains("notes kept as yours"), "{detail}");
    }

    #[test]
    fn an_undecided_row_offers_only_the_moves_it_has() {
        let candidate = MatchCandidate {
            book_id: 3,
            title: "Kokoro: A Novel".into(),
            authors_display: vec!["Natsume Sōseki".into()],
            publish_year: Some(1914),
            score: 0.71,
        };
        let with_id = row(&unmatched(Some("9000001"), vec![candidate.clone()]), false);
        let text: String = with_id
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert!(text.contains("l to link"), "{text}");

        let without = row(&unmatched(None, vec![candidate]), false);
        let text: String = without
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert!(!text.contains("l to link"), "{text}");
        assert!(text.contains("no Goodreads id"), "{text}");
        assert!(text.contains("n if not"), "{text}");
    }

    #[test]
    fn a_long_candidate_title_never_clips_the_next_move_away() {
        let long = MatchCandidate {
            book_id: 3,
            title: "Kokoro and Other Stories of the Late Meiji Period, Annotated".into(),
            authors_display: vec!["Natsume Sōseki".into()],
            publish_year: Some(1914),
            score: 0.71,
        };
        for id in [Some("9000001"), None] {
            let text: String = row(&unmatched(id, vec![long.clone()]), false)
                .spans
                .iter()
                .map(|s| s.content.to_string())
                .collect();
            assert!(
                text.contains("n if not"),
                "the move was clipped away: {text}"
            );
            assert!(
                text.contains('…'),
                "the long title should have been clipped: {text}"
            );
        }
    }

    #[test]
    fn a_row_with_no_candidates_still_has_a_way_forward() {
        let text: String = row(&unmatched(Some("1"), Vec::new()), false)
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert!(text.contains("n brings it in"), "{text}");
    }

    #[test]
    fn the_empty_state_names_both_directions() {
        assert!(HINT.contains('/'), "{HINT}");
        assert!(HINT.contains('x'), "{HINT}");
    }

    #[test]
    fn the_key_bar_counts_nothing() {
        let bar = key_bar()
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect::<String>();
        assert!(
            !bar.chars().any(|c| c.is_ascii_digit()),
            "the key bar carries a number: {bar}"
        );
    }
}
