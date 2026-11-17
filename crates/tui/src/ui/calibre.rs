//! The calibre library's shelf: one row per calibre book in its dry-run import state.
//! Never a dead end: a lookalike row offers those books (`l`) and the override (`n`).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem};

use bookmole::CalibreRowState;

use crate::app::{App, CalibreRow};
use crate::theme;

fn empty_hint(app: &App) -> &'static str {
    if !app.engine.calibre().can_read_library() {
        // Reported, not prescribed: no install instruction or tool name, pinned by
        // `an_absent_calibre_names_nothing_to_install`.
        "calibre's library tools aren't on this machine, so there's no shelf to show"
    } else if app.calibre_scanned {
        "that library holds no books — / to point at another one"
    } else {
        "press r to read your calibre library, / to pick another"
    }
}

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let selected = app.calibre_state.selected();
    let rows: Vec<Line> = app
        .calibre
        .iter()
        .enumerate()
        .map(|(i, r)| row(r, app.calibre_marks.contains(&i), Some(i) == selected))
        .collect();

    let marked = app.calibre_marks.len();
    let where_from = match &app.calibre_library {
        Some(p) => short_root(p),
        None => "yours".to_string(),
    };
    let title = if app.calibre.is_empty() {
        " calibre ".to_string()
    } else if marked == 0 {
        format!(" calibre · {where_from} · {} ", app.calibre.len())
    } else {
        format!(
            " calibre · {where_from} · {} · {marked} marked ",
            app.calibre.len()
        )
    };

    let hint = empty_hint(app);
    let Some((area, block)) = super::shelf_frame(f, area, title, key_bar(), &rows, hint) else {
        return;
    };

    let items: Vec<ListItem> = rows.into_iter().map(ListItem::new).collect();
    let list = List::new(items).block(block).highlight_symbol("› ");
    f.render_stateful_widget(list, area, &mut app.calibre_state);

    if let Some(picker) = &mut app.link_picker {
        super::link_picker(f, picker, area);
    }
}

fn short_root(root: &std::path::Path) -> String {
    root.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string())
}

fn row(r: &CalibreRow, marked: bool, selected: bool) -> Line<'static> {
    let title_style = if selected {
        theme::title().patch(theme::selected())
    } else {
        theme::title()
    };
    let mut spans = vec![
        Span::styled(if marked { "✓ " } else { "  " }, theme::accent()),
        Span::styled(format!("{:<12}", r.state.label()), state_style(&r.state)),
        Span::styled(r.title.clone(), title_style),
    ];
    if let Some(authors) = r.authors_line() {
        spans.push(Span::styled(format!("  {authors}"), theme::dim()));
    }
    let detail = super::clip(detail(r), super::DETAIL_MAX);
    if !detail.is_empty() {
        spans.push(Span::styled(format!("  {detail}"), theme::dim()));
    }
    Line::from(spans)
}

fn detail(r: &CalibreRow) -> String {
    if let Some(note) = &r.note {
        return note.clone();
    }
    match &r.state {
        CalibreRowState::Linked {
            matched_by,
            tags,
            cover,
            files,
            ..
        } => {
            let gains = crate::app::calibre_gains(*tags, *cover, *files);
            format!("{gains} (by {matched_by})")
        }
        CalibreRowState::New => "not in the library — enter brings it in".to_string(),
        CalibreRowState::Candidates(c) => match c.first() {
            Some(c) => super::candidate_hint(&c.title, c.score, "l to link, n if not"),
            None => "nothing here looks like it — n brings it in as new".to_string(),
        },
        CalibreRowState::Unreadable(d) => d.detail.clone(),
    }
}

fn state_style(state: &CalibreRowState) -> ratatui::style::Style {
    match state {
        CalibreRowState::New | CalibreRowState::Candidates(_) => theme::accent(),
        CalibreRowState::Linked { .. } => theme::dim(),
        CalibreRowState::Unreadable(_) => theme::primary(),
    }
}

fn key_bar() -> Line<'static> {
    Line::from(vec![
        Span::styled(" enter", theme::key()),
        Span::styled(" import  ", theme::dim()),
        Span::styled("x", theme::key()),
        Span::styled(" mark  ", theme::dim()),
        Span::styled("s", theme::key()),
        Span::styled(" all  ", theme::dim()),
        Span::styled("l", theme::key()),
        Span::styled(" link  ", theme::dim()),
        Span::styled("n", theme::key()),
        Span::styled(" as new  ", theme::dim()),
        Span::styled("c", theme::key()),
        Span::styled(" convert  ", theme::dim()),
        Span::styled("r", theme::key()),
        Span::styled(" reread  ", theme::dim()),
        Span::styled("m", theme::key()),
        Span::styled(" menu ", theme::dim()),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use bookmole::{CalibreMatch, Diagnostic, DiagnosticKind, MatchCandidate, Severity};
    use ratatui::style::Modifier;

    fn calibre_row(state: CalibreRowState) -> CalibreRow {
        CalibreRow {
            calibre_id: 1,
            uuid: Some("c47437a8".into()),
            title: "Middlemarch".into(),
            authors: vec!["George Eliot".into()],
            state,
            note: None,
            notices: Vec::new(),
        }
    }

    #[test]
    fn selection_reverses_the_title_only() {
        let r = calibre_row(CalibreRowState::New);
        let line = row(&r, false, true);
        let reversed: Vec<bool> = line
            .spans
            .iter()
            .map(|s| s.style.add_modifier.contains(Modifier::REVERSED))
            .collect();
        assert_eq!(reversed, vec![false, false, true, false, false]);
    }

    #[test]
    fn every_state_has_a_readable_detail() {
        let states = [
            CalibreRowState::New,
            CalibreRowState::Linked {
                matched_by: CalibreMatch::Uuid,
                tags: 2,
                cover: true,
                files: 1,
            },
            CalibreRowState::Candidates(vec![MatchCandidate {
                book_id: 3,
                title: "Middlemarch: A Novel".into(),
                authors_display: vec!["George Eliot".into()],
                publish_year: Some(2017),
                score: 0.72,
            }]),
            CalibreRowState::Candidates(Vec::new()),
            CalibreRowState::Unreadable(Diagnostic {
                kind: DiagnosticKind::CalibreRowSkipped { calibre_id: 7 },
                severity: Severity::Warning,
                detail: "no title, so there is nothing to match on".into(),
            }),
        ];
        for state in states {
            let label = state.label();
            let r = calibre_row(state);
            assert!(!detail(&r).is_empty(), "{label} says nothing");
        }
    }

    #[test]
    fn a_candidate_row_names_both_moves() {
        let r = calibre_row(CalibreRowState::Candidates(vec![MatchCandidate {
            book_id: 3,
            title: "Middlemarch: A Novel".into(),
            authors_display: vec!["George Eliot".into()],
            publish_year: Some(2017),
            score: 0.72,
        }]));
        let detail = detail(&r);
        assert!(detail.contains("l to link"), "{detail}");
        assert!(detail.contains("n if not"), "{detail}");
    }

    #[test]
    fn an_imported_row_shows_what_its_import_reported() {
        let mut r = calibre_row(CalibreRowState::New);
        r.note = Some("Middlemarch: 2 tags, cover (matched by new)".into());
        assert_eq!(detail(&r), "Middlemarch: 2 tags, cover (matched by new)");
    }

    #[test]
    fn an_absent_calibre_names_nothing_to_install() {
        let hint = "calibre's library tools aren't on this machine, so there's no shelf to show";
        for forbidden in [
            "install",
            "download",
            "brew ",
            "http",
            ".com",
            "you need",
            "to enable",
        ] {
            assert!(
                !hint.to_lowercase().contains(forbidden),
                "the absent-calibre hint must not say {forbidden:?}: {hint}"
            );
        }
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
