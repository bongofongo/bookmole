//! Edition shape as proportions: width and thickness as multiples of the book's own height.
//! Derivation lives here so every frontend draws the same edition the same way; look is the
//! frontend's. Zero or negative page counts are absence, filled and marked assumed, never short.

use crate::book::Book;

/// Narrowest width/height ratio; covers are cropped at arbitrary aspects, so the clamp restores a
/// plausible object.
pub const NARROWEST: f32 = 0.55;

pub const WIDEST: f32 = 0.85;

pub const THINNEST: f32 = 1.0 / 15.0;

pub const THICKEST: f32 = 4.0 / 15.0;

const PAPERBACK_ASPECT: f32 = 2.0 / 3.0;

const PAPERBACK_PAGES: i64 = 320;

const FEWEST_PAGES: i64 = 48;
const MOST_PAGES: i64 = 1400;

/// Thickness of a zero-page book: a floor that keeps a short book a solid at small render sizes.
/// Not physical; it is why the thickness curve is affine rather than proportional.
const BOARDS: f32 = 0.06;

const PAGES_PER_HEIGHT: f32 = 6750.0;

/// Where one of `EditionShape`'s numbers came from; an assumed value must never pass as recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeSource {
    Recorded,
    Assumed,
}

impl ShapeSource {
    pub fn is_assumed(self) -> bool {
        matches!(self, ShapeSource::Assumed)
    }
}

/// Height is `1.0` and is not a field. Both ratios stay finite and in range for any input,
/// including `NaN`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EditionShape {
    pub width_over_height: f32,
    pub width_source: ShapeSource,
    pub thickness_over_height: f32,
    pub thickness_source: ShapeSource,
}

/// Zero or negative is absence, not a small number.
fn usable_pages(page_count: Option<i64>) -> Option<i64> {
    page_count.filter(|p| *p > 0)
}

/// Rejects non-finite or non-positive values, which a decode can produce.
fn usable_aspect(aspect: Option<f32>) -> Option<f32> {
    aspect.filter(|a| a.is_finite() && *a > 0.0)
}

impl EditionShape {
    pub fn new(page_count: Option<i64>, cover_aspect: Option<f32>) -> EditionShape {
        let (aspect, width_source) = match usable_aspect(cover_aspect) {
            Some(a) => (a, ShapeSource::Recorded),
            None => (PAPERBACK_ASPECT, ShapeSource::Assumed),
        };
        let (pages, thickness_source) = match usable_pages(page_count) {
            Some(p) => (p, ShapeSource::Recorded),
            None => (PAPERBACK_PAGES, ShapeSource::Assumed),
        };
        // Clamped twice: the page clamp is the decision; the output clamp makes the range a guarantee of
        // the type.
        let pages = pages.clamp(FEWEST_PAGES, MOST_PAGES) as f32;
        EditionShape {
            width_over_height: aspect.clamp(NARROWEST, WIDEST),
            width_source,
            thickness_over_height: (BOARDS + pages / PAGES_PER_HEIGHT).clamp(THINNEST, THICKEST),
            thickness_source,
        }
    }

    /// `cover_aspect` is `width / height`; `None` falls back to a trade paperback and reports
    /// `ShapeSource::Assumed`.
    pub fn of_book(book: &Book, cover_aspect: Option<f32>) -> EditionShape {
        EditionShape::new(book.page_count, cover_aspect)
    }

    pub fn is_wholly_assumed(&self) -> bool {
        self.width_source.is_assumed() && self.thickness_source.is_assumed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book(page_count: Option<i64>) -> Book {
        Book {
            page_count,
            ..Book::default()
        }
    }

    #[test]
    fn a_trade_paperback_is_the_reference_shape() {
        let s = EditionShape::new(Some(320), Some(2.0 / 3.0));
        assert!((s.width_over_height - 0.666_666_7).abs() < 1e-6);
        assert!((s.thickness_over_height - 0.107_407_4).abs() < 1e-6);
        assert_eq!(s.width_source, ShapeSource::Recorded);
        assert_eq!(s.thickness_source, ShapeSource::Recorded);
    }

    #[test]
    fn a_doorstop_is_visibly_fatter_than_a_pamphlet() {
        let fat = EditionShape::new(Some(1408), None).thickness_over_height;
        let thin = EditionShape::new(Some(48), None).thickness_over_height;
        assert!(
            fat > thin * 3.0,
            "{fat} vs {thin} is not a visible difference"
        );
    }

    /// `page_count = 0` is a real row meaning nobody recorded one; it must not draw as the thinnest
    /// book.
    #[test]
    fn a_zero_page_count_is_absence_and_not_a_pamphlet() {
        let zero = EditionShape::new(Some(0), None);
        let unknown = EditionShape::new(None, None);
        assert_eq!(zero, unknown);
        assert_eq!(zero.thickness_source, ShapeSource::Assumed);
        assert!(
            zero.thickness_over_height > EditionShape::new(Some(48), None).thickness_over_height,
            "unknown length drew thinner than the thinnest real book"
        );
    }

    #[test]
    fn a_negative_page_count_is_absence_too() {
        assert_eq!(
            EditionShape::new(Some(-40), None),
            EditionShape::new(None, None)
        );
    }

    #[test]
    fn an_unknown_length_lands_inside_the_range_not_at_an_edge() {
        let s = EditionShape::new(None, None);
        assert!(
            s.thickness_over_height > THINNEST && s.thickness_over_height < THICKEST,
            "{} is at an edge",
            s.thickness_over_height
        );
        assert_eq!(
            s.thickness_over_height,
            EditionShape::new(Some(320), None).thickness_over_height,
            "the invented length is a paperback"
        );
        assert_ne!(s, EditionShape::new(Some(320), None));
        assert!(s.is_wholly_assumed());
    }

    #[test]
    fn a_missing_cover_is_a_trade_paperback_and_says_so() {
        let s = EditionShape::new(Some(320), None);
        assert!((s.width_over_height - PAPERBACK_ASPECT).abs() < 1e-6);
        assert_eq!(s.width_source, ShapeSource::Assumed);
        assert_eq!(s.thickness_source, ShapeSource::Recorded);
    }

    /// A cover image is not the object: thumbnails and jacket wraps have unrelated aspects.
    #[test]
    fn a_square_cover_does_not_make_a_square_book() {
        assert_eq!(
            EditionShape::new(Some(300), Some(1.0)).width_over_height,
            WIDEST
        );
        assert_eq!(
            EditionShape::new(Some(300), Some(0.2)).width_over_height,
            NARROWEST
        );
    }

    /// `f32::clamp` passes `NaN` through, which would reach a vertex buffer.
    #[test]
    fn a_nonsense_aspect_falls_back_rather_than_propagating() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.0, -1.5] {
            let s = EditionShape::new(Some(300), Some(bad));
            assert!(
                (s.width_over_height - PAPERBACK_ASPECT).abs() < 1e-6,
                "{bad}"
            );
            assert_eq!(s.width_source, ShapeSource::Assumed, "{bad}");
        }
    }

    #[test]
    fn beyond_the_page_range_thickness_stops_growing() {
        let long = EditionShape::new(Some(1400), None).thickness_over_height;
        assert_eq!(
            long,
            EditionShape::new(Some(20_000), None).thickness_over_height
        );
        assert_eq!(long, THICKEST);
        let short = EditionShape::new(Some(48), None).thickness_over_height;
        assert_eq!(
            short,
            EditionShape::new(Some(1), None).thickness_over_height
        );
    }

    #[test]
    fn of_book_reads_the_length_off_the_row() {
        assert_eq!(
            EditionShape::of_book(&book(Some(900)), Some(0.7)),
            EditionShape::new(Some(900), Some(0.7))
        );
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn a_shape_is_always_drawable(
            pages in proptest::option::of(any::<i64>()),
            aspect in proptest::option::of(any::<f32>()),
        ) {
            let s = EditionShape::new(pages, aspect);
            prop_assert!(s.width_over_height.is_finite());
            prop_assert!(s.thickness_over_height.is_finite());
            prop_assert!(
                (NARROWEST..=WIDEST).contains(&s.width_over_height),
                "width out of range: {}", s.width_over_height
            );
            prop_assert!(
                (THINNEST..=THICKEST).contains(&s.thickness_over_height),
                "thickness out of range: {}", s.thickness_over_height
            );
        }

        #[test]
        fn more_pages_is_never_thinner(
            a in 1i64..30_000,
            b in 1i64..30_000,
        ) {
            let (lo, hi) = (a.min(b), a.max(b));
            prop_assert!(
                EditionShape::new(Some(lo), None).thickness_over_height
                    <= EditionShape::new(Some(hi), None).thickness_over_height
            );
        }

        #[test]
        fn a_wider_cover_is_never_a_narrower_book(
            a in 0.01f32..8.0,
            b in 0.01f32..8.0,
        ) {
            let (lo, hi) = (a.min(b), a.max(b));
            prop_assert!(
                EditionShape::new(None, Some(lo)).width_over_height
                    <= EditionShape::new(None, Some(hi)).width_over_height
            );
        }

        #[test]
        fn recorded_means_something_was_recorded(
            pages in proptest::option::of(-100i64..5000),
            aspect in proptest::option::of(-2.0f32..4.0),
        ) {
            let s = EditionShape::new(pages, aspect);
            if s.thickness_source == ShapeSource::Recorded {
                prop_assert!(pages.is_some_and(|p| p > 0), "a guessed length claimed to be recorded");
            }
            if s.width_source == ShapeSource::Recorded {
                prop_assert!(
                    aspect.is_some_and(|a| a > 0.0 && a.is_finite()),
                    "a guessed aspect claimed to be recorded"
                );
            }
        }
    }
}
