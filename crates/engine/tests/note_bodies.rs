//! `note_bodies` follows the indexed body: written on create, edit and sweep, never written when
//! the body is unchanged, and gone with its note.

mod common;

use bookmole::{Engine, EngineConfig, NewNoteInput, NoteRecord};
use sqlx::SqlitePool;

async fn engine() -> (tempfile::TempDir, Engine, SqlitePool) {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(tmp.path().join("database")).unwrap();
    let mut config = EngineConfig::in_memory(tmp.path());
    config.db_url = format!(
        "sqlite://{}?mode=rwc",
        tmp.path().join("database/app.db").display()
    );
    let engine = Engine::open(config).await.expect("engine opens");
    let pool = SqlitePool::connect(engine.db_url()).await.unwrap();
    (tmp, engine, pool)
}

/// Trimmed: a note's file ends in a newline, and trailing whitespace is not part of the body.
async fn stored_body(pool: &SqlitePool, note_id: i64) -> Option<String> {
    sqlx::query_scalar::<_, String>("SELECT body FROM note_bodies WHERE note_id = ?")
        .bind(note_id)
        .fetch_optional(pool)
        .await
        .unwrap()
        .map(|b| b.trim_end().to_owned())
}

/// Counts every row write to `note_bodies` from here on, whoever makes it.
async fn count_writes(pool: &SqlitePool) {
    for sql in [
        "CREATE TABLE body_writes (kind TEXT NOT NULL)",
        "CREATE TRIGGER body_written_i AFTER INSERT ON note_bodies
         BEGIN INSERT INTO body_writes VALUES ('insert'); END",
        "CREATE TRIGGER body_written_u AFTER UPDATE ON note_bodies
         BEGIN INSERT INTO body_writes VALUES ('update'); END",
    ] {
        sqlx::query(sql).execute(pool).await.unwrap();
    }
}

async fn writes(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM body_writes")
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn note(engine: &Engine, body: &str) -> NoteRecord {
    let book = common::seed_book(engine, "Middlemarch").await;
    let created = engine
        .create_note(NewNoteInput {
            book_id: Some(book),
            title: Some("Middlemarch, chapter 4".into()),
            body: body.into(),
            ..Default::default()
        })
        .await
        .unwrap();
    engine
        .storage()
        .get_note(created.id)
        .await
        .unwrap()
        .unwrap()
}

fn push_mtime_ahead(path: &std::path::Path) {
    let ahead =
        std::fs::metadata(path).unwrap().modified().unwrap() + std::time::Duration::from_secs(60);
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(ahead)
        .unwrap();
}

#[tokio::test]
async fn a_new_note_has_its_body_in_a_row() {
    let (_tmp, engine, pool) = engine().await;
    let n = note(&engine, "Dorothea's dignity under pressure.").await;
    assert_eq!(
        stored_body(&pool, n.id).await.as_deref(),
        Some("Dorothea's dignity under pressure.")
    );
}

#[tokio::test]
async fn an_edit_through_the_engine_rewrites_the_row() {
    let (_tmp, engine, pool) = engine().await;
    let n = note(&engine, "first thought").await;
    engine
        .update_note_body(&n, "Casaubon's calculation.")
        .await
        .unwrap();
    assert_eq!(
        stored_body(&pool, n.id).await.as_deref(),
        Some("Casaubon's calculation.")
    );
}

#[tokio::test]
async fn an_outside_edit_reaches_the_row_through_the_sweep() {
    let (_tmp, engine, pool) = engine().await;
    let n = note(&engine, "Dorothea's dignity under pressure.").await;
    let file = engine.note_path(&n).unwrap();
    let raw = std::fs::read_to_string(&file).unwrap();
    std::fs::write(
        &file,
        raw.replace("Dorothea's dignity under pressure.", "Lydgate's ambition."),
    )
    .unwrap();
    push_mtime_ahead(&file);

    assert_eq!(engine.reconcile_vault().await.unwrap().reindexed, 1);
    assert_eq!(
        stored_body(&pool, n.id).await.as_deref(),
        Some("Lydgate's ambition.")
    );
}

#[tokio::test]
async fn an_unchanged_body_writes_nothing() {
    let (_tmp, engine, pool) = engine().await;
    let n = note(&engine, "a thought that stays").await;
    count_writes(&pool).await;

    engine
        .update_note_body(&n, "a thought that stays")
        .await
        .unwrap();
    engine.refresh_note_from_disk(&n).await.unwrap();
    push_mtime_ahead(&engine.note_path(&n).unwrap());
    engine.reconcile_vault().await.unwrap();
    assert_eq!(writes(&pool).await, 0, "the same body was written again");

    engine
        .update_note_body(&n, "a thought that moved")
        .await
        .unwrap();
    assert_eq!(writes(&pool).await, 1, "a real change is one write");
}

#[tokio::test]
async fn deleting_a_note_takes_its_body_row() {
    let (_tmp, engine, pool) = engine().await;
    let n = note(&engine, "soon gone").await;
    assert!(stored_body(&pool, n.id).await.is_some());
    engine.delete_note(&n).await.unwrap();
    assert_eq!(stored_body(&pool, n.id).await, None);
}
