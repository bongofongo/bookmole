//! Moments: facts about reading already done, derived on every ask; only which have fired is kept.
//! Evidence must post-date the book's `created_at` and the moment epoch, so imports mint nothing.
//! `Moment::id` is built and parsed only here; to callers it is opaque.

use std::collections::HashSet;

use sqlx::Row;

use super::{Storage, now_unix};
use crate::error::{EngineError, Result};

/// A definition, not a threshold: one day is not a run, and a larger number would set a bar.
pub const RUN_MIN_DAYS: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MomentKind {
    ReadingClosed,
    /// Dated by the evidence's own clock (a highlight's `ko_datetime`), so an import cannot look
    /// like today.
    FirstAnnotation,
    /// `Moment::book_id` is the reflection's own book; `reached_book_id` is the far side.
    ReflectionReached {
        note_id: i64,
        reached_book_id: i64,
    },
    RunEnded {
        /// `YYYY-MM-DD`, UTC, inclusive.
        from: String,
        /// `YYYY-MM-DD`, UTC, inclusive. Also [`Moment::day`].
        to: String,
        days: i64,
    },
}

impl MomentKind {
    /// Stable token that opens an id; changing one orphans recorded acknowledgements.
    pub fn tag(&self) -> &'static str {
        match self {
            MomentKind::ReadingClosed => "reading_closed",
            MomentKind::FirstAnnotation => "first_annotation",
            MomentKind::ReflectionReached { .. } => "reflection_reached",
            MomentKind::RunEnded { .. } => "run_ended",
        }
    }

    const TAGS: [&'static str; 4] = [
        "reading_closed",
        "first_annotation",
        "reflection_reached",
        "run_ended",
    ];
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moment {
    /// Opaque; built from row ids so two frontends acknowledge the same string.
    pub id: String,
    pub kind: MomentKind,
    /// `None` only for [`MomentKind::RunEnded`], which is about the library
    /// rather than a book.
    pub book_id: Option<i64>,
    /// `None` where two reads share the evidence or none claims it.
    pub reading_id: Option<i64>,
    /// `YYYY-MM-DD`, UTC, as in `reading_events`, so a moment and its event share a day.
    pub day: String,
    pub occurred_at: i64,
}

impl Moment {
    fn new(
        kind: MomentKind,
        book_id: Option<i64>,
        reading_id: Option<i64>,
        at: i64,
    ) -> Result<Moment> {
        let subject = match &kind {
            MomentKind::ReadingClosed => reading_id
                .ok_or_else(|| {
                    EngineError::InvalidInput("a closed reading has no reading id".into())
                })?
                .to_string(),
            MomentKind::FirstAnnotation => book_id
                .ok_or_else(|| EngineError::InvalidInput("a first annotation has no book".into()))?
                .to_string(),
            MomentKind::ReflectionReached {
                note_id,
                reached_book_id,
            } => format!("{note_id}:{reached_book_id}"),
            MomentKind::RunEnded { from, to, .. } => format!("{from}..{to}"),
        };
        Ok(Moment {
            id: format!("{}:{subject}", kind.tag()),
            day: day_of_unix(at)?,
            kind,
            book_id,
            reading_id,
            occurred_at: at,
        })
    }
}

fn day_of_unix(ts: i64) -> Result<String> {
    super::reading_events::day_of_unix(ts)
}

fn parse_day(s: &str) -> Option<time::Date> {
    let fmt = time::macros::format_description!("[year]-[month]-[day]");
    time::Date::parse(s, fmt).ok()
}

fn format_day(d: time::Date) -> String {
    let fmt = time::macros::format_description!("[year]-[month]-[day]");
    d.format(fmt).unwrap_or_default()
}

/// A run is dated by the start of the day it ended on.
fn midnight_of(day: time::Date) -> i64 {
    day.midnight().assume_utc().unix_timestamp()
}

/// The last day a run may end on and still be over: the day before yesterday, since a run ending
/// yesterday may continue today. UTC; `now` is passed in.
pub(crate) fn run_cutoff(now: i64) -> Result<time::Date> {
    let today = time::OffsetDateTime::from_unix_timestamp(now)
        .map_err(|e| EngineError::InvalidInput(format!("timestamp {now} is out of range: {e}")))?
        .date();
    Ok(today - time::Duration::days(2))
}

/// Input must be sorted and deduplicated; an unreadable day is dropped, not guessed at.
pub fn runs_of_days(days: &[String]) -> Vec<(String, String, usize)> {
    let mut parsed: Vec<time::Date> = days.iter().filter_map(|d| parse_day(d)).collect();
    parsed.sort_unstable();
    parsed.dedup();

    let mut runs = Vec::new();
    let mut i = 0;
    while i < parsed.len() {
        let start = parsed[i];
        let mut end = start;
        while i + 1 < parsed.len() && parsed[i + 1] == end.next_day().unwrap_or(end) {
            i += 1;
            end = parsed[i];
        }
        // `next_day` is `None` only at `Date::MAX`; `unwrap_or(end)` ends the run instead of panicking.
        let len = (end - start).whole_days() as usize + 1;
        runs.push((format_day(start), format_day(end), len));
        i += 1;
    }
    runs
}

impl Storage {
    pub async fn moment_epoch(&self) -> Result<i64> {
        Ok(
            sqlx::query_scalar::<_, i64>("SELECT began_at FROM moment_epoch WHERE id = 1")
                .fetch_one(self.pool())
                .await?,
        )
    }

    pub async fn set_moment_epoch(&self, began_at: i64) -> Result<()> {
        sqlx::query("UPDATE moment_epoch SET began_at = ? WHERE id = 1")
            .bind(began_at)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// `limit` truncates after ordering. There is deliberately no count: a number waiting is a badge.
    pub async fn pending_moments(&self, now: i64, limit: Option<i64>) -> Result<Vec<Moment>> {
        let epoch = self.moment_epoch().await?;
        let mut out = Vec::new();
        out.extend(self.readings_closed(epoch).await?);
        out.extend(self.first_annotations(epoch).await?);
        out.extend(self.reflections_reached(epoch).await?);
        out.extend(self.runs_ended(epoch, now).await?);

        let surfaced: HashSet<String> = sqlx::query_scalar::<_, String>("SELECT id FROM moments")
            .fetch_all(self.pool())
            .await?
            .into_iter()
            .collect();
        out.retain(|m| !surfaced.contains(&m.id));

        // Tie-break by id so two calls return the same order.
        out.sort_by(|a, b| {
            b.occurred_at
                .cmp(&a.occurred_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        if let Some(n) = limit {
            out.truncate(n.max(0) as usize);
        }
        Ok(out)
    }

    /// Idempotent, keeping the first `surfaced_at`. The id is checked only for a known kind, not
    /// re-derived: a run can stop being pending after it was shown, and refusing would replay it.
    pub async fn acknowledge_moment(&self, id: &str) -> Result<()> {
        let tag = id.split(':').next().unwrap_or_default();
        if !MomentKind::TAGS.contains(&tag) || id.len() <= tag.len() + 1 {
            return Err(EngineError::InvalidInput(format!(
                "{id:?} is not a moment id"
            )));
        }
        sqlx::query(
            "INSERT INTO moments (id, surfaced_at) VALUES (?, ?)
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(id)
        .bind(now_unix())
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// "Closed" means `finished_at IS NOT NULL`; status is not the test, since abandoned reads stay
    /// open.
    async fn readings_closed(&self, epoch: i64) -> Result<Vec<Moment>> {
        let rows = sqlx::query(
            "SELECT r.id AS reading_id, r.book_id, r.finished_at AS at
               FROM readings r
               JOIN books b ON b.id = r.book_id
              WHERE r.finished_at IS NOT NULL
                AND r.finished_at >= b.created_at
                AND r.finished_at >= ?",
        )
        .bind(epoch)
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|r| {
                Moment::new(
                    MomentKind::ReadingClosed,
                    Some(r.get("book_id")),
                    Some(r.get("reading_id")),
                    r.get("at"),
                )
            })
            .collect()
    }

    /// A highlight is dated by `ko_datetime`, never `highlights.created_at`, or every import would mint
    /// a moment per book. `min` over notes and highlights makes it the first.
    async fn first_annotations(&self, epoch: i64) -> Result<Vec<Moment>> {
        let rows = sqlx::query(
            "WITH marks AS (
                 SELECT h.book_id AS book_id,
                        CAST(strftime('%s', h.ko_datetime) AS INTEGER) AS at,
                        h.reading_id AS reading_id
                   FROM highlights h
                  WHERE h.ko_datetime IS NOT NULL
                    AND strftime('%s', h.ko_datetime) IS NOT NULL
                  UNION ALL
                 SELECT n.book_id, n.created_at, n.reading_id
                   FROM notes n
                  WHERE n.book_id IS NOT NULL
             ),
             earliest AS (SELECT book_id, min(at) AS at FROM marks GROUP BY book_id)
             SELECT f.book_id, f.at AS at,
                    (SELECT CASE WHEN count(DISTINCT m.reading_id) = 1
                                 THEN min(m.reading_id) END
                       FROM marks m
                      WHERE m.book_id = f.book_id AND m.at = f.at) AS reading_id
               FROM earliest f
               JOIN books b ON b.id = f.book_id
              WHERE f.at IS NOT NULL
                AND f.at >= b.created_at
                AND f.at >= ?",
        )
        .bind(epoch)
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|r| {
                Moment::new(
                    MomentKind::FirstAnnotation,
                    Some(r.get("book_id")),
                    r.get("reading_id"),
                    r.get("at"),
                )
            })
            .collect()
    }

    /// Only resolved links count. Dated `max` of the two notes' `created_at`, as `note_links` has no
    /// timestamp. Links within the reflection's own book are excluded.
    async fn reflections_reached(&self, epoch: i64) -> Result<Vec<Moment>> {
        let rows = sqlx::query(
            "SELECT * FROM (
                 SELECT r.id AS note_id, r.book_id AS book_id, r.reading_id AS reading_id,
                        t.book_id AS reached_book_id,
                        rb.created_at AS reached_created_at,
                        min(max(r.created_at, t.created_at)) AS at
                   FROM notes r
                   JOIN note_links l ON l.from_note = r.id
                   JOIN notes t ON t.id = l.to_note
                   JOIN books rb ON rb.id = t.book_id
                  WHERE r.kind = 'reflection'
                    AND (r.book_id IS NULL OR t.book_id <> r.book_id)
                  GROUP BY r.id, t.book_id
             )
             WHERE at >= reached_created_at AND at >= ?",
        )
        .bind(epoch)
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|r| {
                Moment::new(
                    MomentKind::ReflectionReached {
                        note_id: r.get("note_id"),
                        reached_book_id: r.get("reached_book_id"),
                    },
                    r.get("book_id"),
                    r.get("reading_id"),
                    r.get("at"),
                )
            })
            .collect()
    }

    /// Days come from `reading_events` with the per-book guard applied to the evidence, so imports add
    /// no days. Run grouping is in Rust (`runs_of_days`), not SQL.
    async fn runs_ended(&self, epoch: i64, now: i64) -> Result<Vec<Moment>> {
        let days: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT e.day
               FROM reading_events e
               JOIN books b ON b.id = e.book_id
              WHERE e.day >= date(b.created_at, 'unixepoch')
                AND e.day >= date(?, 'unixepoch')
              ORDER BY e.day ASC",
        )
        .bind(epoch)
        .fetch_all(self.pool())
        .await?;

        let cutoff = run_cutoff(now)?;
        runs_of_days(&days)
            .into_iter()
            .filter(|(_, to, len)| {
                *len >= RUN_MIN_DAYS && parse_day(to).is_some_and(|d| d <= cutoff)
            })
            .map(|(from, to, len)| {
                let at = parse_day(&to).map(midnight_of).unwrap_or_default();
                Moment::new(
                    MomentKind::RunEnded {
                        from,
                        to,
                        days: len as i64,
                    },
                    None,
                    None,
                    at,
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::book::Book;
    use crate::storage::{NewHighlight, NewNoteMeta};

    const DAY: i64 = 86_400;
    /// 2026-01-05 00:00:00 UTC.
    const JAN5: i64 = 1_767_571_200;
    const NOW: i64 = JAN5 + 100 * DAY;
    const LONG_AGO: i64 = JAN5 - 400 * DAY;

    async fn seeded() -> (Storage, i64) {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        s.set_moment_epoch(LONG_AGO).await.unwrap();
        let id = book_at(&s, "Persuasion", LONG_AGO).await;
        (s, id)
    }

    async fn book_at(s: &Storage, title: &str, created_at: i64) -> i64 {
        let id = s
            .upsert_book(
                &Book {
                    title: Some(title.into()),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
        sqlx::query("UPDATE books SET created_at = ? WHERE id = ?")
            .bind(created_at)
            .bind(id)
            .execute(s.pool())
            .await
            .unwrap();
        id
    }

    fn hl(text: &str, when: &str) -> NewHighlight {
        NewHighlight {
            text: text.into(),
            chapter: None,
            page: Some(1),
            pos0: Some(format!("/body/p[1]/text().{text}")),
            pos1: None,
            ko_datetime: Some(when.into()),
            ko_datetime_updated: None,
            color: None,
            note: None,
            source: "koreader".into(),
        }
    }

    async fn note_on(s: &Storage, book: i64, title: &str, kind: &str, at: i64) -> i64 {
        let path = format!("vault/{title}.md");
        let id = s
            .insert_note(
                NewNoteMeta {
                    book_id: Some(book),
                    highlight_id: None,
                    reading_id: None,
                    file_path: &path,
                    title,
                    kind,
                    page: None,
                    location: None,
                },
                "",
                &[],
            )
            .await
            .unwrap();
        sqlx::query("UPDATE notes SET created_at = ? WHERE id = ?")
            .bind(at)
            .bind(id)
            .execute(s.pool())
            .await
            .unwrap();
        id
    }

    async fn pending(s: &Storage) -> Vec<Moment> {
        s.pending_moments(NOW, None).await.unwrap()
    }

    fn ids(ms: &[Moment]) -> Vec<String> {
        ms.iter().map(|m| m.id.clone()).collect()
    }

    fn of_kind(ms: &[Moment], tag: &str) -> Vec<Moment> {
        ms.iter().filter(|m| m.kind.tag() == tag).cloned().collect()
    }

    #[test]
    fn consecutive_days_collapse_into_one_run_and_a_gap_splits_them() {
        let days: Vec<String> = ["2026-01-01", "2026-01-02", "2026-01-03", "2026-01-05"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            runs_of_days(&days),
            vec![
                ("2026-01-01".into(), "2026-01-03".into(), 3),
                ("2026-01-05".into(), "2026-01-05".into(), 1),
            ]
        );
    }

    #[test]
    fn a_month_boundary_is_not_a_gap() {
        let days: Vec<String> = ["2026-01-31", "2026-02-01", "2026-02-28", "2026-03-01"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            runs_of_days(&days),
            vec![
                ("2026-01-31".into(), "2026-02-01".into(), 2),
                ("2026-02-28".into(), "2026-03-01".into(), 2),
            ],
            "2026 is not a leap year, so the 28th and the 1st are adjacent"
        );
    }

    #[test]
    fn an_unreadable_day_is_dropped_rather_than_joining_two_spans() {
        let days: Vec<String> = ["2026-01-01", "2026-13-45", "2026-01-02"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            runs_of_days(&days),
            vec![("2026-01-01".into(), "2026-01-02".into(), 2)]
        );
    }

    #[tokio::test]
    async fn a_reading_that_closed_after_its_book_arrived_is_a_moment() {
        let (s, b) = seeded().await;
        let r = s
            .record_reading(b, Some(JAN5), Some(JAN5 + 5 * DAY), "finished", "manual")
            .await
            .unwrap();

        let ms = pending(&s).await;
        let closed: Vec<&Moment> = ms
            .iter()
            .filter(|m| m.kind == MomentKind::ReadingClosed)
            .collect();
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].id, format!("reading_closed:{r}"));
        assert_eq!(closed[0].book_id, Some(b));
        assert_eq!(
            closed[0].reading_id,
            Some(r),
            "a card is minted per reading, so the moment has to name one"
        );
        assert_eq!(closed[0].day, "2026-01-10");
    }

    #[tokio::test]
    async fn an_open_reading_is_not_a_moment() {
        let (s, b) = seeded().await;
        s.open_reading(b, Some(JAN5), "manual").await.unwrap();
        assert!(
            pending(&s)
                .await
                .iter()
                .all(|m| m.kind != MomentKind::ReadingClosed)
        );
    }

    #[tokio::test]
    async fn the_first_mark_on_a_book_is_a_moment_and_the_second_is_not() {
        let (s, b) = seeded().await;
        s.insert_highlight(b, &hl("a", "2026-01-05 08:00:00"))
            .await
            .unwrap();
        s.insert_highlight(b, &hl("b", "2026-01-09 08:00:00"))
            .await
            .unwrap();

        let first = of_kind(&pending(&s).await, "first_annotation");
        assert_eq!(first.len(), 1, "one book, one first annotation");
        assert_eq!(first[0].day, "2026-01-05", "the earliest, not the latest");
    }

    #[tokio::test]
    async fn a_note_counts_as_a_first_mark_and_beats_a_later_highlight() {
        let (s, b) = seeded().await;
        note_on(&s, b, "thoughts", "note", JAN5).await;
        s.insert_highlight(b, &hl("later", "2026-01-09 08:00:00"))
            .await
            .unwrap();

        let m = pending(&s)
            .await
            .into_iter()
            .find(|m| m.kind == MomentKind::FirstAnnotation)
            .unwrap();
        assert_eq!(m.day, "2026-01-05");
    }

    #[tokio::test]
    async fn a_highlight_with_no_readable_stamp_is_no_evidence_at_all() {
        let (s, b) = seeded().await;
        s.insert_highlight(b, &hl("junk", "sometime last winter"))
            .await
            .unwrap();
        assert!(
            pending(&s)
                .await
                .iter()
                .all(|m| m.kind != MomentKind::FirstAnnotation),
            "nothing here invents a day, so nothing here invents a moment"
        );
    }

    #[tokio::test]
    async fn a_reflection_reaching_another_book_is_a_moment_once_per_book_reached() {
        let (s, a) = seeded().await;
        let b = book_at(&s, "Middlemarch", LONG_AGO).await;
        let c = book_at(&s, "The Overstory", LONG_AGO).await;

        let target_b = note_on(&s, b, "middlemarch-note", "note", JAN5).await;
        let target_c = note_on(&s, c, "overstory-note", "note", JAN5).await;
        let own = note_on(&s, a, "persuasion-note", "note", JAN5).await;
        let refl = note_on(&s, a, "Reflection: Persuasion", "reflection", JAN5).await;

        s.set_note_links(
            refl,
            "Reflection: Persuasion",
            &[
                "middlemarch-note".into(),
                "overstory-note".into(),
                "persuasion-note".into(),
            ],
        )
        .await
        .unwrap();
        assert!(target_b > 0 && target_c > 0 && own > 0);

        let reached = of_kind(&pending(&s).await, "reflection_reached");
        assert_eq!(
            reached.len(),
            2,
            "two books reached; its own is not a reach"
        );
        assert!(reached.iter().all(|m| m.book_id == Some(a)
            && matches!(
                m.kind,
                MomentKind::ReflectionReached { reached_book_id, .. } if reached_book_id != a
            )));
    }

    #[tokio::test]
    async fn a_dangling_wikilink_reaches_nothing() {
        let (s, a) = seeded().await;
        let refl = note_on(&s, a, "Reflection: Persuasion", "reflection", JAN5).await;
        s.set_note_links(
            refl,
            "Reflection: Persuasion",
            &["a note nobody wrote".into()],
        )
        .await
        .unwrap();
        assert!(
            pending(&s)
                .await
                .iter()
                .all(|m| !matches!(m.kind, MomentKind::ReflectionReached { .. })),
            "a target that is still text belongs to no book"
        );
    }

    #[tokio::test]
    async fn a_plain_note_linking_across_is_not_a_reflection_reaching() {
        let (s, a) = seeded().await;
        let b = book_at(&s, "Middlemarch", LONG_AGO).await;
        note_on(&s, b, "middlemarch-note", "note", JAN5).await;
        let plain = note_on(&s, a, "just a note", "note", JAN5).await;
        s.set_note_links(plain, "just a note", &["middlemarch-note".into()])
            .await
            .unwrap();
        assert!(
            pending(&s)
                .await
                .iter()
                .all(|m| !matches!(m.kind, MomentKind::ReflectionReached { .. }))
        );
    }

    #[tokio::test]
    async fn a_run_of_days_that_ended_is_a_moment_and_a_single_day_is_not() {
        let (s, b) = seeded().await;
        for d in [0, 1, 2, 5] {
            note_on(&s, b, &format!("n{d}"), "note", JAN5 + d * DAY).await;
        }
        s.refill_reading_events().await.unwrap();

        let runs = of_kind(&pending(&s).await, "run_ended");
        assert_eq!(runs.len(), 1, "the lone day five days later is not a run");
        assert_eq!(
            runs[0].kind,
            MomentKind::RunEnded {
                from: "2026-01-05".into(),
                to: "2026-01-07".into(),
                days: 3,
            }
        );
        assert_eq!(runs[0].book_id, None, "a run is about the library");
    }

    #[tokio::test]
    async fn a_run_that_might_still_be_running_is_not_a_moment() {
        let (s, b) = seeded().await;
        for d in [0, 1] {
            note_on(&s, b, &format!("n{d}"), "note", JAN5 + d * DAY).await;
        }
        s.refill_reading_events().await.unwrap();

        // "Now" is the day after the run's last day, which today could still extend: not yet a moment.
        let ongoing = s.pending_moments(JAN5 + 2 * DAY, None).await.unwrap();
        assert!(
            ongoing
                .iter()
                .all(|m| !matches!(m.kind, MomentKind::RunEnded { .. }))
        );

        let over = s.pending_moments(JAN5 + 3 * DAY, None).await.unwrap();
        assert_eq!(
            over.iter()
                .filter(|m| matches!(m.kind, MomentKind::RunEnded { .. }))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn a_four_hundred_book_import_mints_nothing() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        s.set_moment_epoch(LONG_AGO).await.unwrap();

        let arrived = JAN5 + 50 * DAY;
        for n in 0..20 {
            let b = book_at(&s, &format!("Imported {n}"), arrived).await;
            s.record_reading(
                b,
                Some(JAN5 - 300 * DAY),
                Some(JAN5 - 290 * DAY + n * DAY),
                "finished",
                "goodreads",
            )
            .await
            .unwrap();
            s.insert_highlight(b, &hl(&format!("h{n}"), "2025-02-02 10:00:00"))
                .await
                .unwrap();
        }
        s.refill_reading_events().await.unwrap();

        assert_eq!(
            pending(&s).await,
            vec![],
            "an import is history arriving, not a thing you just did"
        );
    }

    #[tokio::test]
    async fn a_book_finished_after_it_arrived_is_still_a_moment() {
        let (s, _) = seeded().await;
        let b = book_at(&s, "Bought Yesterday", JAN5).await;
        s.record_reading(b, Some(JAN5), Some(JAN5 + 2 * DAY), "finished", "manual")
            .await
            .unwrap();
        assert!(
            pending(&s)
                .await
                .iter()
                .any(|m| m.kind == MomentKind::ReadingClosed && m.book_id == Some(b)),
            "the guard is about the order of the two facts, not about importers"
        );
    }

    #[tokio::test]
    async fn everything_older_than_the_epoch_is_history() {
        let (s, b) = seeded().await;
        s.record_reading(b, Some(JAN5), Some(JAN5 + 5 * DAY), "finished", "manual")
            .await
            .unwrap();
        note_on(&s, b, "n", "note", JAN5).await;
        assert!(!pending(&s).await.is_empty());

        s.set_moment_epoch(JAN5 + 50 * DAY).await.unwrap();
        assert_eq!(
            pending(&s).await,
            vec![],
            "the first launch after moments existed is not a ceremony"
        );
    }

    #[tokio::test]
    async fn the_default_epoch_is_now_so_a_fresh_migration_shows_nothing_old() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let b = book_at(&s, "Old News", JAN5).await;
        s.record_reading(b, Some(JAN5), Some(JAN5 + DAY), "finished", "manual")
            .await
            .unwrap();
        assert_eq!(
            s.pending_moments(now_unix(), None).await.unwrap(),
            vec![],
            "the migration's own epoch is what makes the upgrade quiet"
        );
    }

    #[tokio::test]
    async fn an_acknowledged_moment_never_comes_back() {
        let (s, b) = seeded().await;
        s.record_reading(b, Some(JAN5), Some(JAN5 + 5 * DAY), "finished", "manual")
            .await
            .unwrap();
        note_on(&s, b, "n", "note", JAN5).await;

        let first = pending(&s).await;
        assert!(first.len() >= 2);
        for m in &first {
            s.acknowledge_moment(&m.id).await.unwrap();
        }
        assert_eq!(pending(&s).await, vec![]);
    }

    #[tokio::test]
    async fn acknowledging_twice_writes_one_row_and_keeps_the_first_time() {
        let (s, b) = seeded().await;
        s.record_reading(b, Some(JAN5), Some(JAN5 + 5 * DAY), "finished", "manual")
            .await
            .unwrap();
        let m = pending(&s).await.into_iter().next().unwrap();

        s.acknowledge_moment(&m.id).await.unwrap();
        let at: i64 = sqlx::query_scalar("SELECT surfaced_at FROM moments WHERE id = ?")
            .bind(&m.id)
            .fetch_one(s.pool())
            .await
            .unwrap();

        s.acknowledge_moment(&m.id).await.unwrap();
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM moments")
            .fetch_one(s.pool())
            .await
            .unwrap();
        assert_eq!(rows, 1);
        let again: i64 = sqlx::query_scalar("SELECT surfaced_at FROM moments WHERE id = ?")
            .bind(&m.id)
            .fetch_one(s.pool())
            .await
            .unwrap();
        assert_eq!(again, at, "the first surfacing is the one that is true");
    }

    #[tokio::test]
    async fn an_id_of_no_known_kind_is_refused() {
        let (s, _) = seeded().await;
        for bad in ["", "nonsense:1", "reading_closed", "reading_closed:"] {
            assert!(
                matches!(
                    s.acknowledge_moment(bad).await,
                    Err(EngineError::InvalidInput(_))
                ),
                "{bad:?} should not be acknowledgeable"
            );
        }
    }

    #[tokio::test]
    async fn the_list_is_newest_first_and_limit_takes_from_the_top() {
        let (s, b) = seeded().await;
        let c = book_at(&s, "Second", LONG_AGO).await;
        s.record_reading(b, Some(JAN5), Some(JAN5 + DAY), "finished", "manual")
            .await
            .unwrap();
        s.record_reading(c, Some(JAN5), Some(JAN5 + 9 * DAY), "finished", "manual")
            .await
            .unwrap();

        let all = pending(&s).await;
        assert!(all.windows(2).all(|w| w[0].occurred_at >= w[1].occurred_at));
        let one = s.pending_moments(NOW, Some(1)).await.unwrap();
        assert_eq!(ids(&one), ids(&all[..1]));
    }

    #[tokio::test]
    async fn deleting_a_book_takes_its_moments_with_it() {
        let (s, b) = seeded().await;
        s.record_reading(b, Some(JAN5), Some(JAN5 + DAY), "finished", "manual")
            .await
            .unwrap();
        assert!(!pending(&s).await.is_empty());
        s.delete_book(b).await.unwrap();
        assert_eq!(
            pending(&s).await,
            vec![],
            "a moment is derived, so deleting its evidence deletes it"
        );
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        /// Days inside one month, so adjacency is common.
        fn day_set() -> impl Strategy<Value = Vec<String>> {
            prop::collection::vec(1u32..=28, 0..20)
                .prop_map(|ds| ds.into_iter().map(|d| format!("2026-01-{d:02}")).collect())
        }

        proptest! {
            #[test]
            fn runs_partition_the_days_in_order(days in day_set()) {
                let runs = runs_of_days(&days);

                let mut want: Vec<time::Date> =
                    days.iter().filter_map(|d| parse_day(d)).collect();
                want.sort_unstable();
                want.dedup();

                let mut got = Vec::new();
                for (from, to, len) in &runs {
                    let (a, b) = (parse_day(from).unwrap(), parse_day(to).unwrap());
                    prop_assert!(a <= b);
                    prop_assert_eq!((b - a).whole_days() as usize + 1, *len);
                    let mut d = a;
                    loop {
                        got.push(d);
                        if d == b { break; }
                        d = d.next_day().unwrap();
                    }
                }

                prop_assert_eq!(&got, &want);

                for w in runs.windows(2) {
                    let end = parse_day(&w[0].1).unwrap();
                    let next = parse_day(&w[1].0).unwrap();
                    prop_assert!(next > end.next_day().unwrap());
                }
            }

            #[test]
            fn runs_do_not_depend_on_the_order_they_arrive_in(days in day_set()) {
                let sorted = runs_of_days(&days);
                let mut shuffled = days.clone();
                shuffled.reverse();
                shuffled.extend(days.iter().cloned());
                prop_assert_eq!(runs_of_days(&shuffled), sorted);
            }
        }
    }

    #[tokio::test]
    async fn a_moment_fires_at_most_once_however_the_calls_interleave() {
        let (s, b) = seeded().await;
        let c = book_at(&s, "Middlemarch", LONG_AGO).await;
        s.record_reading(b, Some(JAN5), Some(JAN5 + 2 * DAY), "finished", "manual")
            .await
            .unwrap();
        s.record_reading(c, Some(JAN5), Some(JAN5 + 4 * DAY), "finished", "manual")
            .await
            .unwrap();
        note_on(&s, b, "n1", "note", JAN5).await;
        note_on(&s, b, "n2", "note", JAN5 + DAY).await;
        note_on(&s, c, "n3", "note", JAN5 + 2 * DAY).await;
        s.refill_reading_events().await.unwrap();

        let mut seen: Vec<String> = Vec::new();
        loop {
            let batch = s.pending_moments(NOW, Some(1)).await.unwrap();
            let Some(m) = batch.into_iter().next() else {
                break;
            };
            assert!(!seen.contains(&m.id), "{} was offered twice", m.id);
            seen.push(m.id.clone());
            s.acknowledge_moment(&m.id).await.unwrap();
            s.acknowledge_moment(&m.id).await.unwrap();
        }
        assert!(seen.len() >= 4, "the fixture is meant to be interesting");
        assert_eq!(pending(&s).await, vec![]);

        assert_eq!(
            s.pending_moments(NOW + 10 * DAY, None).await.unwrap(),
            vec![]
        );
    }
}
