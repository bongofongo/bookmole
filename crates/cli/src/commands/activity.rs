//! An unmeasured column prints `—` or "not measured", never `0`; nothing counts what was not done.

use anyhow::Result;
use bookmole::{ActivitySummary, Confidence, DayActivity, DayRange, Engine, ReadingEvent};
use time::{Duration, OffsetDateTime, macros::format_description};

use super::resolve_one;

pub struct Args {
    pub book: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub days: bool,
    pub refill: bool,
}

const DEFAULT_SPAN_DAYS: i64 = 30;

pub async fn run(engine: &Engine, args: Args) -> Result<()> {
    if args.refill {
        let report = engine.refill_reading_events().await?;
        println!(
            "rebuilt from what was already here: {} new, {} changed",
            report.inserted(),
            report.updated()
        );
    }

    if let Some(selector) = &args.book {
        return one_book(engine, selector).await;
    }

    let range = requested_range(args.from.as_deref(), args.to.as_deref())?;
    let summary = engine.activity_summary(&range).await?;
    print!("{}", summary_text(&summary));

    if summary.activity_days == 0 && !args.refill {
        println!("    nothing recorded here yet.");
        println!("    bookmole activity --refill  builds it from what you already have");
        return Ok(());
    }
    if args.days {
        for d in engine.activity_by_day(&range).await? {
            println!("{}", day_line(&d));
        }
    }
    Ok(())
}

async fn one_book(engine: &Engine, selector: &str) -> Result<()> {
    let book = resolve_one(engine, selector).await?;
    let id = book.id.expect("a stored book has an id");
    let events = engine.reading_events(id).await?;

    if events.is_empty() {
        println!("{}: no days recorded.", book.display_title());
        println!("    bookmole activity --refill  builds it from what you already have");
        return Ok(());
    }
    println!("{} — {} days", book.display_title(), day_count(&events));
    for e in &events {
        println!("{}", event_line(e));
    }
    Ok(())
}

/// Distinct days, not rows: two sources reporting one day count once.
fn day_count(events: &[ReadingEvent]) -> usize {
    let mut days: Vec<&str> = events.iter().map(|e| e.day.as_str()).collect();
    days.sort_unstable();
    days.dedup();
    days.len()
}

/// Source and confidence are both printed: an inferred day is evidence, not a measurement.
fn event_line(e: &ReadingEvent) -> String {
    format!(
        "  {}  {:>7}  {:>6}  {:<9} {}",
        e.day,
        measured(e.minutes, "min"),
        measured(e.pages, "pp"),
        e.source,
        match e.confidence {
            Confidence::Measured => "measured",
            Confidence::Inferred => "inferred",
        }
    )
}

fn day_line(d: &DayActivity) -> String {
    format!(
        "  {}  {:>7}  {:>6}  {} {}",
        d.day,
        measured(d.minutes, "min"),
        measured(d.pages, "pp"),
        d.books,
        if d.books == 1 { "book" } else { "books" }
    )
}

fn summary_text(s: &ActivitySummary) -> String {
    let mut out = format!("{} … {}\n", s.range.from(), s.range.to());
    let mut line = |label: &str, value: String| out.push_str(&format!("  {label:<14} {value}\n"));
    line("days read", s.activity_days.to_string());
    line("books finished", s.books_finished.to_string());
    line("notes", s.notes_created.to_string());
    line("links", s.links_created.to_string());
    // Words, not `—`: a dash in a total column reads as a number.
    line("minutes", total(s.minutes));
    line("pages", total(s.pages));
    out
}

fn total(n: Option<i64>) -> String {
    n.map(|n| n.to_string())
        .unwrap_or_else(|| "not measured".into())
}

fn measured(n: Option<i64>, unit: &str) -> String {
    match n {
        Some(n) => format!("{n} {unit}"),
        None => "—".into(),
    }
}

/// Defaults are computed here because "today" belongs to the machine running the command.
fn requested_range(from: Option<&str>, to: Option<&str>) -> Result<DayRange> {
    let today = OffsetDateTime::now_utc();
    let fmt = format_description!("[year]-[month]-[day]");
    let to = match to {
        Some(t) => t.to_string(),
        None => today.format(fmt)?,
    };
    let from = match from {
        Some(f) => f.to_string(),
        None => (today - Duration::days(DEFAULT_SPAN_DAYS - 1)).format(fmt)?,
    };
    Ok(DayRange::new(&from, &to)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(minutes: Option<i64>, confidence: Confidence) -> ReadingEvent {
        ReadingEvent {
            book_id: 1,
            reading_id: None,
            day: "2026-08-01".into(),
            minutes,
            pages: None,
            source: "koreader".into(),
            confidence,
            created_at: 0,
        }
    }

    #[test]
    fn nothing_measured_is_never_printed_as_zero() {
        assert_eq!(measured(None, "min"), "—");
        assert_eq!(measured(Some(0), "min"), "0 min");
        assert_eq!(measured(Some(42), "min"), "42 min");

        let line = event_line(&event(None, Confidence::Inferred));
        assert!(line.contains('—'), "{line}");
        assert!(!line.contains(" 0 "), "{line}");
    }

    #[test]
    fn a_period_nobody_measured_says_so_rather_than_reporting_zero() {
        let s = ActivitySummary {
            range: DayRange::new("2026-01-01", "2026-01-31").unwrap(),
            books_finished: 0,
            activity_days: 3,
            notes_created: 2,
            links_created: 1,
            minutes: None,
            pages: None,
        };
        let out = summary_text(&s);
        assert!(out.contains("minutes        not measured"), "{out}");
        assert!(out.contains("pages          not measured"), "{out}");
        assert!(out.contains("books finished 0"), "{out}");
        for banned in ["streak", "goal", "missed", "of 31", "remaining"] {
            assert!(!out.contains(banned), "{banned} in: {out}");
        }
        assert_eq!(total(Some(0)), "0");
    }
}
