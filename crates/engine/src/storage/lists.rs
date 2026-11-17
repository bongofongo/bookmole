//! Lists: a book is on a list at most once and positions are exactly `0..n` after every write.
//! Deleting a book must renumber its lists, which the cascade cannot; renumbering parks rows at
//! negative positions first because of the unique `(list_id, position)` index.

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
        "SELECT {BOOK_COLUMNS}, e.position AS e_position, e.note AS e_note, \
         e.added_at AS e_added_at, {YEAR_SQL} AS e_year, \
         CASE WHEN {YEAR_IS_EDITION_SQL} THEN 1 ELSE 0 END AS e_year_is_edition, \
         CASE WHEN {in_lib} THEN 1 ELSE 0 END AS e_in_library \
         {BOOK_FROM} JOIN book_list_entries e ON e.book_id = books.id \
         WHERE e.list_id = ?"
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

/// Two passes: rows park at negative positions first, since the unique `(list_id, position)`
/// index forbids moving a row into a slot another still holds.
async fn renumber(conn: &mut SqliteConnection, list_id: i64, ids: &[i64]) -> Result<()> {
    sqlx::query("UPDATE book_list_entries SET position = -position - 1 WHERE list_id = ?")
        .bind(list_id)
        .execute(&mut *conn)
        .await?;
    for (i, book_id) in ids.iter().enumerate() {
        sqlx::query("UPDATE book_list_entries SET position = ? WHERE list_id = ? AND book_id = ?")
            .bind(i as i64)
            .bind(list_id)
            .bind(book_id)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

async fn entry_order(conn: &mut SqliteConnection, list_id: i64) -> Result<Vec<i64>> {
    Ok(sqlx::query_scalar(
        "SELECT book_id FROM book_list_entries WHERE list_id = ? ORDER BY position",
    )
    .bind(list_id)
    .fetch_all(conn)
    .await?)
}

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
             WHERE e.list_id = ? ORDER BY e.position LIMIT ?"
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
            EntrySort::Yours => "e.position".to_owned(),
            EntrySort::Published => {
                format!("{YEAR_SQL} IS NULL, {YEAR_SQL} ASC, e.position")
            }
            EntrySort::Title => "COALESCE(books.sort_title, books.title) COLLATE NOCASE ASC, \
                 e.position"
                .to_owned(),
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
        let sql = format!("{} AND e.book_id = ?", entry_select());
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
        let next: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(position) + 1, 0) FROM book_list_entries WHERE list_id = ?",
        )
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
        touch(&mut tx, list_id).await?;
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
        touch(&mut tx, list_id).await?;
        tx.commit().await?;
        Ok(())
    }

    pub(crate) async fn lists_for_book(&self, book_id: i64) -> Result<Vec<ListMembership>> {
        let mut conn = self.pool().acquire().await?;
        book_exists(&mut conn, book_id).await?;
        let rows = sqlx::query(
            "SELECT l.id, l.name, e.position, e.note, \
               (SELECT count(*) FROM book_list_entries x WHERE x.list_id = l.id) AS size \
             FROM book_list_entries e JOIN book_lists l ON l.id = e.list_id \
             WHERE e.book_id = ? ORDER BY l.name COLLATE NOCASE ASC, l.id ASC",
        )
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
