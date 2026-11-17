//! The one migration in this repo that destroys data.
//!
//! `0005_readings.sql` back-fills `readings` from `books` and then drops the
//! four progress columns. Everything else in the suite connects a *fully*
//! migrated database, which can never exercise the back-fill: by the time the
//! first row is written the columns it reads are already gone. So these tests
//! apply the migrations in two halves, write a pre-`0005` library in between,
//! and assert what survived.

use sqlx::migrate::{Migrate, Migrator};
use sqlx::sqlite::SqliteConnection;
use sqlx::{Connection, Row};

static MIGRATIONS: Migrator = sqlx::migrate!("./migrations");

const READINGS: i64 = 5;
const REFLECTION: i64 = 7;
const NOTE_LINK_INDEX: i64 = 8;
const GOODREADS: i64 = 9;
const SORT_KEY_INDEXES: i64 = 16;
const MOMENTS: i64 = 17;

/// A connection migrated up to (but not including) `version`.
async fn migrated_below(version: i64) -> SqliteConnection {
    let mut conn = SqliteConnection::connect("sqlite::memory:")
        .await
        .expect("open in-memory db");
    conn.ensure_migrations_table()
        .await
        .expect("migrations table");
    for m in MIGRATIONS.iter().filter(|m| m.version < version) {
        conn.apply(m).await.expect("apply migration");
    }
    conn
}

async fn apply(conn: &mut SqliteConnection, version: i64) {
    for m in MIGRATIONS.iter().filter(|m| m.version == version) {
        conn.apply(m).await.expect("apply migration");
    }
}

async fn before_readings() -> SqliteConnection {
    migrated_below(READINGS).await
}

async fn apply_readings(conn: &mut SqliteConnection) {
    apply(conn, READINGS).await;
}

/// Insert a pre-`0005` book straight through the old columns.
async fn old_book(
    conn: &mut SqliteConnection,
    title: &str,
    current_page: Option<i64>,
    finished: i64,
    date_started: Option<i64>,
    date_finished: Option<i64>,
) -> i64 {
    sqlx::query(
        "INSERT INTO books (title, current_page, finished, date_started, date_finished,
                            created_at, last_modified)
         VALUES (?, ?, ?, ?, ?, 0, 0) RETURNING id",
    )
    .bind(title)
    .bind(current_page)
    .bind(finished)
    .bind(date_started)
    .bind(date_finished)
    .fetch_one(&mut *conn)
    .await
    .expect("insert old book")
    .get("id")
}

/// Version numbers are a cross-branch resource, and this is the only thing that
/// notices when two of them collide.
///
/// Threads run in parallel, each owning a migration, and each picks its number
/// from what is on `main` when it starts. Two threads that both pick `0008` do
/// not conflict in git — the filenames differ past the number — so the collision
/// arrives as a second migration sharing a version, which sqlx applies in
/// whatever order the filenames sort in and records under one version. The
/// symptom lands much later and somewhere else.
///
/// Contiguity rather than mere uniqueness, because a *gap* is the same mistake
/// caught earlier: it means a number was claimed by a branch that has not merged
/// yet, and merging out of numeric order is what the build order forbids.
///
/// One caveat, and it is why this cannot be the *only* guard: `sqlx::migrate!`
/// is a compile-time macro, and dropping a new `.sql` into the directory does
/// not on its own invalidate this test binary — so locally the check can appear
/// to pass until something else forces a rebuild. CI always compiles from a
/// fresh checkout, and the `migrations` job in `ci.yml` catches the other half
/// (an *edited* migration) straight from the diff.
#[test]
fn migration_versions_are_contiguous_from_one() {
    let versions: Vec<i64> = MIGRATIONS.iter().map(|m| m.version).collect();

    assert!(
        !versions.is_empty(),
        "no migrations found — sqlx::migrate! resolved an empty directory"
    );

    let expected: Vec<i64> = (1..=versions.len() as i64).collect();
    assert_eq!(
        versions,
        expected,
        "migration versions must be contiguous 1..={}, got {versions:?}. \
         A duplicate means two branches claimed the same number; a gap means one \
         claimed a number and has not merged.",
        versions.len()
    );
}

#[tokio::test]
async fn the_backfill_round_trips_every_shape_of_progress() {
    let mut conn = before_readings().await;

    let part = old_book(&mut conn, "part read", Some(120), 0, Some(1_000), None).await;
    let done = old_book(&mut conn, "finished", Some(490), 1, Some(1), Some(2)).await;
    // Finished with no recorded end date: the back-fill leaves `finished_at`
    // NULL, so this reading counts as *open* while its status says finished.
    // That is deliberate — inventing a date would be worse than an open reading
    // the user can close.
    let dateless = old_book(&mut conn, "finished, undated", None, 1, None, None).await;
    let untouched = old_book(&mut conn, "never opened", None, 0, None, None).await;

    apply_readings(&mut conn).await;

    let rows = sqlx::query(
        "SELECT book_id, started_at, finished_at, status, source, current_page
         FROM readings ORDER BY book_id",
    )
    .fetch_all(&mut conn)
    .await
    .expect("select readings");

    assert_eq!(rows.len(), 3, "a book with no progress gets no reading");
    let by_book: Vec<i64> = rows.iter().map(|r| r.get("book_id")).collect();
    assert_eq!(by_book, vec![part, done, dateless]);
    assert!(!by_book.contains(&untouched));

    assert_eq!(rows[0].get::<Option<i64>, _>("current_page"), Some(120));
    assert_eq!(rows[0].get::<Option<i64>, _>("started_at"), Some(1_000));
    assert_eq!(rows[0].get::<Option<i64>, _>("finished_at"), None);
    assert_eq!(rows[0].get::<String, _>("status"), "reading");
    assert_eq!(rows[0].get::<String, _>("source"), "migrated");

    assert_eq!(rows[1].get::<Option<i64>, _>("current_page"), Some(490));
    assert_eq!(rows[1].get::<Option<i64>, _>("started_at"), Some(1));
    assert_eq!(rows[1].get::<Option<i64>, _>("finished_at"), Some(2));
    assert_eq!(rows[1].get::<String, _>("status"), "finished");

    assert_eq!(rows[2].get::<String, _>("status"), "finished");
    assert_eq!(rows[2].get::<Option<i64>, _>("finished_at"), None);
}

/// The columns are really gone. A migration that back-filled but left them
/// behind would keep every stale writer silently working.
#[tokio::test]
async fn the_progress_columns_leave_books() {
    let mut conn = before_readings().await;
    apply_readings(&mut conn).await;

    let names: Vec<String> = sqlx::query("SELECT name FROM pragma_table_info('books')")
        .fetch_all(&mut conn)
        .await
        .expect("pragma")
        .into_iter()
        .map(|r| r.get("name"))
        .collect();
    for gone in ["current_page", "finished", "date_started", "date_finished"] {
        assert!(!names.contains(&gone.to_string()), "books still has {gone}");
    }
}

/// The back-fill writes exactly one reading per book, so it cannot itself
/// violate the "at most one open reading" index — but the index has to be
/// *there* for anything written afterwards.
#[tokio::test]
async fn the_one_open_reading_index_exists_after_the_backfill() {
    let mut conn = before_readings().await;
    let id = old_book(&mut conn, "part read", Some(10), 0, None, None).await;
    apply_readings(&mut conn).await;

    let err = sqlx::query(
        "INSERT INTO readings (book_id, status, source, created_at, last_modified)
         VALUES (?, 'reading', 'manual', 0, 0)",
    )
    .bind(id)
    .execute(&mut conn)
    .await
    .expect_err("a second open reading must be refused");
    assert!(
        matches!(&err, sqlx::Error::Database(db) if db.is_unique_violation()),
        "expected a unique violation, got {err}"
    );
}

/// `0007` rewrites the superseded `final` kind. A vault file is the note's
/// body, so the rewrite must move the *row* and leave the path alone — a note
/// whose `file_path` shifted would be a note whose text vanished.
#[tokio::test]
async fn old_final_notes_become_reflections_and_keep_their_files() {
    let mut conn = migrated_below(REFLECTION).await;
    for (path, title, kind) in [
        ("unsorted/a.md", "Last thoughts", "final"),
        ("unsorted/b.md", "A stray idea", "note"),
        ("unsorted/c.md", "Mid-book", "session"),
    ] {
        sqlx::query(
            "INSERT INTO notes (file_path, title, kind, created_at, last_modified)
             VALUES (?, ?, ?, 0, 0)",
        )
        .bind(path)
        .bind(title)
        .bind(kind)
        .execute(&mut conn)
        .await
        .expect("insert pre-0007 note");
    }

    apply(&mut conn, REFLECTION).await;

    let rows = sqlx::query("SELECT file_path, kind, reading_id FROM notes ORDER BY file_path")
        .fetch_all(&mut conn)
        .await
        .expect("select notes");
    assert_eq!(rows[0].get::<String, _>("kind"), "reflection");
    assert_eq!(rows[0].get::<String, _>("file_path"), "unsorted/a.md");
    assert_eq!(rows[0].get::<Option<i64>, _>("reading_id"), None);
    assert_eq!(rows[1].get::<String, _>("kind"), "note");
    assert_eq!(rows[2].get::<String, _>("kind"), "session");
}

/// Several old `final` notes migrate at once, and they all land with
/// `reading_id IS NULL` — under a plain unique index that would be a violation
/// and the whole migration would fail on a real library. SQLite treats NULLs as
/// distinct, which is exactly what makes the partial index usable here.
#[tokio::test]
async fn the_rewrite_survives_a_library_full_of_final_notes() {
    let mut conn = migrated_below(REFLECTION).await;
    for i in 0..5 {
        sqlx::query(
            "INSERT INTO notes (file_path, title, kind, created_at, last_modified)
             VALUES (?, ?, 'final', 0, 0)",
        )
        .bind(format!("unsorted/{i}.md"))
        .bind(format!("Note {i}"))
        .execute(&mut conn)
        .await
        .expect("insert");
    }
    apply(&mut conn, REFLECTION).await;

    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM notes WHERE kind = 'reflection'")
        .fetch_one(&mut conn)
        .await
        .expect("count");
    assert_eq!(n, 5);
}

/// The two partial indexes are the invariant "one reflection and one review per
/// reading". Asserted here against raw SQL rather than through the engine,
/// because it is the *index* that has to hold — the engine's accretion path
/// looks the existing note up and never gets this far.
#[tokio::test]
async fn a_reading_holds_one_reflection_and_one_review() {
    let mut conn = migrated_below(REFLECTION).await;
    apply(&mut conn, REFLECTION).await;

    let book: i64 = sqlx::query_scalar(
        "INSERT INTO books (title, created_at, last_modified) VALUES ('t', 0, 0) RETURNING id",
    )
    .fetch_one(&mut conn)
    .await
    .expect("book");
    let reading: i64 = sqlx::query_scalar(
        "INSERT INTO readings (book_id, status, source, created_at, last_modified)
         VALUES (?, 'reading', 'manual', 0, 0) RETURNING id",
    )
    .bind(book)
    .fetch_one(&mut conn)
    .await
    .expect("reading");

    let add = |path: &'static str, kind: &'static str| {
        sqlx::query(
            "INSERT INTO notes (reading_id, file_path, title, kind, created_at, last_modified)
             VALUES (?, ?, ?, ?, 0, 0)",
        )
        .bind(reading)
        .bind(path)
        .bind(path)
        .bind(kind)
    };

    add("a.md", "reflection")
        .execute(&mut conn)
        .await
        .expect("first reflection");
    add("b.md", "review")
        .execute(&mut conn)
        .await
        .expect("a review is a different kind, so it fits beside it");

    for (path, kind) in [("c.md", "reflection"), ("d.md", "review")] {
        let err = add(path, kind)
            .execute(&mut conn)
            .await
            .expect_err("a second one of the same kind must be refused");
        assert!(
            matches!(&err, sqlx::Error::Database(db) if db.is_unique_violation()),
            "expected a unique violation for a second {kind}, got {err}"
        );
    }

    // Ordinary notes are not constrained: a reading collects as many as it likes.
    for path in ["e.md", "f.md"] {
        add(path, "note")
            .execute(&mut conn)
            .await
            .expect("notes are unconstrained");
    }
}

/// The plan for one of the two queries `0008` exists for.
async fn plan(conn: &mut SqliteConnection, sql: &str) -> String {
    sqlx::query(&format!("EXPLAIN QUERY PLAN {sql}"))
        .fetch_all(conn)
        .await
        .expect("query plan")
        .iter()
        .map(|r| r.get::<String, _>("detail"))
        .collect::<Vec<_>>()
        .join("; ")
}

/// `0008` adds two indexes and changes nothing else, so the only claim it can
/// be judged on is that the planner reaches them.
///
/// Not ceremony. An index carries a collation and SQLite ignores one that does
/// not match the comparison's, so the obvious
/// `CREATE INDEX … ON note_links(target_title)` is BINARY and the
/// `COLLATE NOCASE` back-resolution in `write_links` scans straight past it —
/// an index that exists, reads correctly in the schema, and does nothing. The
/// `before` half is what makes the `after` half evidence rather than a
/// coincidence.
#[tokio::test]
async fn the_note_link_indexes_are_the_plan_the_planner_picks() {
    // `Storage::backlinks`, and `write_links`' back-resolution.
    const BACKLINKS: &str = "SELECT from_note FROM note_links WHERE to_note = 1";
    const RESOLVE: &str = "UPDATE note_links SET to_note = 1 \
         WHERE to_note IS NULL AND target_title = 'x' COLLATE NOCASE";

    let mut conn = migrated_below(NOTE_LINK_INDEX).await;
    for sql in [BACKLINKS, RESOLVE] {
        let detail = plan(&mut conn, sql).await;
        assert!(
            detail.contains("SCAN"),
            "before 0008 this must be a scan, or the test proves nothing: {detail}"
        );
    }

    apply(&mut conn, NOTE_LINK_INDEX).await;

    for (sql, index) in [
        (BACKLINKS, "idx_note_links_to"),
        (RESOLVE, "idx_note_links_target"),
    ] {
        let detail = plan(&mut conn, sql).await;
        assert!(
            detail.contains(index),
            "expected {index} in the plan, got {detail}"
        );
    }
}

/// `0016`'s before-and-after, in `0008`'s shape.
///
/// The engine's own
/// `storage::books::tests::the_sort_key_indexes_are_the_plan_the_planner_picks`
/// is the stronger half — it runs the exact statement `list_books` builds, so a
/// clause that stops matching its index fails there. What that one cannot show
/// is what the schema looked like *before*, and "the index is used" only means
/// something against a plan that did not use one. This is that half.
///
/// The `sort_author` clause is deliberately absent: the column does not exist
/// below this migration, so a "before" for it would be a syntax error rather
/// than a scan. Three sorts is what can honestly be compared across the line.
#[tokio::test]
async fn the_sort_key_indexes_are_a_change_of_plan() {
    const BY_RECENCY: &str =
        "SELECT id FROM books ORDER BY books.last_modified DESC, books.id DESC LIMIT 20";
    const BY_TITLE: &str = "SELECT id FROM books \
         ORDER BY COALESCE(books.sort_title, books.title) COLLATE NOCASE ASC, books.id ASC \
         LIMIT 20";
    const BY_YEAR: &str =
        "SELECT id FROM books ORDER BY books.publish_year DESC NULLS LAST, books.id DESC LIMIT 20";

    let mut conn = migrated_below(SORT_KEY_INDEXES).await;
    for sql in [BY_RECENCY, BY_TITLE, BY_YEAR] {
        let detail = plan(&mut conn, sql).await;
        assert!(
            detail.contains("USE TEMP B-TREE FOR ORDER BY"),
            "before 0016 every sort key sorted the whole table, or this test \
             proves nothing: {detail}"
        );
    }

    apply(&mut conn, SORT_KEY_INDEXES).await;

    for (sql, index) in [
        (BY_RECENCY, "idx_books_last_modified"),
        (BY_TITLE, "idx_books_sort_title"),
        (BY_YEAR, "idx_books_publish_year"),
    ] {
        let detail = plan(&mut conn, sql).await;
        assert!(
            detail.contains(index),
            "expected {index} in the plan, got {detail}"
        );
        assert!(
            !detail.contains("USE TEMP B-TREE FOR ORDER BY"),
            "{index} was scanned and the rows were sorted anyway: {detail}"
        );
    }
}

/// **The collation is part of the index**, and `0008` learned it the hard way.
///
/// `idx_books_sort_title` is declared `COLLATE NOCASE` because `BookSort::Title`
/// compares that way. Written without it — or over the bare column rather than
/// the `COALESCE` the clause names — the index exists, reads correctly in the
/// schema, and is silently never used. This asserts that the two *near misses*
/// really do miss, so the declaration above is doing work rather than
/// coinciding.
#[tokio::test]
async fn a_sort_title_index_that_nearly_matches_is_not_used() {
    let mut conn = migrated_below(SORT_KEY_INDEXES).await;
    apply(&mut conn, SORT_KEY_INDEXES).await;

    for near_miss in [
        // Right expression, wrong collation.
        "SELECT id FROM books ORDER BY COALESCE(books.sort_title, books.title) ASC, books.id ASC",
        // Right collation, wrong expression — the bare column the obvious
        // implementation would have indexed.
        "SELECT id FROM books ORDER BY books.sort_title COLLATE NOCASE ASC, books.id ASC",
    ] {
        let detail = plan(&mut conn, near_miss).await;
        assert!(
            !detail.contains("idx_books_sort_title"),
            "a clause that differs from the index must not reach it, or the \
             index's declaration is not what makes the real one work: {detail}"
        );
    }
}

/// The trap in the Goodreads migration (`0009`), caught at the level it is
/// actually set: the migration.
///
/// `active_rating_scale()` used to be "the newest scale", so seeding a
/// `goodreads` one would have made it the scale every unqualified rating landed
/// on. `is_default` back-fills to **whichever scale the old ordering would have
/// picked** — not to `default` by name — so a library where the user made their
/// own scale keeps using theirs, and the seeded `goodreads` row takes nothing.
#[tokio::test]
async fn the_default_scale_survives_seeding_goodreads() {
    let mut conn = migrated_below(GOODREADS).await;
    // A scale the user made after the seeded `default`, which is exactly what
    // the old ordering rule meant by "active".
    sqlx::query(
        "INSERT INTO rating_scales (name, min, max, step, created_at)
         VALUES ('mine', 0.0, 10.0, 1.0, strftime('%s','now') + 10)",
    )
    .execute(&mut conn)
    .await
    .expect("insert scale");

    apply(&mut conn, GOODREADS).await;

    let default: String = sqlx::query_scalar("SELECT name FROM rating_scales WHERE is_default = 1")
        .fetch_one(&mut conn)
        .await
        .expect("exactly one default");
    assert_eq!(default, "mine");

    let goodreads: i64 =
        sqlx::query_scalar("SELECT is_default FROM rating_scales WHERE name = 'goodreads'")
            .fetch_one(&mut conn)
            .await
            .expect("0009 seeds it");
    assert_eq!(goodreads, 0, "the newest scale is not the default any more");

    // And the seeded map is the identity, so an imported `My Rating` needs no
    // translation to be exported again.
    let mapped: Vec<(f64, i64)> = sqlx::query_as(
        "SELECT value, goodreads FROM rating_map
          WHERE scale_id = (SELECT id FROM rating_scales WHERE name = 'goodreads')
          ORDER BY value",
    )
    .fetch_all(&mut conn)
    .await
    .expect("map");
    assert_eq!(mapped.len(), 6);
    assert!(mapped.iter().all(|(v, g)| *v as i64 == *g));
}

/// "Exactly one default" is an index, not a convention — the same shape as
/// `idx_readings_one_open` and for the same reason: two of them is a state no
/// code should have to defend against.
#[tokio::test]
async fn a_second_default_scale_is_refused() {
    let mut conn = migrated_below(GOODREADS).await;
    apply(&mut conn, GOODREADS).await;

    let err = sqlx::query("UPDATE rating_scales SET is_default = 1 WHERE name = 'goodreads'")
        .execute(&mut conn)
        .await
        .expect_err("two defaults must be refused");
    assert!(
        matches!(&err, sqlx::Error::Database(db) if db.is_unique_violation()),
        "expected a unique violation, got {err}"
    );
}

/// The moments epoch is a **singleton written by the migration itself**, and
/// both halves of that are load-bearing.
///
/// Its value is what stops the first launch after `0017` replaying an entire
/// reading history as a ceremony, so a database where the row is missing has no
/// answer to "is this news or is this history" — and a database with two has
/// whichever answer a query happened to read. `CHECK (id = 1)` is the same
/// device `idx_readings_one_open` and `idx_one_reflection` are: an invariant a
/// table holds rather than a convention code defends.
#[tokio::test]
async fn the_moments_epoch_is_one_row_and_the_migration_wrote_it() {
    let mut conn = migrated_below(MOMENTS).await;
    apply(&mut conn, MOMENTS).await;

    let began: i64 = sqlx::query_scalar("SELECT began_at FROM moment_epoch")
        .fetch_one(&mut conn)
        .await
        .expect("the migration writes its own epoch");
    assert!(
        began > 1_700_000_000,
        "the epoch is the instant the schema learned about moments, not zero: {began}"
    );

    let err = sqlx::query("INSERT INTO moment_epoch (id, began_at) VALUES (2, 0)")
        .execute(&mut conn)
        .await
        .expect_err("a second epoch must be refused");
    assert!(
        matches!(&err, sqlx::Error::Database(db) if db.message().contains("CHECK")),
        "expected the CHECK to refuse it, got {err}"
    );
}

mod props {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// Whatever a book's progress was, the back-fill reproduces it exactly:
        /// one reading, same page, same dates, and a status that agrees with the
        /// old `finished` flag.
        ///
        /// A property rather than more examples because the rule is general over
        /// a four-field product — page present or not, finished or not, either
        /// date present or not — and the sixteen corners include the two that
        /// decide whether a *row is written at all*. Hand-picking three of them
        /// picks the three that work.
        #[test]
        fn every_pre_migration_book_round_trips(
            page in proptest::option::of(0i64..2000),
            finished in 0i64..2,
            started in proptest::option::of(0i64..2_000_000_000),
            ended in proptest::option::of(0i64..2_000_000_000),
        ) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all().build().unwrap();
            rt.block_on(async {
                let mut conn = before_readings().await;
                let id = old_book(&mut conn, "t", page, finished, started, ended).await;
                apply_readings(&mut conn).await;

                let rows = sqlx::query(
                    "SELECT started_at, finished_at, status, current_page
                     FROM readings WHERE book_id = ?")
                    .bind(id)
                    .fetch_all(&mut conn)
                    .await
                    .expect("select");

                let has_progress = page.is_some() || finished == 1
                    || started.is_some() || ended.is_some();
                if !has_progress {
                    prop_assert!(rows.is_empty(), "nothing to record, nothing recorded");
                    return Ok(());
                }
                prop_assert_eq!(rows.len(), 1, "exactly one reading per book");
                prop_assert_eq!(rows[0].get::<Option<i64>, _>("current_page"), page);
                prop_assert_eq!(rows[0].get::<Option<i64>, _>("started_at"), started);
                prop_assert_eq!(rows[0].get::<Option<i64>, _>("finished_at"), ended);
                prop_assert_eq!(
                    rows[0].get::<String, _>("status"),
                    if finished == 1 { "finished" } else { "reading" }
                );
                Ok(())
            })?;
        }
    }
}

const NOTE_PARTS: i64 = 23;

/// `0023` back-fills `latest_part_at` from `created_at` and every existing note
/// is one part — and the list's `ORDER BY latest_part_at DESC, id DESC` is the
/// index's own order rather than a sort of the whole table.
#[tokio::test]
async fn note_parts_back_fill_and_the_list_order_is_indexed() {
    let mut conn = migrated_below(NOTE_PARTS).await;
    sqlx::query(
        "INSERT INTO notes (file_path, title, kind, created_at, last_modified)
         VALUES ('a.md', 'A', 'note', 1000, 5000), ('b.md', 'B', 'note', 2000, 2000)",
    )
    .execute(&mut conn)
    .await
    .unwrap();
    apply(&mut conn, NOTE_PARTS).await;

    let rows: Vec<(i64, i64, i64)> =
        sqlx::query_as("SELECT created_at, parts, latest_part_at FROM notes ORDER BY id")
            .fetch_all(&mut conn)
            .await
            .unwrap();
    assert_eq!(
        rows,
        vec![(1000, 1, 1000), (2000, 1, 2000)],
        "created_at, never last_modified"
    );

    let plan: Vec<String> = sqlx::query(
        "EXPLAIN QUERY PLAN SELECT id FROM notes ORDER BY latest_part_at DESC, id DESC LIMIT 20",
    )
    .fetch_all(&mut conn)
    .await
    .unwrap()
    .iter()
    .map(|r| r.get::<String, _>("detail"))
    .collect();
    let plan = plan.join(" | ");
    assert!(plan.contains("idx_notes_latest_part"), "{plan}");
    assert!(!plan.contains("TEMP B-TREE"), "{plan}");
}

const PUT_DOWN_AT: i64 = 25;

/// `0025` is the sixth deliberate non-back-fill: a reading abandoned before it
/// keeps `put_down_at` NULL — *not since we started recording* — rather than
/// borrowing `last_modified`, which is only an upper bound. `0024` (settings)
/// lands empty beside it.
#[tokio::test]
async fn put_down_at_is_not_back_filled_and_settings_start_empty() {
    let mut conn = migrated_below(PUT_DOWN_AT).await;
    sqlx::query("INSERT INTO books (title, created_at, last_modified) VALUES ('B', 1, 1)")
        .execute(&mut conn)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO readings (book_id, started_at, status, created_at, last_modified)
         VALUES (1, 100, 'abandoned', 100, 9000)",
    )
    .execute(&mut conn)
    .await
    .unwrap();
    apply(&mut conn, PUT_DOWN_AT).await;
    let put_down: Option<i64> = sqlx::query_scalar("SELECT put_down_at FROM readings")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(put_down, None);
    let settings: i64 = sqlx::query_scalar("SELECT count(*) FROM settings")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(settings, 0);
}

const QUARTER_RATINGS: i64 = 26;

/// `0026`: both seeded scales become 0 to 5 in quarters and map onto each
/// other one-to-one — and the migration only *adds*. A rating given on the old
/// half-star grid is still on the new one, the map's column comes out REAL for
/// every row (not an INTEGER for the whole stars and a REAL for the rest), and
/// a half star the user had mapped by hand keeps their answer.
#[tokio::test]
async fn quarter_ratings_keep_every_rating_and_every_hand_made_mapping() {
    let mut conn = migrated_below(QUARTER_RATINGS).await;
    let step: f64 = sqlx::query_scalar("SELECT step FROM rating_scales WHERE name = 'default'")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(
        step, 0.5,
        "the old grid, so the migration has something to do"
    );

    // A review rated 4.5 on the half-star grid, and the user's own decision
    // about what 4.5 meant on Goodreads when Goodreads took no halves.
    sqlx::query(
        "INSERT INTO notes (file_path, title, kind, created_at, last_modified)
         VALUES ('r.md', 'Review', 'review', 1, 1)",
    )
    .execute(&mut conn)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO review_ratings (note_id, scale_id, value)
         SELECT 1, id, 4.5 FROM rating_scales WHERE name = 'default'",
    )
    .execute(&mut conn)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO rating_map (scale_id, value, goodreads)
         SELECT id, 4.5, 5 FROM rating_scales WHERE name = 'default'",
    )
    .execute(&mut conn)
    .await
    .unwrap();

    apply(&mut conn, QUARTER_RATINGS).await;

    let scales: Vec<(String, f64, f64, f64)> =
        sqlx::query_as("SELECT name, min, max, step FROM rating_scales ORDER BY id")
            .fetch_all(&mut conn)
            .await
            .unwrap();
    assert_eq!(
        scales,
        vec![
            ("default".to_string(), 0.0, 5.0, 0.25),
            ("goodreads".to_string(), 0.0, 5.0, 0.25),
        ]
    );

    for name in ["default", "goodreads"] {
        let map: Vec<(f64, f64, String)> = sqlx::query_as(
            "SELECT value, goodreads, typeof(goodreads) FROM rating_map
              WHERE scale_id = (SELECT id FROM rating_scales WHERE name = ?)
              ORDER BY value",
        )
        .bind(name)
        .fetch_all(&mut conn)
        .await
        .unwrap();
        assert_eq!(map.len(), 21, "every quarter from 0 to 5 on {name}");
        for (i, (value, goodreads, ty)) in map.iter().enumerate() {
            assert_eq!(*value, i as f64 * 0.25);
            assert_eq!(ty, "real", "{name} {value}");
            if name == "default" && *value == 4.5 {
                assert_eq!(*goodreads, 5.0, "the hand-made mapping is kept");
            } else {
                assert_eq!(goodreads, value, "{name} maps one-to-one");
            }
        }
    }

    let kept: f64 = sqlx::query_scalar("SELECT value FROM review_ratings WHERE note_id = 1")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(kept, 4.5);

    // Goodreads' own range is a CHECK, so the table cannot hold a sixth star.
    let err = sqlx::query(
        "INSERT INTO rating_map (scale_id, value, goodreads)
         SELECT id, 9.0, 6.0 FROM rating_scales WHERE name = 'default'",
    )
    .execute(&mut conn)
    .await;
    assert!(err.is_err());
}

/// A `default` the user redefined is theirs: `0026` moves only the seeded
/// range, so a 0–10 scale keeps its step and gets no identity rows it would
/// be wrong about.
#[tokio::test]
async fn quarter_ratings_leave_a_redefined_scale_alone() {
    let mut conn = migrated_below(QUARTER_RATINGS).await;
    sqlx::query("UPDATE rating_scales SET max = 10.0, step = 1.0 WHERE name = 'default'")
        .execute(&mut conn)
        .await
        .unwrap();
    apply(&mut conn, QUARTER_RATINGS).await;

    let (max, step): (f64, f64) =
        sqlx::query_as("SELECT max, step FROM rating_scales WHERE name = 'default'")
            .fetch_one(&mut conn)
            .await
            .unwrap();
    assert_eq!((max, step), (10.0, 1.0));
    let mapped: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM rating_map
          WHERE scale_id = (SELECT id FROM rating_scales WHERE name = 'default')",
    )
    .fetch_one(&mut conn)
    .await
    .unwrap();
    assert_eq!(mapped, 6, "the seeded whole stars, and nothing added");
}

const COPIES: i64 = 27;

async fn plain_book(conn: &mut SqliteConnection, title: &str) -> i64 {
    sqlx::query(
        "INSERT INTO books (title, created_at, last_modified) VALUES (?, 0, 0) RETURNING id",
    )
    .bind(title)
    .fetch_one(&mut *conn)
    .await
    .unwrap()
    .get("id")
}

/// One back-filled copy's (form, tenure, learned_from, places as `kind:ref`).
type CopyShape = (Option<String>, String, String, Vec<String>);

async fn copy_shape(conn: &mut SqliteConnection, book_id: i64) -> CopyShape {
    let copies = sqlx::query(
        "SELECT id, form, tenure, have_now, learned_from FROM copies WHERE book_id = ?",
    )
    .bind(book_id)
    .fetch_all(&mut *conn)
    .await
    .unwrap();
    assert_eq!(
        copies.len(),
        1,
        "book {book_id}: exactly one back-filled copy"
    );
    let c = &copies[0];
    let have_now: Option<i64> = c.get("have_now");
    assert_eq!(
        have_now, None,
        "the migration says nothing the user must say"
    );
    let places: Vec<String> = sqlx::query_scalar(
        "SELECT kind || ':' || ref FROM copy_places WHERE copy_id = ? ORDER BY kind, ref",
    )
    .bind(c.get::<i64, _>("id"))
    .fetch_all(&mut *conn)
    .await
    .unwrap();
    (
        c.get("form"),
        c.get("tenure"),
        c.get("learned_from"),
        places,
    )
}

/// `0027`: every book ends up in the library, with a copy that says what we
/// actually knew. Apply through `0026`, seed one book per kind of evidence
/// (and a book with several kinds, and one with none), apply `0027`, read back.
#[tokio::test]
async fn copies_back_fill_puts_every_book_in_the_library() {
    let mut conn = migrated_below(COPIES).await;
    let calibre = plain_book(&mut conn, "Calibre only").await;
    let reader = plain_book(&mut conn, "Reader only").await;
    let stored = plain_book(&mut conn, "Stored only").await;
    let all_three = plain_book(&mut conn, "All three").await;
    let none = plain_book(&mut conn, "No evidence").await;

    for (book_id, uuid) in [
        (calibre, "uuid-c"),
        (all_three, "uuid-a"),
        (all_three, "uuid-b"),
    ] {
        sqlx::query(
            "INSERT INTO external_ids (source, external_id, book_id, created_at)
             VALUES ('calibre', ?, ?, 7)",
        )
        .bind(uuid)
        .bind(book_id)
        .execute(&mut conn)
        .await
        .unwrap();
    }
    // A Goodreads id is not a copy.
    sqlx::query(
        "INSERT INTO external_ids (source, external_id, book_id, created_at)
         VALUES ('goodreads', '99', ?, 7)",
    )
    .bind(none)
    .execute(&mut conn)
    .await
    .unwrap();
    for (book_id, md5) in [(reader, "md5-r"), (all_three, "md5-a")] {
        sqlx::query(
            "INSERT INTO device_books (partial_md5, book_id, linked_by, first_seen, last_seen)
             VALUES (?, ?, 'auto', 1, 9)",
        )
        .bind(md5)
        .bind(book_id)
        .execute(&mut conn)
        .await
        .unwrap();
    }
    for (book_id, sha) in [(stored, "sha-s"), (all_three, "sha-a")] {
        sqlx::query(
            "INSERT INTO book_files (sha256, book_id, format, size, added_at)
             VALUES (?, ?, 'epub', 1, 3)",
        )
        .bind(sha)
        .bind(book_id)
        .execute(&mut conn)
        .await
        .unwrap();
    }

    apply(&mut conn, COPIES).await;

    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let file = Some("file".to_string());
    assert_eq!(
        copy_shape(&mut conn, calibre).await,
        (
            file.clone(),
            "own".into(),
            "calibre".into(),
            s(&["calibre:uuid-c"])
        )
    );
    assert_eq!(
        copy_shape(&mut conn, reader).await,
        (
            file.clone(),
            "own".into(),
            "koreader".into(),
            s(&["reader:md5-r"])
        )
    );
    assert_eq!(
        copy_shape(&mut conn, stored).await,
        (
            file.clone(),
            "own".into(),
            "file_import".into(),
            s(&["store:sha-s"])
        )
    );
    // One file is one copy wherever it sits: calibre's copy gathers the rest.
    assert_eq!(
        copy_shape(&mut conn, all_three).await,
        (
            file,
            "own".into(),
            "calibre".into(),
            s(&[
                "calibre:uuid-a",
                "calibre:uuid-b",
                "reader:md5-a",
                "store:sha-a"
            ])
        )
    );
    assert_eq!(
        copy_shape(&mut conn, none).await,
        (None, "unrecorded".into(), "migration".into(), s(&[]))
    );

    // The point of the migration: nothing leaves the shelf.
    let sql = format!(
        "SELECT count(*) FROM books b WHERE {}",
        bookmole::storage::in_library_sql("b.id")
    );
    let n: i64 = sqlx::query_scalar(&sql).fetch_one(&mut conn).await.unwrap();
    assert_eq!(n, 5, "every book is in the library after the back-fill");

    // Place identity is (kind, ref): a second claim on one is refused.
    let dup =
        sqlx::query("INSERT INTO copy_places (copy_id, kind, ref) VALUES (1, 'calibre', 'uuid-c')")
            .execute(&mut conn)
            .await;
    assert!(dup.is_err());
}

const HIGHLIGHT_ORIGIN: i64 = 30;

async fn old_highlight(
    conn: &mut SqliteConnection,
    book_id: i64,
    text: &str,
    pos0: Option<&str>,
    note: Option<&str>,
    source: &str,
) -> i64 {
    sqlx::query(
        "INSERT INTO highlights (book_id, text, pos0, ko_note, last_seen_ko_note, source,
                                 identity_hash, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, 0) RETURNING id",
    )
    .bind(book_id)
    .bind(text)
    .bind(pos0)
    .bind(note)
    .bind(note)
    .bind(source)
    .bind(format!("{book_id}-{text}"))
    .fetch_one(&mut *conn)
    .await
    .unwrap()
    .get("id")
}

async fn reader_copy_of(conn: &mut SqliteConnection, book_id: i64, md5: &str) -> i64 {
    let id: i64 = sqlx::query(
        "INSERT INTO copies (book_id, form, tenure, learned_from, created_at, updated_at)
         VALUES (?, 'file', 'own', 'koreader', 0, 0) RETURNING id",
    )
    .bind(book_id)
    .fetch_one(&mut *conn)
    .await
    .unwrap()
    .get("id");
    sqlx::query("INSERT INTO copy_places (copy_id, kind, ref) VALUES (?, 'reader', ?)")
        .bind(id)
        .bind(md5)
        .execute(&mut *conn)
        .await
        .unwrap();
    id
}

type OriginRow = (
    String,
    Option<i64>,
    Option<String>,
    Option<String>,
    Option<String>,
);

async fn origin_row(conn: &mut SqliteConnection, id: i64) -> OriginRow {
    sqlx::query_as(
        "SELECT origin, copy_id, origin_note, last_seen_origin_note, pos_kind
           FROM highlights WHERE id = ?",
    )
    .bind(id)
    .fetch_one(&mut *conn)
    .await
    .unwrap()
}

async fn fts_hits(conn: &mut SqliteConnection, word: &str) -> Vec<i64> {
    sqlx::query_scalar("SELECT rowid FROM highlights_fts WHERE highlights_fts MATCH ?")
        .bind(word)
        .fetch_all(&mut *conn)
        .await
        .unwrap()
}

/// `0030`: apply through `0029`, seed KOReader and typed highlights with notes,
/// then apply `0030` and read back what the back-fill decided.
#[tokio::test]
async fn highlight_origin_back_fills_origin_copy_note_and_position() {
    let mut conn = migrated_below(HIGHLIGHT_ORIGIN).await;
    let one = plain_book(&mut conn, "One reader copy").await;
    let none = plain_book(&mut conn, "No copy").await;
    let two = plain_book(&mut conn, "Two reader copies").await;
    let copy = reader_copy_of(&mut conn, one, "md5-one").await;
    reader_copy_of(&mut conn, two, "md5-two-a").await;
    reader_copy_of(&mut conn, two, "md5-two-b").await;

    let note = Some("zebra marginalia");
    let ko = old_highlight(&mut conn, one, "a", Some("/body/p[1]"), note, "koreader").await;
    let typed = old_highlight(&mut conn, one, "b", None, None, "user").await;
    let no_pos = old_highlight(&mut conn, one, "c", None, None, "koreader").await;
    let orphan = old_highlight(&mut conn, none, "d", Some("/body/p[2]"), None, "koreader").await;
    let ambiguous = old_highlight(&mut conn, two, "e", Some("/body/p[3]"), None, "koreader").await;

    apply(&mut conn, HIGHLIGHT_ORIGIN).await;

    assert_eq!(
        origin_row(&mut conn, ko).await,
        (
            "koreader".into(),
            Some(copy),
            Some("zebra marginalia".into()),
            Some("zebra marginalia".into()),
            Some("koreader_xpointer".into())
        )
    );
    // A typed passage is the user's; a copy is a KOReader fact, so none.
    assert_eq!(
        origin_row(&mut conn, typed).await,
        ("user".into(), None, None, None, None)
    );
    assert_eq!(origin_row(&mut conn, no_pos).await.4, None);
    // No copy, or two candidate copies, is a guess: left NULL.
    assert_eq!(origin_row(&mut conn, orphan).await.1, None);
    assert_eq!(origin_row(&mut conn, ambiguous).await.1, None);

    // The rebuilt index finds the note word, and a later edit of the new
    // column moves it.
    assert_eq!(fts_hits(&mut conn, "zebra").await, vec![ko]);
    sqlx::query("UPDATE highlights SET origin_note = 'giraffe' WHERE id = ?")
        .bind(ko)
        .execute(&mut conn)
        .await
        .unwrap();
    assert!(fts_hits(&mut conn, "zebra").await.is_empty());
    assert_eq!(fts_hits(&mut conn, "giraffe").await, vec![ko]);

    // Deleting a copy keeps the history.
    sqlx::query("DELETE FROM copies WHERE id = ?")
        .bind(copy)
        .execute(&mut conn)
        .await
        .unwrap();
    assert_eq!(origin_row(&mut conn, ko).await.1, None);
}

const SEARCH_CACHE: i64 = 32;

async fn index_hits(conn: &mut SqliteConnection, word: &str) -> Vec<i64> {
    sqlx::query_scalar(
        "SELECT rowid FROM provider_record_index WHERE provider_record_index MATCH ? ORDER BY rowid",
    )
    .bind(format!("\"{word}\""))
    .fetch_all(&mut *conn)
    .await
    .unwrap()
}

/// `0032`: a library from before it gains four empty tables and loses nothing;
/// nothing is seeded into `provider_settings` (an absent row is the default);
/// the trigram index follows its table through insert, update and delete; and
/// `field_provenance.source` was never constrained, so nothing widened it.
#[tokio::test]
async fn the_search_cache_arrives_empty_and_its_index_follows_its_table() {
    let mut conn = migrated_below(SEARCH_CACHE).await;
    let book = plain_book(&mut conn, "Dune").await;
    apply(&mut conn, SEARCH_CACHE).await;

    for table in ["provider_records", "search_cache", "provider_settings"] {
        let n: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(n, 0, "{table} is seeded");
    }
    let title: String = sqlx::query_scalar("SELECT title FROM books WHERE id = ?")
        .bind(book)
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(title, "Dune");

    let id: i64 = sqlx::query_scalar(
        "INSERT INTO provider_records
             (provider, key, kind, form, title, authors, body, fetched_at, expires_at)
         VALUES ('openlibrary', '/works/OL1W', 'work', 'doc', 'The Dispossessed',
                 'Ursula K. Le Guin', '{}', 0, 1) RETURNING id",
    )
    .fetch_one(&mut conn)
    .await
    .unwrap();
    assert_eq!(index_hits(&mut conn, "possess").await, vec![id], "infix");
    assert_eq!(index_hits(&mut conn, "guin").await, vec![id], "authors");
    sqlx::query(
        "INSERT INTO provider_records
             (provider, key, kind, form, title, authors, body, fetched_at, expires_at)
         VALUES ('openlibrary', '/works/OL1W', 'work', 'doc', 'Café Society', NULL, '{}', 0, 1)
         ON CONFLICT (provider, key) DO UPDATE SET title = excluded.title,
             authors = excluded.authors",
    )
    .execute(&mut conn)
    .await
    .unwrap();
    assert!(index_hits(&mut conn, "possess").await.is_empty());
    assert_eq!(
        index_hits(&mut conn, "cafe").await,
        vec![id],
        "diacritics removed"
    );
    sqlx::query("DELETE FROM provider_records")
        .execute(&mut conn)
        .await
        .unwrap();
    assert!(index_hits(&mut conn, "cafe").await.is_empty());

    // The vocabularies are checked where they are closed and not where they
    // grow.
    for bad in [
        "INSERT INTO provider_records (provider, key, kind, form, body, fetched_at, expires_at)
         VALUES ('openlibrary', 'k', 'author', 'doc', '{}', 0, 1)",
        "INSERT INTO provider_settings VALUES ('openlibrary', 2, 0)",
    ] {
        assert!(sqlx::query(bad).execute(&mut conn).await.is_err(), "{bad}");
    }
    sqlx::query("INSERT INTO provider_settings VALUES ('inventaire', 1, 0)")
        .execute(&mut conn)
        .await
        .unwrap();
    let provenance: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'field_provenance'",
    )
    .fetch_one(&mut conn)
    .await
    .unwrap();
    assert!(
        !provenance.to_uppercase().contains("CHECK"),
        "field_provenance.source grew a CHECK; 0032's note that no widening was needed is stale"
    );
}

const BOOKS_SEARCH_INDEX: i64 = 33;

async fn book_hits(conn: &mut SqliteConnection, text: &str) -> Vec<i64> {
    sqlx::query_scalar("SELECT rowid FROM books_search WHERE books_search MATCH ? ORDER BY rowid")
        .bind(format!("\"{text}\""))
        .fetch_all(&mut *conn)
        .await
        .unwrap()
}

async fn books_index_is_true(conn: &mut SqliteConnection) {
    sqlx::query("INSERT INTO books_search (books_search, rank) VALUES ('integrity-check', 1)")
        .execute(&mut *conn)
        .await
        .expect("books_search agrees with books");
}

/// `0033`: the books written before it are indexed by its `'rebuild'`, and the
/// triggers follow the table from then on — an insert, an edit to an indexed
/// column, an edit to one that is not, and a delete.
#[tokio::test]
async fn the_books_index_back_fills_and_follows_its_table() {
    let mut conn = migrated_below(BOOKS_SEARCH_INDEX).await;
    let old = plain_book(&mut conn, "The Dispossessed").await;
    sqlx::query(
        "UPDATE books SET authors = '[\"Ursula K. Le Guin\"]', series = 'Hainish Cycle',
                          isbn_13 = '9780060512750' WHERE id = ?",
    )
    .bind(old)
    .execute(&mut conn)
    .await
    .unwrap();
    apply(&mut conn, BOOKS_SEARCH_INDEX).await;
    books_index_is_true(&mut conn).await;

    assert_eq!(
        book_hits(&mut conn, "possess").await,
        vec![old],
        "back-filled, infix"
    );
    assert_eq!(book_hits(&mut conn, "guin").await, vec![old], "authors");
    assert_eq!(book_hits(&mut conn, "hainish").await, vec![old], "series");
    assert_eq!(
        book_hits(&mut conn, "9780060512750").await,
        vec![old],
        "isbn"
    );

    let new = plain_book(&mut conn, "Brontë Country").await;
    assert_eq!(
        book_hits(&mut conn, "bronte").await,
        vec![new],
        "diacritics removed"
    );
    sqlx::query("UPDATE books SET title = 'Anarres' WHERE id = ?")
        .bind(old)
        .execute(&mut conn)
        .await
        .unwrap();
    assert!(book_hits(&mut conn, "possess").await.is_empty());
    assert_eq!(book_hits(&mut conn, "anarres").await, vec![old]);
    sqlx::query("UPDATE books SET last_modified = 99, sort_title = 'x' WHERE id = ?")
        .bind(old)
        .execute(&mut conn)
        .await
        .unwrap();
    books_index_is_true(&mut conn).await;
    sqlx::query("DELETE FROM books WHERE id = ?")
        .bind(new)
        .execute(&mut conn)
        .await
        .unwrap();
    assert!(book_hits(&mut conn, "bronte").await.is_empty());
    books_index_is_true(&mut conn).await;
}
