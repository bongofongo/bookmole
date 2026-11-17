//! Copies and their places, and facts derived from them: `in_library`, `touched`,
//! `read_without_copy`.
//! Only the user writes `have_now`; free text is private and never logged.

use sqlx::sqlite::{SqliteConnection, SqliteRow};
use sqlx::{Row, SqlitePool};

use super::readings::STATUS_FINISHED;
use super::{Storage, now_unix};
use crate::copies::{
    BookStanding, Copy, CopyForm, CopyPlace, CopyStanding, LibraryCopies, LibraryCopyCounts,
    NewCopy, NewPlace, PlaceAttach, PlaceFile, PlaceKind, Tenure,
};
use crate::error::{EngineError, Result};

/// SQL: is this book touched (reading, reading event, highlight, note or flashcard)? `book_id` is
/// an
/// expression. Lists, tags, metadata, covers and links do not count.
pub fn touched_sql(book_id: &str) -> String {
    format!(
        "(EXISTS (SELECT 1 FROM readings t_r WHERE t_r.book_id = {book_id}) \
          OR EXISTS (SELECT 1 FROM reading_events t_e WHERE t_e.book_id = {book_id}) \
          OR EXISTS (SELECT 1 FROM highlights t_h WHERE t_h.book_id = {book_id}) \
          OR EXISTS (SELECT 1 FROM notes t_n WHERE t_n.book_id = {book_id}) \
          OR EXISTS (SELECT 1 FROM flashcards t_f WHERE t_f.book_id = {book_id}))"
    )
}

/// SQL: is this book in the library? A copy whose `have_now` is not explicitly false, or any copy
/// and
/// touched. `have_now` is three-valued: only an explicit `false` removes a copy from the reckoning.
pub fn in_library_sql(book_id: &str) -> String {
    format!(
        "(EXISTS (SELECT 1 FROM copies il_c \
                   WHERE il_c.book_id = {book_id} AND (il_c.have_now IS NULL OR il_c.have_now <> 0)) \
          OR (EXISTS (SELECT 1 FROM copies il_a WHERE il_a.book_id = {book_id}) AND {}))",
        touched_sql(book_id)
    )
}

const COPY_COLUMNS: &str = "id, book_id, form, tenure, have_now, from_whom, app, location, \
     started_at, due_at, ended_at, lent_to, unreadable, note, learned_from, created_at, updated_at";

const PLACE_COLUMNS: &str =
    "copy_id, kind, ref, detail, last_hash, last_seen_at, last_mtime, missing_since";

/// Empty or whitespace-only text becomes `NULL`.
fn text(s: &Option<String>) -> Option<&str> {
    s.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

fn row_to_copy(row: &SqliteRow) -> Result<Copy> {
    let form: Option<String> = row.try_get("form")?;
    let tenure: String = row.try_get("tenure")?;
    let learned: String = row.try_get("learned_from")?;
    let have: Option<i64> = row.try_get("have_now")?;
    Ok(Copy {
        id: row.try_get("id")?,
        book_id: row.try_get("book_id")?,
        form: form.map(|f| f.parse::<CopyForm>()).transpose()?,
        tenure: tenure.parse()?,
        have_now: have.map(|h| h != 0),
        from_whom: row.try_get("from_whom")?,
        app: row.try_get("app")?,
        location: row.try_get("location")?,
        started_at: row.try_get("started_at")?,
        due_at: row.try_get("due_at")?,
        ended_at: row.try_get("ended_at")?,
        lent_to: row.try_get("lent_to")?,
        unreadable: row.try_get::<i64, _>("unreadable")? != 0,
        note: row.try_get("note")?,
        learned_from: learned.parse()?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        places: Vec::new(),
    })
}

fn row_to_place(row: &SqliteRow) -> Result<CopyPlace> {
    let kind: String = row.try_get("kind")?;
    Ok(CopyPlace {
        copy_id: row.try_get("copy_id")?,
        kind: kind.parse()?,
        reference: row.try_get("ref")?,
        detail: row.try_get("detail")?,
        last_hash: row.try_get("last_hash")?,
        last_seen_at: row.try_get("last_seen_at")?,
        last_mtime: row.try_get("last_mtime")?,
        missing_since: row.try_get("missing_since")?,
    })
}

async fn places_in(conn: &mut SqliteConnection, copy_id: i64) -> Result<Vec<CopyPlace>> {
    let rows = sqlx::query(&format!(
        "SELECT {PLACE_COLUMNS} FROM copy_places WHERE copy_id = ? ORDER BY kind, ref"
    ))
    .bind(copy_id)
    .fetch_all(&mut *conn)
    .await?;
    rows.iter().map(row_to_place).collect()
}

async fn copy_in(conn: &mut SqliteConnection, id: i64) -> Result<Option<Copy>> {
    let row = sqlx::query(&format!("SELECT {COPY_COLUMNS} FROM copies WHERE id = ?"))
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
    let Some(row) = row else { return Ok(None) };
    let mut copy = row_to_copy(&row)?;
    copy.places = places_in(conn, id).await?;
    Ok(Some(copy))
}

/// Folds `drop` into `keep`: places move, gaps fill (`keep` wins), notes join. `have_now` is filled
/// only when `keep` has not answered.
async fn merge_copies_in(conn: &mut SqliteConnection, keep: i64, drop: i64) -> Result<()> {
    if keep == drop {
        return Ok(());
    }
    let (Some(k), Some(d)) = (copy_in(conn, keep).await?, copy_in(conn, drop).await?) else {
        return Err(EngineError::InvalidInput(format!(
            "no such copy to merge: {keep} / {drop}"
        )));
    };
    if k.book_id != d.book_id {
        return Err(EngineError::InvalidInput(
            "copies of two different books are not the same copy".into(),
        ));
    }
    sqlx::query("UPDATE copy_places SET copy_id = ? WHERE copy_id = ?")
        .bind(keep)
        .bind(drop)
        .execute(&mut *conn)
        .await?;
    // Highlights made in the absorbed copy were made in the survivor; the FK would otherwise null them.
    sqlx::query("UPDATE highlights SET copy_id = ? WHERE copy_id = ?")
        .bind(keep)
        .bind(drop)
        .execute(&mut *conn)
        .await?;
    let note = match (&k.note, &d.note) {
        (Some(a), Some(b)) if a != b => Some(format!("{a}\n{b}")),
        (Some(a), _) => Some(a.clone()),
        (None, b) => b.clone(),
    };
    let tenure = if k.tenure == Tenure::Unrecorded {
        d.tenure
    } else {
        k.tenure
    };
    sqlx::query(
        "UPDATE copies SET form = ?, tenure = ?, have_now = ?, from_whom = ?, app = ?,
                location = ?, started_at = ?, due_at = ?, ended_at = ?, lent_to = ?,
                unreadable = ?, note = ?, updated_at = ?
          WHERE id = ?",
    )
    .bind(k.form.or(d.form).map(|f| f.as_str()))
    .bind(tenure.as_str())
    .bind(k.have_now.or(d.have_now))
    .bind(k.from_whom.or(d.from_whom))
    .bind(k.app.or(d.app))
    .bind(k.location.or(d.location))
    .bind(k.started_at.or(d.started_at))
    .bind(k.due_at.or(d.due_at))
    .bind(k.ended_at.or(d.ended_at))
    .bind(k.lent_to.or(d.lent_to))
    .bind(k.unreadable || d.unreadable)
    .bind(note)
    .bind(now_unix())
    .bind(keep)
    .execute(&mut *conn)
    .await?;
    sqlx::query("DELETE FROM copies WHERE id = ?")
        .bind(drop)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// `merge_books`' block: repoints every copy of `src` to `dst`. A plain UPDATE, since `copy_places`
/// is keyed
/// on `(kind, ref)` and a copy is identified by its place; look-alike copies stay distinct.
pub(super) async fn repoint_copies(conn: &mut SqliteConnection, src: i64, dst: i64) -> Result<u64> {
    Ok(
        sqlx::query("UPDATE copies SET book_id = ? WHERE book_id = ?")
            .bind(dst)
            .bind(src)
            .execute(&mut *conn)
            .await?
            .rows_affected(),
    )
}

async fn attach_place_in(
    conn: &mut SqliteConnection,
    copy_id: i64,
    place: &NewPlace,
) -> Result<PlaceAttach> {
    let book: Option<i64> = sqlx::query_scalar("SELECT book_id FROM copies WHERE id = ?")
        .bind(copy_id)
        .fetch_optional(&mut *conn)
        .await?;
    let Some(book_id) = book else {
        return Err(EngineError::InvalidInput(format!(
            "no such copy: {copy_id}"
        )));
    };
    let held: Option<(i64, i64)> = sqlx::query_as(
        "SELECT p.copy_id, c.book_id FROM copy_places p JOIN copies c ON c.id = p.copy_id
          WHERE p.kind = ? AND p.ref = ?",
    )
    .bind(place.kind.as_str())
    .bind(&place.reference)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(match held {
        Some((c, _)) if c == copy_id => PlaceAttach::Present,
        Some((c, b)) if b == book_id => {
            merge_copies_in(conn, copy_id, c).await?;
            PlaceAttach::Merged { absorbed: c }
        }
        Some((c, b)) => PlaceAttach::OtherBook {
            book_id: b,
            copy_id: c,
        },
        None => {
            sqlx::query(
                "INSERT INTO copy_places (copy_id, kind, ref, detail, last_hash, last_seen_at,
                                          last_mtime)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(copy_id)
            .bind(place.kind.as_str())
            .bind(&place.reference)
            .bind(place.detail.as_deref())
            .bind(place.last_hash.as_deref())
            .bind(now_unix())
            .bind(place.last_mtime)
            .execute(&mut *conn)
            .await?;
            PlaceAttach::Added
        }
    })
}

async fn flag(pool: &SqlitePool, sql: &str, book_id: i64) -> Result<bool> {
    Ok(sqlx::query_scalar::<_, i64>(sql)
        .bind(book_id)
        .fetch_one(pool)
        .await?
        != 0)
}

impl Storage {
    /// Records a copy; empty free text is stored as `NULL`.
    pub async fn create_copy(&self, new: &NewCopy) -> Result<Copy> {
        let id = insert_copy(&mut *self.pool().acquire().await?, new).await?;
        self.get_copy(id)
            .await?
            .ok_or_else(|| EngineError::Other("copy vanished after insert".into()))
    }

    pub async fn get_copy(&self, id: i64) -> Result<Option<Copy>> {
        let mut conn = self.pool().acquire().await?;
        copy_in(&mut conn, id).await
    }

    pub async fn copies_for_book(&self, book_id: i64) -> Result<Vec<Copy>> {
        let rows = sqlx::query(&format!(
            "SELECT {COPY_COLUMNS} FROM copies WHERE book_id = ? ORDER BY created_at, id"
        ))
        .bind(book_id)
        .fetch_all(self.pool())
        .await?;
        let mut conn = self.pool().acquire().await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let mut copy = row_to_copy(row)?;
            copy.places = places_in(&mut conn, copy.id).await?;
            out.push(copy);
        }
        Ok(out)
    }

    /// Writes the user-editable fields (not `id`, `book_id`, `learned_from`, `created_at` or places);
    /// false if absent.
    pub async fn update_copy(&self, copy: &Copy) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE copies SET form = ?, tenure = ?, have_now = ?, from_whom = ?, app = ?,
                    location = ?, started_at = ?, due_at = ?, ended_at = ?, lent_to = ?,
                    unreadable = ?, note = ?, updated_at = ?
              WHERE id = ?",
        )
        .bind(copy.form.map(|f| f.as_str()))
        .bind(copy.tenure.as_str())
        .bind(copy.have_now)
        .bind(text(&copy.from_whom))
        .bind(text(&copy.app))
        .bind(text(&copy.location))
        .bind(copy.started_at)
        .bind(copy.due_at)
        .bind(copy.ended_at)
        .bind(text(&copy.lent_to))
        .bind(copy.unreadable)
        .bind(text(&copy.note))
        .bind(now_unix())
        .bind(copy.id)
        .execute(self.pool())
        .await?;
        Ok(done.rows_affected() > 0)
    }

    /// Deletes a copy and its places; only an explicit user act calls it.
    pub async fn delete_copy(&self, id: i64) -> Result<bool> {
        let done = sqlx::query("DELETE FROM copies WHERE id = ?")
            .bind(id)
            .execute(self.pool())
            .await?;
        Ok(done.rows_affected() > 0)
    }

    /// The copy holding this place, if any; identity is the place, not the bytes.
    pub async fn copy_for_place(&self, kind: PlaceKind, reference: &str) -> Result<Option<Copy>> {
        let id: Option<i64> =
            sqlx::query_scalar("SELECT copy_id FROM copy_places WHERE kind = ? AND ref = ?")
                .bind(kind.as_str())
                .bind(reference)
                .fetch_optional(self.pool())
                .await?;
        match id {
            Some(id) => self.get_copy(id).await,
            None => Ok(None),
        }
    }

    /// A copy with a place that last saw these exact bytes; dedup by content, below identity-by-place.
    pub async fn copy_for_hash(&self, sha256: &str) -> Result<Option<Copy>> {
        let id: Option<i64> = sqlx::query_scalar(
            "SELECT copy_id FROM copy_places
              WHERE last_hash = ?1 OR (kind = 'store' AND ref = ?1)
              ORDER BY copy_id LIMIT 1",
        )
        .bind(sha256)
        .fetch_optional(self.pool())
        .await?;
        match id {
            Some(id) => self.get_copy(id).await,
            None => Ok(None),
        }
    }

    /// Attaches a place to a copy. If another copy of the same book holds it, that copy is merged in
    /// (`PlaceAttach::Merged`); a different book's copy holding it changes nothing (`OtherBook`).
    pub async fn attach_place(&self, copy_id: i64, place: &NewPlace) -> Result<PlaceAttach> {
        let mut tx = self.pool().begin().await?;
        let outcome = attach_place_in(&mut tx, copy_id, place).await?;
        tx.commit().await?;
        Ok(outcome)
    }

    /// Merges `drop` into `keep`; errors if they belong to different books.
    pub async fn merge_copies(&self, keep: i64, drop: i64) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        merge_copies_in(&mut tx, keep, drop).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Records `last_hash`, `last_mtime` and `last_seen_at` = now; new bytes at a known place are the
    /// same copy.
    /// Leaves `missing_since` alone (see `unmiss_place`). False if there is no such place.
    pub async fn refresh_place(
        &self,
        kind: PlaceKind,
        reference: &str,
        last_hash: Option<&str>,
        last_mtime: Option<i64>,
    ) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE copy_places SET last_hash = ?, last_mtime = ?, last_seen_at = ?
              WHERE kind = ? AND ref = ?",
        )
        .bind(last_hash)
        .bind(last_mtime)
        .bind(now_unix())
        .bind(kind.as_str())
        .bind(reference)
        .execute(self.pool())
        .await?;
        Ok(done.rows_affected() > 0)
    }

    /// Marks a place missing, keeping the earliest `missing_since`; the copy and `have_now` are
    /// unchanged.
    pub async fn mark_place_missing(
        &self,
        kind: PlaceKind,
        reference: &str,
        since: i64,
    ) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE copy_places SET missing_since = ?
              WHERE kind = ? AND ref = ? AND missing_since IS NULL",
        )
        .bind(since)
        .bind(kind.as_str())
        .bind(reference)
        .execute(self.pool())
        .await?;
        Ok(done.rows_affected() > 0)
    }

    pub async fn places_of_kind(&self, kind: PlaceKind) -> Result<Vec<CopyPlace>> {
        let rows = sqlx::query(&format!(
            "SELECT {PLACE_COLUMNS} FROM copy_places WHERE kind = ? ORDER BY ref"
        ))
        .bind(kind.as_str())
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(row_to_place).collect()
    }

    pub async fn set_place_detail(
        &self,
        kind: PlaceKind,
        reference: &str,
        detail: Option<&str>,
    ) -> Result<bool> {
        let done = sqlx::query("UPDATE copy_places SET detail = ? WHERE kind = ? AND ref = ?")
            .bind(detail)
            .bind(kind.as_str())
            .bind(reference)
            .execute(self.pool())
            .await?;
        Ok(done.rows_affected() > 0)
    }

    pub async fn delete_place(&self, kind: PlaceKind, reference: &str) -> Result<bool> {
        let done = sqlx::query("DELETE FROM copy_places WHERE kind = ? AND ref = ?")
            .bind(kind.as_str())
            .bind(reference)
            .execute(self.pool())
            .await?;
        Ok(done.rows_affected() > 0)
    }

    /// Replaces the files a place holds, in one transaction: the origin owns the list. False if no such
    /// place.
    pub async fn set_place_files(
        &self,
        kind: PlaceKind,
        reference: &str,
        files: &[PlaceFile],
    ) -> Result<bool> {
        let mut tx = self.pool().begin().await?;
        let exists: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM copy_places WHERE kind = ? AND ref = ?")
                .bind(kind.as_str())
                .bind(reference)
                .fetch_optional(&mut *tx)
                .await?;
        if exists.is_none() {
            return Ok(false);
        }
        sqlx::query("DELETE FROM copy_place_files WHERE kind = ? AND ref = ?")
            .bind(kind.as_str())
            .bind(reference)
            .execute(&mut *tx)
            .await?;
        for f in files {
            sqlx::query(
                "INSERT OR IGNORE INTO copy_place_files (kind, ref, format, path)
                 VALUES (?, ?, ?, ?)",
            )
            .bind(kind.as_str())
            .bind(reference)
            .bind(f.format.to_lowercase())
            .bind(f.path.display().to_string())
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(true)
    }

    /// Files a place holds, as last recorded; empty for kinds that record none.
    pub async fn place_files(&self, kind: PlaceKind, reference: &str) -> Result<Vec<PlaceFile>> {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT format, path FROM copy_place_files WHERE kind = ? AND ref = ?
              ORDER BY format, path",
        )
        .bind(kind.as_str())
        .bind(reference)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|(format, path)| PlaceFile {
                format,
                path: path.into(),
            })
            .collect())
    }

    /// In one transaction: attaches `place` to `copy_id` as `attach_place` does, deletes the copy's
    /// `path`
    /// places named in `stale`, and refreshes and un-misses a place already its own. Other place kinds
    /// are
    /// untouched; a place held by another book's copy writes nothing (`PlaceAttach::OtherBook`).
    pub async fn relocate_path_place(
        &self,
        copy_id: i64,
        place: &NewPlace,
        stale: &[String],
    ) -> Result<PlaceAttach> {
        let mut tx = self.pool().begin().await?;
        let outcome = attach_place_in(&mut tx, copy_id, place).await?;
        match outcome {
            PlaceAttach::OtherBook { .. } => return Ok(outcome),
            PlaceAttach::Present => {
                sqlx::query(
                    "UPDATE copy_places SET last_hash = ?, last_mtime = ?, last_seen_at = ?,
                            missing_since = NULL
                      WHERE kind = ? AND ref = ?",
                )
                .bind(place.last_hash.as_deref())
                .bind(place.last_mtime)
                .bind(now_unix())
                .bind(place.kind.as_str())
                .bind(&place.reference)
                .execute(&mut *tx)
                .await?;
            }
            PlaceAttach::Added | PlaceAttach::Merged { .. } => {}
        }
        for reference in stale.iter().filter(|r| **r != place.reference) {
            sqlx::query("DELETE FROM copy_places WHERE copy_id = ? AND kind = 'path' AND ref = ?")
                .bind(copy_id)
                .bind(reference)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(outcome)
    }

    pub async fn unmiss_place(&self, kind: PlaceKind, reference: &str) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE copy_places SET missing_since = NULL
              WHERE kind = ? AND ref = ? AND missing_since IS NOT NULL",
        )
        .bind(kind.as_str())
        .bind(reference)
        .execute(self.pool())
        .await?;
        Ok(done.rows_affected() > 0)
    }

    pub async fn in_library(&self, book_id: i64) -> Result<bool> {
        flag(
            self.pool(),
            &format!("SELECT {}", in_library_sql("?1")),
            book_id,
        )
        .await
    }

    pub async fn touched(&self, book_id: i64) -> Result<bool> {
        flag(
            self.pool(),
            &format!("SELECT {}", touched_sql("?1")),
            book_id,
        )
        .await
    }

    /// A finished reading and no copy. The engine does not refuse it (bulk past reads); a frontend asks
    /// the user.
    pub async fn read_without_copy(&self, book_id: i64) -> Result<bool> {
        flag(
            self.pool(),
            &format!(
                "SELECT EXISTS (SELECT 1 FROM readings WHERE book_id = ?1 AND status = '{STATUS_FINISHED}')
                    AND NOT EXISTS (SELECT 1 FROM copies WHERE book_id = ?1)"
            ),
            book_id,
        )
        .await
    }

    pub async fn book_standing(&self, book_id: i64) -> Result<BookStanding> {
        Ok(BookStanding {
            in_library: self.in_library(book_id).await?,
            touched: self.touched(book_id).await?,
            read_without_copy: self.read_without_copy(book_id).await?,
        })
    }

    pub async fn in_library_ids(&self) -> Result<Vec<i64>> {
        Ok(sqlx::query_scalar(&format!(
            "SELECT b.id FROM books b WHERE {}",
            in_library_sql("b.id")
        ))
        .fetch_all(self.pool())
        .await?)
    }

    /// For each in-library book with a copy: `have_now` (any copy not false), `had` (copies, all
    /// false),
    /// `borrowed` (borrowed/rented/subscription copy not false), `copy_not_recorded` (a tenure
    /// `unrecorded` copy).
    pub async fn library_copies(&self) -> Result<LibraryCopies> {
        let rows = sqlx::query(&format!(
            "SELECT c.book_id, c.tenure, c.have_now FROM copies c
              WHERE {} ORDER BY c.book_id, c.created_at, c.id",
            in_library_sql("c.book_id")
        ))
        .fetch_all(self.pool())
        .await?;
        let mut out: Vec<CopyStanding> = Vec::new();
        for row in &rows {
            let book_id: i64 = row.try_get("book_id")?;
            let tenure: Tenure = row.try_get::<String, _>("tenure")?.parse()?;
            let held = row.try_get::<Option<i64>, _>("have_now")? != Some(0);
            if out.last().is_none_or(|s| s.book_id != book_id) {
                out.push(CopyStanding {
                    book_id,
                    have_now: false,
                    had: true,
                    borrowed: false,
                    copy_not_recorded: false,
                    tenure,
                });
            }
            let s = out.last_mut().expect("pushed above");
            if held {
                // The tenure shown is the first copy still had, else the oldest copy.
                if !s.have_now {
                    s.tenure = tenure;
                }
                s.have_now = true;
                s.had = false;
                s.borrowed |= matches!(
                    tenure,
                    Tenure::Borrowed | Tenure::Rented | Tenure::Subscription
                );
            }
            s.copy_not_recorded |= tenure == Tenure::Unrecorded;
        }
        let counts = LibraryCopyCounts {
            have_now: out.iter().filter(|s| s.have_now).count() as i64,
            had: out.iter().filter(|s| s.had).count() as i64,
            borrowed: out.iter().filter(|s| s.borrowed).count() as i64,
            copy_not_recorded: out.iter().filter(|s| s.copy_not_recorded).count() as i64,
        };
        Ok(LibraryCopies { rows: out, counts })
    }
}

pub(super) async fn insert_copy(conn: &mut SqliteConnection, new: &NewCopy) -> Result<i64> {
    let now = now_unix();
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO copies (book_id, form, tenure, have_now, from_whom, app, location,
                             started_at, due_at, ended_at, lent_to, unreadable, note,
                             learned_from, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(new.book_id)
    .bind(new.form.map(|f| f.as_str()))
    .bind(new.tenure.as_str())
    .bind(new.have_now)
    .bind(text(&new.from_whom))
    .bind(text(&new.app))
    .bind(text(&new.location))
    .bind(new.started_at)
    .bind(new.due_at)
    .bind(new.ended_at)
    .bind(text(&new.lent_to))
    .bind(new.unreadable)
    .bind(text(&new.note))
    .bind(new.learned_from.as_str())
    .bind(now)
    .bind(now)
    .fetch_one(&mut *conn)
    .await?;
    tracing::debug!(copy_id = id, book_id = new.book_id, "copy recorded");
    Ok(id)
}
