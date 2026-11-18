//! Lists: a book is on a list at most once and positions are exactly `0..n` after every write.
//! The database does not enforce that (positions may arrive duplicated or gapped), so reads order
//! by `ENTRY_ORDER` and report a rank, and writes renumber. Deleting a book must renumber its
//! lists, which the cascade cannot.

use sqlx::{Row, SqliteConnection};

use super::books::{BOOK_COLUMNS, BOOK_FROM, row_to_book};
use super::{Storage, in_library_sql, now_unix};
use crate::book::Book;
use crate::error::{EngineError, Result};
use crate::lists::{
    BookList, EntryShow, EntrySort, ListEntry, ListMembership, ListSort, ListSummary, ListTimeline,
    ShelfSuggestion, TIMELINE_MIN_DATED,
};

/// The only place a book's timeline year is decided.
const YEAR_SQL: &str = "COALESCE(books.first_published, books.publish_year)";

const YEAR_IS_EDITION_SQL: &str =
    "(books.first_published IS NULL AND books.publish_year IS NOT NULL)";

const IN_LIBRARY_BOOK: &str = "books.id";

/// The only source whose shelves are offered. Calibre's tags are never suggested.
const SHELF_SOURCE: &str = "goodreads";

/// The one order of a list's entries. Ends in the book so equal positions still read stably.
const ENTRY_ORDER: &str = "e.position, e.added_at, e.book_id";

/// Each entry's place in `ENTRY_ORDER`, 0-based: what `position` holds once the list is compact.
/// Callers narrow it to the lists they read and join it, so the rank always counts a whole list
/// whatever filter the outer query applies.
const RANKED: &str = "SELECT list_id, book_id, \
     row_number() OVER (PARTITION BY list_id ORDER BY position, added_at, book_id) - 1 AS rank \
     FROM book_list_entries";

const LIST_COLUMNS: &str = "id, name, description, source, source_ref, created_at, updated_at";

fn blank_to_none(s: Option<&str>) -> Option<String> {
    s.filter(|s| !s.trim().is_empty()).map(str::to_owned)
}

fn clean_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(EngineError::InvalidInput("a list needs a name".into()));
    }
    Ok(name.to_owned())
}

fn row_to_list(row: &sqlx::sqlite::SqliteRow) -> Result<BookList> {
    Ok(BookList {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        description: row.try_get("description")?,
        source: row.try_get("source")?,
        source_ref: row.try_get("source_ref")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn row_to_entry(row: &sqlx::sqlite::SqliteRow) -> Result<ListEntry> {
    Ok(ListEntry {
        book: row_to_book(row)?,
        position: row.try_get("e_position")?,
        note: row.try_get("e_note")?,
        added_at: row.try_get("e_added_at")?,
        year: row.try_get("e_year")?,
        year_is_edition: row.try_get::<i64, _>("e_year_is_edition")? != 0,
        in_library: row.try_get::<i64, _>("e_in_library")? != 0,
    })
}

fn entry_select() -> String {
    let in_lib = in_library_sql(IN_LIBRARY_BOOK);
    format!(
        "WITH r AS ({RANKED} WHERE list_id = ?1) \
         SELECT {BOOK_COLUMNS}, r.rank AS e_position, e.note AS e_note, \
         e.added_at AS e_added_at, {YEAR_SQL} AS e_year, \
         CASE WHEN {YEAR_IS_EDITION_SQL} THEN 1 ELSE 0 END AS e_year_is_edition, \
         CASE WHEN {in_lib} THEN 1 ELSE 0 END AS e_in_library \
         {BOOK_FROM} JOIN book_list_entries e ON e.book_id = books.id \
         JOIN r ON r.book_id = e.book_id \
         WHERE e.list_id = ?1"
    )
}

async fn list_exists(conn: &mut SqliteConnection, id: i64) -> Result<()> {
    let found: Option<i64> = sqlx::query_scalar("SELECT id FROM book_lists WHERE id = ?")
        .bind(id)
        .fetch_optional(conn)
        .await?;
    found
        .map(|_| ())
        .ok_or_else(|| EngineError::NotFound(format!("list id {id}")))
}

async fn book_exists(conn: &mut SqliteConnection, id: i64) -> Result<()> {
    let found: Option<i64> = sqlx::query_scalar("SELECT id FROM books WHERE id = ?")
        .bind(id)
        .fetch_optional(conn)
        .await?;
    found
        .map(|_| ())
        .ok_or_else(|| EngineError::NotFound(format!("book id {id}")))
}

async fn touch(conn: &mut SqliteConnection, list_id: i64) -> Result<()> {
    sqlx::query("UPDATE book_lists SET updated_at = ? WHERE id = ?")
        .bind(now_unix())
        .bind(list_id)
        .execute(conn)
        .await?;
    Ok(())
}

/// `ids` must be every entry of the list, in its new order. A row already in its place is not
/// written.
async fn renumber(conn: &mut SqliteConnection, list_id: i64, ids: &[i64]) -> Result<()> {
    for (i, book_id) in ids.iter().enumerate() {
        sqlx::query(
            "UPDATE book_list_entries SET position = ?1
              WHERE list_id = ?2 AND book_id = ?3 AND position IS NOT ?1",
        )
        .bind(i as i64)
        .bind(list_id)
        .bind(book_id)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

async fn entry_order(conn: &mut SqliteConnection, list_id: i64) -> Result<Vec<i64>> {
    let sql =
        format!("SELECT book_id FROM book_list_entries e WHERE list_id = ? ORDER BY {ENTRY_ORDER}");
    Ok(sqlx::query_scalar(&sql)
        .bind(list_id)
        .fetch_all(conn)
        .await?)
}

/// Restores positions to exactly `0..n` in `ENTRY_ORDER`, repairing duplicates and gaps.
async fn compact(conn: &mut SqliteConnection, list_id: i64) -> Result<()> {
    let ids = entry_order(&mut *conn, list_id).await?;
    renumber(&mut *conn, list_id, &ids).await?;
    touch(conn, list_id).await
}

/// Read before the delete; the cascade leaves nothing to ask afterwards.
pub(super) async fn lists_of_book(conn: &mut SqliteConnection, book_id: i64) -> Result<Vec<i64>> {
    Ok(
        sqlx::query_scalar("SELECT list_id FROM book_list_entries WHERE book_id = ?")
            .bind(book_id)
            .fetch_all(conn)
            .await?,
    )
}

pub(super) async fn compact_lists(conn: &mut SqliteConnection, list_ids: &[i64]) -> Result<()> {
    for id in list_ids {
        compact(&mut *conn, *id).await?;
    }
    Ok(())
}

/// On a list holding both, the earlier position survives, notes are joined, earlier `added_at`
/// kept.
pub(super) async fn repoint_entries(conn: &mut SqliteConnection, src: i64, dst: i64) -> Result<()> {
    let theirs = sqlx::query(
        "SELECT list_id, position, note, added_at FROM book_list_entries WHERE book_id = ?",
    )
    .bind(src)
    .fetch_all(&mut *conn)
    .await?;
    let mut touched = Vec::new();
    for row in &theirs {
        let list_id: i64 = row.try_get("list_id")?;
        let s_pos: i64 = row.try_get("position")?;
        let s_note: Option<String> = row.try_get("note")?;
        let s_added: i64 = row.try_get("added_at")?;
        touched.push(list_id);
        let mine = sqlx::query(
            "SELECT position, note, added_at FROM book_list_entries \
             WHERE list_id = ? AND book_id = ?",
        )
        .bind(list_id)
        .bind(dst)
        .fetch_optional(&mut *conn)
        .await?;
        let Some(mine) = mine else {
            sqlx::query(
                "UPDATE book_list_entries SET book_id = ? WHERE list_id = ? AND book_id = ?",
            )
            .bind(dst)
            .bind(list_id)
            .bind(src)
            .execute(&mut *conn)
            .await?;
            continue;
        };
        let d_pos: i64 = mine.try_get("position")?;
        let d_note: Option<String> = mine.try_get("note")?;
        let d_added: i64 = mine.try_get("added_at")?;
        let (first, second) = if s_pos < d_pos {
            (s_note, d_note)
        } else {
            (d_note, s_note)
        };
        let note = match (first, second) {
            (Some(a), Some(b)) => Some(format!("{a}\n\n{b}")),
            (a, b) => a.or(b),
        };
        // `src`'s row goes first so its position is free for `dst` to take.
        sqlx::query("DELETE FROM book_list_entries WHERE list_id = ? AND book_id = ?")
            .bind(list_id)
            .bind(src)
            .execute(&mut *conn)
            .await?;
        sqlx::query(
            "UPDATE book_list_entries SET position = ?, note = ?, added_at = ? \
             WHERE list_id = ? AND book_id = ?",
        )
        .bind(s_pos.min(d_pos))
        .bind(note)
        .bind(s_added.min(d_added))
        .bind(list_id)
        .bind(dst)
        .execute(&mut *conn)
        .await?;
    }
    compact_lists(conn, &touched).await
}

impl Storage {
    pub(crate) async fn create_list(
        &self,
        name: &str,
        description: Option<&str>,
    ) -> Result<BookList> {
        let name = clean_name(name)?;
        let now = now_unix();
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO book_lists (name, description, created_at, updated_at) \
             VALUES (?, ?, ?, ?) RETURNING id",
        )
        .bind(name)
        .bind(blank_to_none(description))
        .bind(now)
        .bind(now)
        .fetch_one(self.pool())
        .await?;
        self.get_list(id).await
    }

    pub(crate) async fn get_list(&self, id: i64) -> Result<BookList> {
        let sql = format!("SELECT {LIST_COLUMNS} FROM book_lists WHERE id = ?");
        let row = sqlx::query(&sql)
            .bind(id)
            .fetch_optional(self.pool())
            .await?;
        match row {
            Some(r) => row_to_list(&r),
            None => Err(EngineError::NotFound(format!("list id {id}"))),
        }
    }

    pub(crate) async fn update_list(
        &self,
        id: i64,
        name: &str,
        description: Option<&str>,
    ) -> Result<BookList> {
        let name = clean_name(name)?;
        let done = sqlx::query(
            "UPDATE book_lists SET name = ?, description = ?, updated_at = ? WHERE id = ?",
        )
        .bind(name)
        .bind(blank_to_none(description))
        .bind(now_unix())
        .bind(id)
        .execute(self.pool())
        .await?;
        if done.rows_affected() == 0 {
            return Err(EngineError::NotFound(format!("list id {id}")));
        }
        self.get_list(id).await
    }

    pub(crate) async fn delete_list(&self, id: i64) -> Result<()> {
        let done = sqlx::query("DELETE FROM book_lists WHERE id = ?")
            .bind(id)
            .execute(self.pool())
            .await?;
        if done.rows_affected() == 0 {
            return Err(EngineError::NotFound(format!("list id {id}")));
        }
        Ok(())
    }

    pub(crate) async fn list_summaries(
        &self,
        sort: ListSort,
        preview: usize,
    ) -> Result<Vec<ListSummary>> {
        let order = match sort {
            ListSort::Name => "name COLLATE NOCASE ASC, id ASC",
            ListSort::RecentlyChanged => "updated_at DESC, id DESC",
        };
        let in_lib = in_library_sql(IN_LIBRARY_BOOK);
        let sql = format!(
            "SELECT {LIST_COLUMNS}, \
               (SELECT count(*) FROM book_list_entries e WHERE e.list_id = book_lists.id) AS n, \
               (SELECT count(*) FROM book_list_entries e JOIN books ON books.id = e.book_id \
                 WHERE e.list_id = book_lists.id AND NOT ({in_lib})) AS n_out, \
               (SELECT count(*) FROM book_list_entries e JOIN books ON books.id = e.book_id \
                 WHERE e.list_id = book_lists.id AND ({in_lib})) AS n_in \
             FROM book_lists ORDER BY {order}"
        );
        let rows = sqlx::query(&sql).fetch_all(self.pool()).await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let list = row_to_list(row)?;
            let faces = self.list_faces(list.id, preview).await?;
            out.push(ListSummary {
                book_count: row.try_get("n")?,
                not_in_library: row.try_get("n_out")?,
                in_library: row.try_get("n_in")?,
                preview: faces,
                list,
            });
        }
        Ok(out)
    }

    async fn list_faces(&self, list_id: i64, n: usize) -> Result<Vec<Book>> {
        let sql = format!(
            "SELECT {BOOK_COLUMNS} {BOOK_FROM} \
             JOIN book_list_entries e ON e.book_id = books.id \
             WHERE e.list_id = ? ORDER BY {ENTRY_ORDER} LIMIT ?"
        );
        let rows = sqlx::query(&sql)
            .bind(list_id)
            .bind(n as i64)
            .fetch_all(self.pool())
            .await?;
        rows.iter().map(row_to_book).collect()
    }

    /// `sort` is a view: it never rewrites `position`.
    pub(crate) async fn list_entries(
        &self,
        list_id: i64,
        show: EntryShow,
        sort: EntrySort,
    ) -> Result<Vec<ListEntry>> {
        let mut conn = self.pool().acquire().await?;
        list_exists(&mut conn, list_id).await?;
        let filter = match show {
            EntryShow::All => String::new(),
            EntryShow::InLibrary => format!(" AND ({})", in_library_sql(IN_LIBRARY_BOOK)),
            EntryShow::NotInLibrary => format!(" AND NOT ({})", in_library_sql(IN_LIBRARY_BOOK)),
        };
        let order = match sort {
            EntrySort::Yours => ENTRY_ORDER.to_owned(),
            EntrySort::Published => {
                format!("{YEAR_SQL} IS NULL, {YEAR_SQL} ASC, {ENTRY_ORDER}")
            }
            EntrySort::Title => {
                format!("COALESCE(books.sort_title, books.title) COLLATE NOCASE ASC, {ENTRY_ORDER}")
            }
        };
        let sql = format!("{}{filter} ORDER BY {order}", entry_select());
        let rows = sqlx::query(&sql)
            .bind(list_id)
            .fetch_all(&mut *conn)
            .await?;
        rows.iter().map(row_to_entry).collect()
    }

    async fn get_entry(
        conn: &mut SqliteConnection,
        list_id: i64,
        book_id: i64,
    ) -> Result<Option<ListEntry>> {
        let sql = format!("{} AND e.book_id = ?2", entry_select());
        let row = sqlx::query(&sql)
            .bind(list_id)
            .bind(book_id)
            .fetch_optional(conn)
            .await?;
        row.as_ref().map(row_to_entry).transpose()
    }

    /// Idempotent: a book already on the list comes back unchanged, its note kept, the list unstamped.
    pub(crate) async fn add_to_list(
        &self,
        list_id: i64,
        book_id: i64,
        note: Option<&str>,
    ) -> Result<ListEntry> {
        let mut tx = self.pool().begin().await?;
        list_exists(&mut tx, list_id).await?;
        book_exists(&mut tx, book_id).await?;
        if let Some(existing) = Self::get_entry(&mut tx, list_id, book_id).await? {
            return Ok(existing);
        }
        // Compact first, so the count is the next free place and no stored value is added to.
        compact(&mut tx, list_id).await?;
        let next: i64 =
            sqlx::query_scalar("SELECT count(*) FROM book_list_entries WHERE list_id = ?")
                .bind(list_id)
                .fetch_one(&mut *tx)
                .await?;
        sqlx::query(
            "INSERT INTO book_list_entries (list_id, book_id, position, note, added_at) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(list_id)
        .bind(book_id)
        .bind(next)
        .bind(blank_to_none(note))
        .bind(now_unix())
        .execute(&mut *tx)
        .await?;
        let entry = Self::get_entry(&mut tx, list_id, book_id).await?;
        tx.commit().await?;
        entry.ok_or_else(|| EngineError::NotFound(format!("book id {book_id}")))
    }

    pub(crate) async fn remove_from_list(&self, list_id: i64, book_id: i64) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        list_exists(&mut tx, list_id).await?;
        book_exists(&mut tx, book_id).await?;
        let done = sqlx::query("DELETE FROM book_list_entries WHERE list_id = ? AND book_id = ?")
            .bind(list_id)
            .bind(book_id)
            .execute(&mut *tx)
            .await?;
        if done.rows_affected() > 0 {
            compact(&mut tx, list_id).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// `to` is clamped to the last place; the book must be on the list.
    pub(crate) async fn move_in_list(&self, list_id: i64, book_id: i64, to: usize) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        list_exists(&mut tx, list_id).await?;
        let mut ids = entry_order(&mut tx, list_id).await?;
        let Some(from) = ids.iter().position(|b| *b == book_id) else {
            return Err(EngineError::NotFound(format!(
                "book id {book_id} on list id {list_id}"
            )));
        };
        ids.remove(from);
        ids.insert(to.min(ids.len()), book_id);
        renumber(&mut tx, list_id, &ids).await?;
        touch(&mut tx, list_id).await?;
        tx.commit().await?;
        Ok(())
    }

    pub(crate) async fn set_list_note(
        &self,
        list_id: i64,
        book_id: i64,
        note: Option<&str>,
    ) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        list_exists(&mut tx, list_id).await?;
        let done =
            sqlx::query("UPDATE book_list_entries SET note = ? WHERE list_id = ? AND book_id = ?")
                .bind(blank_to_none(note))
                .bind(list_id)
                .bind(book_id)
                .execute(&mut *tx)
                .await?;
        if done.rows_affected() == 0 {
            return Err(EngineError::NotFound(format!(
                "book id {book_id} on list id {list_id}"
            )));
        }
        compact(&mut tx, list_id).await?;
        tx.commit().await?;
        Ok(())
    }

    pub(crate) async fn lists_for_book(&self, book_id: i64) -> Result<Vec<ListMembership>> {
        let mut conn = self.pool().acquire().await?;
        book_exists(&mut conn, book_id).await?;
        let sql = format!(
            "WITH r AS ({RANKED} WHERE list_id IN \
                 (SELECT list_id FROM book_list_entries WHERE book_id = ?1)) \
             SELECT l.id, l.name, r.rank AS position, e.note, \
               (SELECT count(*) FROM book_list_entries x WHERE x.list_id = l.id) AS size \
             FROM book_list_entries e JOIN book_lists l ON l.id = e.list_id \
             JOIN r ON r.list_id = e.list_id AND r.book_id = e.book_id \
             WHERE e.book_id = ?1 ORDER BY l.name COLLATE NOCASE ASC, l.id ASC"
        );
        let rows = sqlx::query(&sql)
            .bind(book_id)
            .fetch_all(&mut *conn)
            .await?;
        rows.iter()
            .map(|r| {
                Ok(ListMembership {
                    list_id: r.try_get("id")?,
                    name: r.try_get("name")?,
                    position: r.try_get::<i64, _>("position")? + 1,
                    list_size: r.try_get("size")?,
                    note: r.try_get("note")?,
                })
            })
            .collect()
    }

    /// Dated books then undated, each in list order; `ready` once `TIMELINE_MIN_DATED` have a year.
    pub(crate) async fn list_timeline(&self, list_id: i64) -> Result<ListTimeline> {
        let entries = self
            .list_entries(list_id, EntryShow::All, EntrySort::Yours)
            .await?;
        let (dated, undated): (Vec<_>, Vec<_>) =
            entries.into_iter().partition(|e| e.year.is_some());
        Ok(ListTimeline {
            ready: dated.len() >= TIMELINE_MIN_DATED,
            dated,
            undated,
        })
    }

    pub(crate) async fn shelf_suggestions(&self, preview: usize) -> Result<Vec<ShelfSuggestion>> {
        let rows = sqlx::query(
            "SELECT bt.tag AS tag, count(*) AS n FROM book_tags bt \
             WHERE bt.source = ? \
               AND NOT EXISTS (SELECT 1 FROM book_lists l \
                                WHERE l.source = bt.source AND l.source_ref = bt.tag) \
               AND NOT EXISTS (SELECT 1 FROM dismissed_shelves d \
                                WHERE d.source = bt.source AND d.tag = bt.tag) \
             GROUP BY bt.tag ORDER BY n DESC, bt.tag ASC",
        )
        .bind(SHELF_SOURCE)
        .fetch_all(self.pool())
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let tag: String = row.try_get("tag")?;
            let faces = self.shelf_books(&tag, Some(preview)).await?;
            out.push(ShelfSuggestion {
                source: SHELF_SOURCE.to_owned(),
                name: self.shelf_name(&tag).await?,
                book_count: row.try_get("n")?,
                preview: faces,
                tag,
            });
        }
        Ok(out)
    }

    async fn shelf_name(&self, tag: &str) -> Result<String> {
        let raw: Option<String> = sqlx::query_scalar(
            "SELECT raw FROM book_tags WHERE source = ? AND tag = ? \
               AND raw IS NOT NULL AND trim(raw) <> '' ORDER BY rowid LIMIT 1",
        )
        .bind(SHELF_SOURCE)
        .bind(tag)
        .fetch_optional(self.pool())
        .await?;
        Ok(raw.unwrap_or_else(|| tag.to_owned()))
    }

    /// CSV order via rowid (`book_tags` is written row by row on import); title breaks ties.
    async fn shelf_books(&self, tag: &str, limit: Option<usize>) -> Result<Vec<Book>> {
        let sql = format!(
            "SELECT {BOOK_COLUMNS} {BOOK_FROM} \
             JOIN book_tags bt ON bt.book_id = books.id \
             WHERE bt.source = ? AND bt.tag = ? \
             ORDER BY bt.rowid ASC, COALESCE(books.sort_title, books.title) COLLATE NOCASE, \
                      books.id LIMIT ?"
        );
        let rows = sqlx::query(&sql)
            .bind(SHELF_SOURCE)
            .bind(tag)
            .bind(limit.map_or(-1, |n| n as i64))
            .fetch_all(self.pool())
            .await?;
        rows.iter().map(row_to_book).collect()
    }

    /// Holds a copy of the shelf's books; keeping twice returns the first list. Only `goodreads`.
    pub(crate) async fn keep_shelf(&self, source: &str, tag: &str) -> Result<BookList> {
        if source != SHELF_SOURCE {
            return Err(EngineError::InvalidInput(format!(
                "no shelves are offered from {source:?}"
            )));
        }
        let kept: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM book_lists WHERE source = ? AND source_ref = ? ORDER BY id LIMIT 1",
        )
        .bind(source)
        .bind(tag)
        .fetch_optional(self.pool())
        .await?;
        if let Some(id) = kept {
            return self.get_list(id).await;
        }
        let books = self.shelf_books(tag, None).await?;
        if books.is_empty() {
            return Err(EngineError::NotFound(format!("shelf {tag:?}")));
        }
        let name = self.shelf_name(tag).await?;
        let now = now_unix();
        let mut tx = self.pool().begin().await?;
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO book_lists (name, source, source_ref, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(clean_name(&name).unwrap_or_else(|_| tag.to_owned()))
        .bind(source)
        .bind(tag)
        .bind(now)
        .bind(now)
        .fetch_one(&mut *tx)
        .await?;
        for (i, book) in books.iter().enumerate() {
            sqlx::query(
                "INSERT INTO book_list_entries (list_id, book_id, position, added_at) \
                 VALUES (?, ?, ?, ?)",
            )
            .bind(id)
            .bind(book.id)
            .bind(i as i64)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        self.get_list(id).await
    }

    /// Idempotent; the tag need not exist.
    pub(crate) async fn dismiss_shelf(&self, source: &str, tag: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO dismissed_shelves (source, tag, dismissed_at) VALUES (?, ?, ?) \
             ON CONFLICT (source, tag) DO NOTHING",
        )
        .bind(source)
        .bind(tag)
        .bind(now_unix())
        .execute(self.pool())
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn rows(s: &Storage, list: i64) -> Vec<(i64, i64)> {
        sqlx::query_as(
            "SELECT book_id, position FROM book_list_entries WHERE list_id = ? ORDER BY book_id",
        )
        .bind(list)
        .fetch_all(s.pool())
        .await
        .unwrap()
    }

    async fn read_order(s: &Storage, list: i64) -> Vec<(i64, i64)> {
        s.list_entries(list, EntryShow::All, EntrySort::Yours)
            .await
            .unwrap()
            .iter()
            .map(|e| (e.book.id.unwrap(), e.position))
            .collect()
    }

    /// Positions as two copies of a library combined could leave them: duplicated and gapped.
    async fn scrambled() -> (Storage, i64, Vec<i64>) {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let list = s.create_list("To read", None).await.unwrap().id;
        let mut books = Vec::new();
        for title in [
            "Middlemarch",
            "Silas Marner",
            "Adam Bede",
            "Romola",
            "Felix Holt",
        ] {
            let b = Book {
                title: Some(title.into()),
                ..Default::default()
            };
            let id = s.upsert_book(&b, None).await.unwrap();
            s.add_to_list(list, id, None).await.unwrap();
            books.push(id);
        }
        // (position, added_at) per book: two ties on position, one broken by added_at and one
        // only by the book id, and gaps at 1, 2 and 4.
        for (book, (pos, added)) in books
            .iter()
            .zip([(3, 10), (0, 20), (3, 5), (0, 20), (9, 1)])
        {
            sqlx::query(
                "UPDATE book_list_entries SET position = ?, added_at = ?
                  WHERE list_id = ? AND book_id = ?",
            )
            .bind(pos)
            .bind(added)
            .bind(list)
            .bind(book)
            .execute(s.pool())
            .await
            .unwrap();
        }
        (s, list, books)
    }

    #[tokio::test]
    async fn duplicate_and_gapped_positions_read_in_one_stable_order() {
        let (s, list, b) = scrambled().await;
        let want = vec![(b[1], 0), (b[3], 1), (b[2], 2), (b[0], 3), (b[4], 4)];
        assert_eq!(read_order(&s, list).await, want);
        assert_eq!(read_order(&s, list).await, want, "the same order twice");

        let faces: Vec<i64> = s
            .list_faces(list, 2)
            .await
            .unwrap()
            .iter()
            .map(|b| b.id.unwrap())
            .collect();
        assert_eq!(faces, vec![b[1], b[3]]);
        let on = s.lists_for_book(b[0]).await.unwrap();
        assert_eq!(on[0].position, 4, "a rank, not the stored 3");
        assert!(
            rows(&s, list).await.iter().any(|&(_, p)| p == 9),
            "reading repairs nothing"
        );
    }

    #[tokio::test]
    async fn compacting_restores_zero_to_n_in_the_order_read() {
        let (s, list, b) = scrambled().await;
        let read = read_order(&s, list).await;
        let mut conn = s.pool().acquire().await.unwrap();
        compact(&mut conn, list).await.unwrap();
        drop(conn);
        let mut stored = rows(&s, list).await;
        stored.sort_by_key(|&(_, p)| p);
        assert_eq!(stored, read, "stored positions are now the ranks");
        assert_eq!(stored[0].0, b[1]);
    }

    #[tokio::test]
    async fn every_write_leaves_a_scrambled_list_dense() {
        let dense = |r: Vec<(i64, i64)>| {
            let mut p: Vec<i64> = r.iter().map(|&(_, p)| p).collect();
            p.sort_unstable();
            p == (0..r.len() as i64).collect::<Vec<_>>()
        };

        let (s, list, b) = scrambled().await;
        s.move_in_list(list, b[4], 0).await.unwrap();
        assert!(dense(rows(&s, list).await));
        assert_eq!(read_order(&s, list).await[0].0, b[4]);

        let (s, list, b) = scrambled().await;
        s.remove_from_list(list, b[2]).await.unwrap();
        assert!(dense(rows(&s, list).await));

        let (s, list, _) = scrambled().await;
        let extra = s
            .upsert_book(
                &Book {
                    title: Some("Daniel Deronda".into()),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
        let added = s.add_to_list(list, extra, None).await.unwrap();
        assert!(dense(rows(&s, list).await));
        assert_eq!(added.position, 5, "appended last");

        let (s, list, b) = scrambled().await;
        s.set_list_note(list, b[0], Some("read this first"))
            .await
            .unwrap();
        assert!(dense(rows(&s, list).await));
    }

    /// `MAX(position) + 1` would overflow into a REAL here.
    #[tokio::test]
    async fn adding_after_the_largest_position_appends() {
        let (s, list, b) = scrambled().await;
        sqlx::query("UPDATE book_list_entries SET position = ? WHERE book_id = ?")
            .bind(i64::MAX)
            .bind(b[4])
            .execute(s.pool())
            .await
            .unwrap();
        let extra = s
            .upsert_book(
                &Book {
                    title: Some("Daniel Deronda".into()),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
        let added = s.add_to_list(list, extra, None).await.unwrap();
        assert_eq!(added.position, 5);
        let mut stored = rows(&s, list).await;
        stored.sort_by_key(|&(_, p)| p);
        assert_eq!(
            stored.iter().map(|&(_, p)| p).collect::<Vec<_>>(),
            (0..6).collect::<Vec<_>>()
        );
        assert_eq!(stored[4].0, b[4], "the largest stays before the new entry");
    }

    /// Ranks are one window over the list. A correlated count per entry took seconds here.
    #[tokio::test]
    async fn a_long_list_reads_quickly_and_ranks_count_the_whole_list() {
        const N: i64 = 4_000;
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let list = s.create_list("Everything", None).await.unwrap().id;
        let mut tx = s.pool().begin().await.unwrap();
        for i in 0..N {
            let id: i64 = sqlx::query_scalar(
                "INSERT INTO books (title, created_at, last_modified) VALUES (?, 0, 0) RETURNING id",
            )
            .bind(format!("Book {i}"))
            .fetch_one(&mut *tx)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO book_list_entries (list_id, book_id, position, added_at)
                 VALUES (?, ?, ?, 0)",
            )
            .bind(list)
            .bind(id)
            .bind(N - 1 - i)
            .execute(&mut *tx)
            .await
            .unwrap();
        }
        tx.commit().await.unwrap();

        let started = std::time::Instant::now();
        for sort in [EntrySort::Yours, EntrySort::Title, EntrySort::Published] {
            let read = s.list_entries(list, EntryShow::All, sort).await.unwrap();
            assert_eq!(read.len() as i64, N);
        }
        let yours = read_order(&s, list).await;
        let ranks: Vec<i64> = yours.iter().map(|&(_, p)| p).collect();
        assert_eq!(ranks, (0..N).collect::<Vec<_>>());
        let last = yours.last().unwrap().0;
        assert_eq!(s.lists_for_book(last).await.unwrap()[0].position, N);
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "{N} entries took {elapsed:?}"
        );

        // One book in the library: the filter leaves one entry, ranked within the whole list.
        let book_1: i64 = sqlx::query_scalar("SELECT id FROM books WHERE title = 'Book 1'")
            .fetch_one(s.pool())
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO copies (book_id, tenure, learned_from, created_at, updated_at)
             VALUES (?, 'own', 'user', 0, 0)",
        )
        .bind(book_1)
        .execute(s.pool())
        .await
        .unwrap();
        let held = s
            .list_entries(list, EntryShow::InLibrary, EntrySort::Yours)
            .await
            .unwrap();
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].position, N - 2);
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use proptest::prelude::*;

    #[derive(Debug, Clone)]
    enum Op {
        Add(usize),
        Remove(usize),
        Move(usize, usize),
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            (0..6usize).prop_map(Op::Add),
            (0..6usize).prop_map(Op::Remove),
            (0..6usize, 0..8usize).prop_map(|(b, to)| Op::Move(b, to)),
        ]
    }

    proptest! {
        #[test]
        fn any_sequence_leaves_positions_exactly_zero_to_n(
            ops in proptest::collection::vec(op(), 0..40)
        ) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let s = Storage::connect("sqlite::memory:").await.unwrap();
                let mut books = Vec::new();
                for i in 0..6 {
                    let b = Book { title: Some(format!("B{i}")), ..Default::default() };
                    books.push(s.upsert_book(&b, None).await.unwrap());
                }
                let list = s.create_list("L", None).await.unwrap().id;
                let mut model: Vec<i64> = Vec::new();
                for o in &ops {
                    match o {
                        Op::Add(b) => {
                            s.add_to_list(list, books[*b], None).await.unwrap();
                            if !model.contains(&books[*b]) { model.push(books[*b]); }
                        }
                        Op::Remove(b) => {
                            s.remove_from_list(list, books[*b]).await.unwrap();
                            model.retain(|x| *x != books[*b]);
                        }
                        Op::Move(b, to) => {
                            let r = s.move_in_list(list, books[*b], *to).await;
                            if let Some(from) = model.iter().position(|x| *x == books[*b]) {
                                r.unwrap();
                                model.remove(from);
                                model.insert((*to).min(model.len()), books[*b]);
                            } else {
                                prop_assert!(matches!(r, Err(EngineError::NotFound(_))));
                            }
                        }
                    }
                    let rows: Vec<(i64, i64)> = sqlx::query_as(
                        "SELECT position, book_id FROM book_list_entries \
                         WHERE list_id = ? ORDER BY position",
                    )
                    .bind(list)
                    .fetch_all(s.pool())
                    .await
                    .unwrap();
                    let positions: Vec<i64> = rows.iter().map(|r| r.0).collect();
                    prop_assert_eq!(positions, (0..rows.len() as i64).collect::<Vec<_>>());
                    let order: Vec<i64> = rows.iter().map(|r| r.1).collect();
                    prop_assert_eq!(order, model.clone());
                }
                Ok(())
            })?;
        }
    }
}
