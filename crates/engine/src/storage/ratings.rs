//! Rating scales (numeric `min`/`max`/`step`), their Goodreads lookup, and a review's rating. The
//! raw value is stored with its scale id; the Goodreads map is an explicit table, never a
//! formula.

use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::{Storage, now_unix};
use crate::error::{EngineError, Result};

pub const GOODREADS_STEP: f64 = 0.25;

/// `None` when off the quarter grid, never rounded; `0` means unrated in Goodreads' encoding.
pub fn goodreads_point(g: f64) -> Option<f64> {
    let grid = RatingScale {
        id: 0,
        name: String::new(),
        min: 0.0,
        max: 5.0,
        step: GOODREADS_STEP,
    };
    grid.step_index(g).map(|n| grid.value_at(n))
}

#[derive(Debug, Clone, PartialEq)]
pub struct RatingScale {
    pub id: i64,
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub step: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rating {
    pub scale: RatingScale,
    pub value: f64,
}

pub(super) fn row_to_scale(r: &SqliteRow) -> RatingScale {
    RatingScale {
        id: r.get("id"),
        name: r.get("name"),
        min: r.get("min"),
        max: r.get("max"),
        step: r.get("step"),
    }
}

impl RatingScale {
    /// The one quantizer for every read and write: `rating_map.value` is a REAL key, so values must
    /// be bit-identical.
    pub fn step_index(&self, value: f64) -> Option<i64> {
        if !value.is_finite() || self.step <= 0.0 {
            return None;
        }
        // A tolerance, not exact equality: a typed 4.5 and a computed one may differ in the last bit.
        let tolerance = (self.step * 1e-6).max(1e-8);
        // Bounds get the tolerance too: with a fractional `min`, `value_at(0)` rounds just below it.
        if value < self.min - tolerance || value > self.max + tolerance {
            return None;
        }
        let n = ((value - self.min) / self.step).round();
        let snapped = self.min + n * self.step;
        ((value - snapped).abs() <= tolerance && n >= 0.0).then_some(n as i64)
    }

    pub fn value_at(&self, n: i64) -> f64 {
        // Rounded to nine places so accumulated float error and a typed value land on one key.
        let v = self.min + n as f64 * self.step;
        (v * 1e9).round() / 1e9
    }

    pub fn points(&self) -> Vec<f64> {
        let mut out = Vec::new();
        if self.step <= 0.0 || self.max < self.min {
            return out;
        }
        let last = ((self.max - self.min) / self.step).floor() as i64;
        for n in 0..=last {
            out.push(self.value_at(n));
        }
        out
    }

    pub fn canonical(&self, value: f64) -> Result<f64> {
        match self.step_index(value) {
            Some(n) => Ok(self.value_at(n)),
            None => Err(EngineError::InvalidInput(format!(
                "{value} is not a point on the '{}' scale ({}..{} step {})",
                self.name, self.min, self.max, self.step
            ))),
        }
    }
}

impl Storage {
    /// Redefining keeps the row's id and leaves the default flag alone; creating makes the new scale
    /// the default.
    pub async fn put_rating_scale(
        &self,
        name: &str,
        min: f64,
        max: f64,
        step: f64,
    ) -> Result<RatingScale> {
        if !(min.is_finite() && max.is_finite() && step.is_finite()) || step <= 0.0 || max <= min {
            return Err(EngineError::InvalidInput(format!(
                "a scale needs min < max and a positive step (got {min}..{max} step {step})"
            )));
        }
        let mut tx = self.pool().begin().await?;
        let existing: Option<i64> =
            sqlx::query_scalar("SELECT id FROM rating_scales WHERE name = ?")
                .bind(name)
                .fetch_optional(&mut *tx)
                .await?;

        let row = match existing {
            Some(id) => {
                sqlx::query(
                    "UPDATE rating_scales SET min = ?2, max = ?3, step = ?4 WHERE id = ?1
                     RETURNING id, name, min, max, step",
                )
                .bind(id)
                .bind(min)
                .bind(max)
                .bind(step)
                .fetch_one(&mut *tx)
                .await?
            }
            None => {
                // Clear first: the partial unique index allows one `is_default = 1`.
                sqlx::query("UPDATE rating_scales SET is_default = 0 WHERE is_default = 1")
                    .execute(&mut *tx)
                    .await?;
                sqlx::query(
                    "INSERT INTO rating_scales (name, min, max, step, created_at, is_default)
                     VALUES (?, ?, ?, ?, ?, 1)
                     RETURNING id, name, min, max, step",
                )
                .bind(name)
                .bind(min)
                .bind(max)
                .bind(step)
                .bind(now_unix())
                .fetch_one(&mut *tx)
                .await?
            }
        };
        let scale = row_to_scale(&row);
        tx.commit().await?;
        Ok(scale)
    }

    /// By the `is_default` flag, never creation order, so the seeded `goodreads` scale cannot become
    /// the default. `None` if unset.
    pub async fn active_rating_scale(&self) -> Result<Option<RatingScale>> {
        let row = sqlx::query(
            "SELECT id, name, min, max, step FROM rating_scales WHERE is_default = 1 LIMIT 1",
        )
        .fetch_optional(self.pool())
        .await?;
        Ok(row.as_ref().map(row_to_scale))
    }

    pub async fn rating_scale_by_name(&self, name: &str) -> Result<Option<RatingScale>> {
        let row = sqlx::query("SELECT id, name, min, max, step FROM rating_scales WHERE name = ?")
            .bind(name)
            .fetch_optional(self.pool())
            .await?;
        Ok(row.as_ref().map(row_to_scale))
    }

    pub async fn list_rating_scales(&self) -> Result<Vec<RatingScale>> {
        let rows =
            sqlx::query("SELECT id, name, min, max, step FROM rating_scales ORDER BY id ASC")
                .fetch_all(self.pool())
                .await?;
        Ok(rows.iter().map(row_to_scale).collect())
    }

    pub async fn set_review_rating(
        &self,
        note_id: i64,
        scale: &RatingScale,
        value: f64,
    ) -> Result<()> {
        let value = scale.canonical(value)?;
        sqlx::query(
            "INSERT INTO review_ratings (note_id, scale_id, value) VALUES (?, ?, ?)
             ON CONFLICT (note_id) DO UPDATE SET scale_id = excluded.scale_id,
                                                 value = excluded.value",
        )
        .bind(note_id)
        .bind(scale.id)
        .bind(value)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn clear_review_rating(&self, note_id: i64) -> Result<bool> {
        let done = sqlx::query("DELETE FROM review_ratings WHERE note_id = ?")
            .bind(note_id)
            .execute(self.pool())
            .await?;
        Ok(done.rows_affected() > 0)
    }

    pub async fn review_rating(&self, note_id: i64) -> Result<Option<Rating>> {
        let row = sqlx::query(
            "SELECT s.id, s.name, s.min, s.max, s.step, r.value
               FROM review_ratings r JOIN rating_scales s ON s.id = r.scale_id
              WHERE r.note_id = ?",
        )
        .bind(note_id)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.map(|r| Rating {
            value: r.get("value"),
            scale: row_to_scale(&r),
        }))
    }

    pub async fn map_rating(&self, scale: &RatingScale, value: f64, goodreads: f64) -> Result<()> {
        let Some(goodreads) = goodreads_point(goodreads) else {
            return Err(EngineError::InvalidInput(format!(
                "Goodreads ratings are 0 to 5 in quarters (0 = unrated); got {goodreads}"
            )));
        };
        let value = scale.canonical(value)?;
        sqlx::query(
            "INSERT INTO rating_map (scale_id, value, goodreads) VALUES (?, ?, ?)
             ON CONFLICT (scale_id, value) DO UPDATE SET goodreads = excluded.goodreads",
        )
        .bind(scale.id)
        .bind(value)
        .bind(goodreads)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn rating_map(&self, scale_id: i64) -> Result<Vec<(f64, f64)>> {
        let rows = sqlx::query(
            "SELECT value, goodreads FROM rating_map WHERE scale_id = ? ORDER BY value ASC",
        )
        .bind(scale_id)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .iter()
            .map(|r| (r.get::<f64, _>("value"), r.get::<f64, _>("goodreads")))
            .collect())
    }

    /// `None` is an answer, not a failure: the caller reports it rather than rounding.
    pub async fn goodreads_for(&self, scale: &RatingScale, value: f64) -> Result<Option<f64>> {
        let Some(n) = scale.step_index(value) else {
            return Ok(None);
        };
        let mapped: Option<f64> =
            sqlx::query_scalar("SELECT goodreads FROM rating_map WHERE scale_id = ? AND value = ?")
                .bind(scale.id)
                .bind(scale.value_at(n))
                .fetch_optional(self.pool())
                .await?;
        Ok(mapped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn storage() -> Storage {
        Storage::connect("sqlite::memory:").await.unwrap()
    }

    #[tokio::test]
    async fn the_seeded_scale_is_quarters_and_maps_one_to_one() {
        let s = storage().await;
        let scale = s.active_rating_scale().await.unwrap().expect("seeded");
        assert_eq!(scale.name, "default");
        assert_eq!((scale.min, scale.max, scale.step), (0.0, 5.0, 0.25));

        for p in scale.points() {
            assert_eq!(s.goodreads_for(&scale, p).await.unwrap(), Some(p), "{p}");
        }
        assert_eq!(scale.points().len(), 21);
        assert_eq!(s.goodreads_for(&scale, 4.25).await.unwrap(), Some(4.25));

        s.map_rating(&scale, 4.5, 5.0).await.unwrap();
        assert_eq!(s.goodreads_for(&scale, 4.5).await.unwrap(), Some(5.0));
    }

    #[test]
    fn goodreads_holds_quarters_from_nought_to_five_and_nothing_else() {
        assert_eq!(goodreads_point(0.0), Some(0.0));
        assert_eq!(goodreads_point(4.25), Some(4.25));
        assert_eq!(goodreads_point(5.0), Some(5.0));
        assert_eq!(goodreads_point(4.3), None, "never the nearest quarter");
        assert_eq!(goodreads_point(5.25), None);
        assert_eq!(goodreads_point(-0.25), None);
        assert_eq!(goodreads_point(f64::NAN), None);
    }

    #[tokio::test]
    async fn seeding_the_goodreads_scale_does_not_move_the_default() {
        let s = storage().await;
        let active = s.active_rating_scale().await.unwrap().expect("seeded");
        assert_eq!(active.name, "default");
        assert_eq!((active.min, active.max, active.step), (0.0, 5.0, 0.25));

        let goodreads = s
            .rating_scale_by_name("goodreads")
            .await
            .unwrap()
            .expect("0009 seeds it");
        assert!(goodreads.id > active.id, "and it really is the newer row");
        assert_eq!(
            (goodreads.min, goodreads.max, goodreads.step),
            (0.0, 5.0, 0.25)
        );

        for p in goodreads.points() {
            assert_eq!(s.goodreads_for(&goodreads, p).await.unwrap(), Some(p));
        }
    }

    #[tokio::test]
    async fn a_new_scale_takes_the_default_and_a_redefinition_does_not() {
        let s = storage().await;
        let mine = s.put_rating_scale("mine", 0.0, 10.0, 1.0).await.unwrap();
        assert_eq!(s.active_rating_scale().await.unwrap().unwrap().id, mine.id);

        s.put_rating_scale("default", 0.0, 5.0, 0.25).await.unwrap();
        assert_eq!(
            s.active_rating_scale().await.unwrap().unwrap().id,
            mine.id,
            "redefining an older scale's bounds must not steal the default back"
        );

        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM rating_scales WHERE is_default = 1")
            .fetch_one(s.pool())
            .await
            .unwrap();
        assert_eq!(n, 1);
    }

    #[tokio::test]
    async fn a_value_off_the_grid_is_refused_rather_than_snapped() {
        let s = storage().await;
        let scale = s.active_rating_scale().await.unwrap().unwrap();
        assert!(matches!(
            scale.canonical(4.3),
            Err(EngineError::InvalidInput(_))
        ));
        assert!(matches!(
            scale.canonical(9.0),
            Err(EngineError::InvalidInput(_))
        ));
        assert_eq!(scale.canonical(4.5).unwrap(), 4.5);
    }

    #[tokio::test]
    async fn the_raw_value_and_its_scale_round_trip() {
        let s = storage().await;
        let scale = s.put_rating_scale("tenths", 0.0, 1.0, 0.1).await.unwrap();
        let note = seed_note(&s).await;

        s.set_review_rating(note, &scale, 0.3).await.unwrap();
        let got = s.review_rating(note).await.unwrap().expect("rated");
        assert_eq!(got.value, 0.3);
        assert_eq!(got.scale, scale);

        assert_eq!(s.goodreads_for(&got.scale, got.value).await.unwrap(), None);
        s.map_rating(&scale, 0.3, 2.0).await.unwrap();
        assert_eq!(
            s.goodreads_for(&got.scale, got.value).await.unwrap(),
            Some(2.0)
        );
        s.map_rating(&scale, 0.3, 3.25).await.unwrap();
        assert_eq!(
            s.goodreads_for(&got.scale, got.value).await.unwrap(),
            Some(3.25)
        );
    }

    #[tokio::test]
    async fn a_walked_scale_and_a_typed_value_agree() {
        let s = storage().await;
        let scale = s.put_rating_scale("tenths", 0.0, 1.0, 0.1).await.unwrap();
        for (i, p) in scale.points().iter().enumerate() {
            s.map_rating(&scale, *p, (i / 2) as f64).await.unwrap();
        }
        assert_eq!(s.goodreads_for(&scale, 0.3).await.unwrap(), Some(1.0));
        assert_eq!(s.goodreads_for(&scale, 0.7).await.unwrap(), Some(3.0));
    }

    #[tokio::test]
    async fn the_first_point_of_a_fractional_scale_is_on_its_own_scale() {
        let s = storage().await;
        let min = 2.2720732703878195;
        let scale = s
            .put_rating_scale("odd", min, min + 5.0, 0.273)
            .await
            .unwrap();
        let first = scale.points()[0];
        assert!(first < min, "the rounding really does go under");
        assert_eq!(scale.canonical(first).unwrap(), first);

        s.map_rating(&scale, first, 1.0).await.unwrap();
        assert_eq!(s.goodreads_for(&scale, first).await.unwrap(), Some(1.0));
        assert_eq!(s.goodreads_for(&scale, min).await.unwrap(), Some(1.0));
    }

    #[tokio::test]
    async fn redefining_a_scale_keeps_its_id_and_its_ratings() {
        let s = storage().await;
        let scale = s.active_rating_scale().await.unwrap().unwrap();
        let note = seed_note(&s).await;
        s.set_review_rating(note, &scale, 4.0).await.unwrap();

        let wider = s.put_rating_scale("default", 0.0, 10.0, 1.0).await.unwrap();
        assert_eq!(wider.id, scale.id);
        let got = s.review_rating(note).await.unwrap().unwrap();
        assert_eq!(got.value, 4.0);
        assert_eq!(got.scale.max, 10.0);
    }

    #[tokio::test]
    async fn a_scale_needs_a_positive_step_and_room_to_move() {
        let s = storage().await;
        assert!(s.put_rating_scale("bad", 0.0, 5.0, 0.0).await.is_err());
        assert!(s.put_rating_scale("bad", 5.0, 5.0, 1.0).await.is_err());
        assert!(s.put_rating_scale("bad", 5.0, 0.0, 1.0).await.is_err());
    }

    #[tokio::test]
    async fn goodreads_refuses_a_value_it_cannot_hold() {
        let s = storage().await;
        let scale = s.active_rating_scale().await.unwrap().unwrap();
        for g in [6.0, 5.25, 4.3, -1.0] {
            assert!(
                matches!(
                    s.map_rating(&scale, 4.0, g).await,
                    Err(EngineError::InvalidInput(_))
                ),
                "{g}"
            );
        }
    }

    async fn seed_note(s: &Storage) -> i64 {
        s.insert_note(
            super::super::NewNoteMeta {
                book_id: None,
                reading_id: None,
                highlight_id: None,
                page: None,
                location: None,
                file_path: "unsorted/review.md",
                title: "Review",
                kind: "review",
            },
            "body",
            &[],
        )
        .await
        .unwrap()
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn a_fully_mapped_scale_is_total(
            min in -10.0f64..10.0,
            span in 1.0f64..12.0,
            step in 0.25f64..3.0,
        ) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all().build().unwrap();
            rt.block_on(async {
                let s = Storage::connect("sqlite::memory:").await.unwrap();
                let scale = s.put_rating_scale("p", min, min + span, step).await.unwrap();
                let points = scale.points();
                prop_assert!(!points.is_empty());

                for (i, p) in points.iter().enumerate() {
                    s.map_rating(&scale, *p, (i % 6) as f64).await.unwrap();
                }
                for (i, p) in points.iter().enumerate() {
                    prop_assert_eq!(
                        s.goodreads_for(&scale, *p).await.unwrap(),
                        Some((i % 6) as f64),
                        "unmapped point {} on {}..{} step {}", p, min, min + span, step
                    );
                }

                if points.len() > 1 {
                    let between = (points[0] + points[1]) / 2.0;
                    if scale.step_index(between).is_none() {
                        prop_assert_eq!(s.goodreads_for(&scale, between).await.unwrap(), None);
                    }
                }
                Ok(())
            })?;
        }
    }
}
