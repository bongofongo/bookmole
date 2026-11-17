//! How far into a book a reading is, as a value. Pages win where there is a nonzero page count; the
//! device's percentage fills in otherwise. Not reading state: `Untouched` only means nothing
//! recorded.

use crate::book::Book;
use crate::storage::{Reading, STATUS_FINISHED};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FractionSource {
    Pages,
    Device,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fraction {
    /// Clamped to `0.0..=1.0` on construction; devices report 1.02 and stale page counts exist.
    pub value: f64,
    pub source: FractionSource,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Progress {
    /// Nothing recorded. Deliberately not named `Unread`: a never-opened book and one at page one are
    /// the same row here, and that name would put completion framing into a type.
    Untouched,
    Started {
        page: Option<i64>,
        /// The book's length. Never `Some(0)`: a zero page count is normalised to absence here.
        of: Option<i64>,
        /// `None` is a real answer: a page with no length has no percentage.
        fraction: Option<Fraction>,
    },
    /// The current reading is closed as finished. Carries no page: frontends show a word, not numbers.
    Finished,
}

fn denominator(page_count: Option<i64>) -> Option<i64> {
    page_count.filter(|t| *t > 0)
}

fn clamp(v: f64) -> f64 {
    if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) }
}

impl Progress {
    fn from_parts(
        finished: bool,
        page: Option<i64>,
        page_count: Option<i64>,
        ko: Option<f64>,
    ) -> Progress {
        if finished {
            return Progress::Finished;
        }
        let of = denominator(page_count);
        let fraction = match (page, of) {
            (Some(p), Some(t)) => Some(Fraction {
                value: clamp(p as f64 / t as f64),
                source: FractionSource::Pages,
            }),
            _ => ko.map(|v| Fraction {
                value: clamp(v),
                source: FractionSource::Device,
            }),
        };
        if page.is_none() && fraction.is_none() {
            return Progress::Untouched;
        }
        Progress::Started { page, of, fraction }
    }

    /// The current reading's progress (open one, else the most recent), not the book's across rereads.
    /// A screen showing one specific reading must use [`Progress::of_reading`].
    pub fn of_book(b: &Book) -> Progress {
        Progress::from_parts(b.finished, b.current_page, b.page_count, b.ko_percent)
    }

    /// One named reading's progress. `page_count` comes from the book; finished means closed as
    /// finished by `status`, since `abandon_reading` leaves the reading open.
    pub fn of_reading(r: &Reading, page_count: Option<i64>) -> Progress {
        Progress::from_parts(
            r.status == STATUS_FINISHED,
            r.current_page,
            page_count,
            r.ko_percent,
        )
    }

    /// A whole-number percentage. Pages use integer division, since flooring the float gives 28 for
    /// 29/100; a device fraction rounds.
    pub fn percent(&self) -> Option<i64> {
        match self {
            Progress::Finished => Some(100),
            Progress::Untouched => None,
            Progress::Started { page, of, fraction } => {
                let f = (*fraction)?;
                match f.source {
                    FractionSource::Pages => {
                        let (p, t) = ((*page)?, (*of)?);
                        Some((p.saturating_mul(100) / t).clamp(0, 100))
                    }
                    FractionSource::Device => Some((f.value * 100.0).round() as i64),
                }
            }
        }
    }

    pub fn fraction(&self) -> Option<f64> {
        match self {
            Progress::Finished => Some(1.0),
            Progress::Untouched => None,
            Progress::Started { fraction, .. } => fraction.map(|f| f.value),
        }
    }

    pub fn is_finished(&self) -> bool {
        matches!(self, Progress::Finished)
    }

    pub fn is_untouched(&self) -> bool {
        matches!(self, Progress::Untouched)
    }

    /// The page to name, if any; `None` on a finished reading by design.
    pub fn page(&self) -> Option<i64> {
        match self {
            Progress::Started { page, .. } => *page,
            _ => None,
        }
    }

    pub fn of(&self) -> Option<i64> {
        match self {
            Progress::Started { of, .. } => *of,
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book(page: Option<i64>, count: Option<i64>, finished: bool, ko: Option<f64>) -> Book {
        Book {
            current_page: page,
            page_count: count,
            finished,
            ko_percent: ko,
            ..Book::default()
        }
    }

    #[test]
    fn finished_wins_over_every_number() {
        let p = Progress::of_book(&book(Some(12), Some(200), true, Some(0.4)));
        assert_eq!(p, Progress::Finished);
        assert_eq!(p.percent(), Some(100));
        assert_eq!(p.fraction(), Some(1.0));
    }

    #[test]
    fn a_page_and_a_length_give_a_percentage_from_pages() {
        let p = Progress::of_book(&book(Some(50), Some(200), false, None));
        assert_eq!(p.percent(), Some(25));
        assert_eq!(p.page(), Some(50));
        assert_eq!(p.of(), Some(200));
        let Progress::Started { fraction, .. } = p else {
            panic!("expected Started, got {p:?}");
        };
        assert_eq!(fraction.unwrap().source, FractionSource::Pages);
    }

    #[test]
    fn a_zero_page_count_is_absence_and_never_a_denominator() {
        let p = Progress::of_book(&book(Some(12), Some(0), false, None));
        assert_eq!(p.of(), None, "a zero length must not reach a caller");
        assert_eq!(p.percent(), None);
        assert_eq!(p.page(), Some(12));
    }

    #[test]
    fn a_null_page_count_is_absence_not_zero() {
        let p = Progress::of_book(&book(Some(12), None, false, None));
        assert_eq!(p.of(), None);
        assert_eq!(p.percent(), None);
        assert_eq!(
            p.fraction(),
            None,
            "no track to draw over an unknown length"
        );
    }

    #[test]
    fn the_device_percentage_answers_where_pages_cannot() {
        let p = Progress::of_book(&book(None, None, false, Some(0.435)));
        assert_eq!(p.percent(), Some(44));
        let Progress::Started { fraction, page, .. } = p else {
            panic!("expected Started, got {p:?}");
        };
        assert_eq!(page, None);
        assert_eq!(fraction.unwrap().source, FractionSource::Device);
    }

    #[test]
    fn pages_win_over_the_device_when_both_are_there() {
        let p = Progress::of_book(&book(Some(50), Some(200), false, Some(0.9)));
        assert_eq!(p.percent(), Some(25));
    }

    #[test]
    fn a_page_with_no_length_still_takes_the_device_fraction() {
        let p = Progress::of_book(&book(Some(12), None, false, Some(0.5)));
        assert_eq!(p.page(), Some(12));
        assert_eq!(p.percent(), Some(50));
    }

    #[test]
    fn nothing_recorded_is_untouched_rather_than_zero() {
        let p = Progress::of_book(&book(None, Some(200), false, None));
        assert_eq!(p, Progress::Untouched);
        assert_eq!(p.percent(), None);
        assert_eq!(p.fraction(), None);
    }

    #[test]
    fn out_of_range_input_is_clamped_rather_than_carried() {
        assert_eq!(
            Progress::of_book(&book(Some(300), Some(200), false, None)).percent(),
            Some(100)
        );
        assert_eq!(
            Progress::of_book(&book(None, None, false, Some(1.4))).fraction(),
            Some(1.0)
        );
        assert_eq!(
            Progress::of_book(&book(None, None, false, Some(-0.2))).fraction(),
            Some(0.0)
        );
    }

    fn reading(status: &str, page: Option<i64>, ko: Option<f64>) -> Reading {
        Reading {
            id: 1,
            book_id: 1,
            started_at: None,
            finished_at: None,
            status: status.to_string(),
            source: "manual".to_string(),
            current_page: page,
            ko_status: None,
            ko_percent: ko,
            ko_rating: None,
            created_at: 0,
            last_modified: 0,
            put_down_at: None,
        }
    }

    #[test]
    fn a_reading_is_finished_by_its_status() {
        assert_eq!(
            Progress::of_reading(&reading("finished", Some(10), None), Some(200)),
            Progress::Finished
        );
        let abandoned = Progress::of_reading(&reading("abandoned", Some(50), None), Some(200));
        assert_eq!(abandoned.percent(), Some(25));
        assert!(!abandoned.is_finished(), "put down is not finished");
    }

    #[test]
    fn abandoned_and_reading_have_the_same_shape_of_progress() {
        let a = Progress::of_reading(&reading("abandoned", Some(50), None), Some(200));
        let b = Progress::of_reading(&reading("reading", Some(50), None), Some(200));
        assert_eq!(a, b);
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use proptest::prelude::*;

    fn book(page: Option<i64>, count: Option<i64>, finished: bool, ko: Option<f64>) -> Book {
        Book {
            current_page: page,
            page_count: count,
            finished,
            ko_percent: ko,
            ..Book::default()
        }
    }

    proptest! {
        #[test]
        fn a_fraction_is_always_drawable(
            page in proptest::option::of(-1000i64..100_000),
            count in proptest::option::of(-10i64..5000),
            finished in any::<bool>(),
            ko in proptest::option::of(-2.0f64..3.0),
        ) {
            let p = Progress::of_book(&book(page, count, finished, ko));
            if let Some(f) = p.fraction() {
                prop_assert!((0.0..=1.0).contains(&f), "out of range: {f}");
            }
            if let Some(pct) = p.percent() {
                prop_assert!((0..=100).contains(&pct), "out of range: {pct}");
            }
        }

        #[test]
        fn a_reported_length_is_never_zero(
            page in proptest::option::of(0i64..1000),
            count in proptest::option::of(-5i64..5),
            ko in proptest::option::of(0.0f64..1.0),
        ) {
            let p = Progress::of_book(&book(page, count, false, ko));
            if let Some(t) = p.of() {
                prop_assert!(t > 0, "a false denominator escaped: {t}");
            }
        }

        #[test]
        fn untouched_only_when_there_is_genuinely_nothing(
            page in proptest::option::of(0i64..1000),
            count in proptest::option::of(0i64..1000),
            ko in proptest::option::of(0.0f64..1.0),
        ) {
            let p = Progress::of_book(&book(page, count, false, ko));
            if p.is_untouched() {
                prop_assert!(page.is_none(), "a page was dropped");
                prop_assert!(ko.is_none(), "a device fraction was dropped");
            }
        }
    }
}
