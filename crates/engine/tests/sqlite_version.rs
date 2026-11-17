//! The linked SQLite must be the patched one (>= 3.54.0): asserts the running library's
//! version, since a dropped `[patch.crates-io]` would otherwise go unnoticed.

use sqlx::{Connection, SqliteConnection};

#[tokio::test]
async fn the_linked_sqlite_carries_the_wal_page_size_fix() {
    let mut conn = SqliteConnection::connect("sqlite::memory:")
        .await
        .expect("an in-memory database opens");
    let version: String = sqlx::query_scalar("SELECT sqlite_version()")
        .fetch_one(&mut conn)
        .await
        .expect("sqlite_version() answers");
    let parts: Vec<u32> = version
        .split('.')
        .map(|p| p.parse().expect("numeric version part"))
        .collect();
    assert!(
        parts.as_slice() >= [3, 54, 0].as_slice(),
        "SQLite {version} predates 3.54.0 (RUSTSEC-2026-0459); is vendor/libsqlite3-sys patched in?"
    );
}
