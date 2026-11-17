//! The mounted reader's shelf: one row per device book in a `scan_device` state, paths hidden.
//! An unmatched row offers the library books it might already be.

use bookmole::DeviceState;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem};

use crate::app::{App, DeviceRow};
use crate::theme;

const HINT: &str = "press r to look again, / for another path";

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let selected = app.device_state.selected();
    let rows: Vec<Line> = app
        .device
        .iter()
        .enumerate()
        .map(|(i, r)| row(r, app.device_marks.contains(&i), Some(i) == selected))
        .collect();

    let marked = app.device_marks.len();
    let title = match (&app.device_root, marked) {
        (Some(root), 0) => format!(" device · {} · {} ", short_root(root), app.device.len()),
        (Some(root), n) => format!(
            " device · {} · {} · {n} marked ",
            short_root(root),
            app.device.len()
        ),
        (None, _) => " device ".to_string(),
    };
    let Some((area, block)) = super::shelf_frame(f, area, title, key_bar(), &rows, HINT) else {
        return;
    };

    let items: Vec<ListItem> = rows.into_iter().map(ListItem::new).collect();
    let list = List::new(items).block(block).highlight_symbol("› ");
    f.render_stateful_widget(list, area, &mut app.device_state);

    if let Some(picker) = &mut app.link_picker {
        super::link_picker(f, picker, area);
    }
}

fn short_root(root: &std::path::Path) -> String {
    root.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string())
}

fn row(r: &DeviceRow, marked: bool, selected: bool) -> Line<'static> {
    let title_style = if selected {
        theme::title().patch(theme::selected())
    } else {
        theme::title()
    };
    let b = &r.book;
    let mut spans = vec![
        Span::styled(if marked { "✓ " } else { "  " }, theme::accent()),
        Span::styled(format!("{:<12}", b.state.label()), state_style(&b.state)),
        Span::styled(b.display_title(), title_style),
    ];
    if let Some(authors) = &b.authors {
        spans.push(Span::styled(
            format!("  {}", authors.replace('\n', ", ")),
            theme::dim(),
        ));
    }
    if let Some(p) = b.ko_percent {
        spans.push(Span::styled(
            format!("  {:.0}%", p * 100.0),
            theme::accent(),
        ));
    }
    let detail = super::clip(detail(r), super::DETAIL_MAX);
    if !detail.is_empty() {
        spans.push(Span::styled(format!("  {detail}"), theme::dim()));
    }
    Line::from(spans)
}

fn detail(r: &DeviceRow) -> String {
    if let Some(note) = &r.note {
        return note.clone();
    }
    match &r.book.state {
        DeviceState::Updated {
            new_highlights,
            refreshed,
        } => {
            let mut parts = Vec::new();
            if *new_highlights > 0 {
                parts.push(format!("{new_highlights} new"));
            }
            if *refreshed > 0 {
                parts.push(format!("{refreshed} edited on the device"));
            }
            parts.join(", ")
        }
        DeviceState::New { candidates } => match candidates.first() {
            Some(c) => format!("maybe “{}” ({:.0}%) — l to link", c.title, c.score * 100.0),
            None => "not in the library — enter to pull it in".to_string(),
        },
        DeviceState::Unreadable(d) => d.detail.clone(),
        DeviceState::Unchanged => String::new(),
    }
}

fn state_style(state: &DeviceState) -> ratatui::style::Style {
    match state {
        DeviceState::New { .. } | DeviceState::Updated { .. } => theme::accent(),
        DeviceState::Unchanged => theme::dim(),
        DeviceState::Unreadable(_) => theme::primary(),
    }
}

fn key_bar() -> Line<'static> {
    Line::from(vec![
        Span::styled(" enter", theme::key()),
        Span::styled(" pull  ", theme::dim()),
        Span::styled("x", theme::key()),
        Span::styled(" mark  ", theme::dim()),
        Span::styled("s", theme::key()),
        Span::styled(" sync  ", theme::dim()),
        Span::styled("l", theme::key()),
        Span::styled(" link  ", theme::dim()),
        Span::styled("r", theme::key()),
        Span::styled(" rescan  ", theme::dim()),
        Span::styled("m", theme::key()),
        Span::styled(" menu ", theme::dim()),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use bookmole::{DeviceBook, MatchCandidate};
    use ratatui::style::Modifier;

    fn device_row(state: DeviceState) -> DeviceRow {
        DeviceRow::new(DeviceBook {
            path: std::path::PathBuf::from("/mnt/Middlemarch.sdr/metadata.epub.lua"),
            title: Some("Middlemarch".into()),
            authors: Some("George Eliot".into()),
            partial_md5: None,
            book_id: None,
            matched_by: None,
            state,
            ko_percent: Some(0.42),
            ko_status: None,
        })
    }

    #[test]
    fn selection_reverses_the_title_only() {
        let r = device_row(DeviceState::Unchanged);
        let line = row(&r, false, true);
        let reversed: Vec<bool> = line
            .spans
            .iter()
            .map(|s| s.style.add_modifier.contains(Modifier::REVERSED))
            .collect();
        assert_eq!(reversed, vec![false, false, true, false, false]);

        let plain = row(&r, false, false);
        assert!(
            plain
                .spans
                .iter()
                .all(|s| !s.style.add_modifier.contains(Modifier::REVERSED))
        );
    }

    #[test]
    fn every_state_has_a_readable_detail() {
        assert_eq!(detail(&device_row(DeviceState::Unchanged)), "");
        assert_eq!(
            detail(&device_row(DeviceState::Updated {
                new_highlights: 2,
                refreshed: 1
            })),
            "2 new, 1 edited on the device"
        );
        assert!(
            detail(&device_row(DeviceState::New {
                candidates: Vec::new()
            }))
            .contains("not in the library")
        );
        assert!(
            detail(&device_row(DeviceState::New {
                candidates: vec![MatchCandidate {
                    book_id: 1,
                    title: "Middlemarch".into(),
                    authors_display: vec!["George Eliot".into()],
                    publish_year: Some(2017),
                    score: 0.72,
                }],
            }))
            .contains("72%")
        );
    }

    #[test]
    fn a_pulled_row_shows_what_its_pull_reported() {
        let mut r = device_row(DeviceState::Unchanged);
        r.note = Some("Middlemarch: 2 new, 0 refreshed, 5 already here".into());
        assert_eq!(
            detail(&r),
            "Middlemarch: 2 new, 0 refreshed, 5 already here"
        );
    }
}
