mod annotation_alternatives;
mod book_files;
mod book_search;
mod books;
pub(crate) mod clashes;
mod copies;
mod device_books;
mod field_provenance;
mod flashcards;
mod fts;
mod highlights;
mod history;
mod imported_highlights;
mod lists;
mod moments;
mod note_rows;
mod notes;
mod paired_devices;
mod provenance;
mod query;
mod ratings;
mod reading_events;
mod readings;
mod search_cache;
mod settings;
mod sidecar_seen;
pub(crate) mod synced;

pub use annotation_alternatives::AnnotationAlternative;
pub use book_files::BookFile;
pub use book_search::BookMatch;
#[cfg(test)]
pub(crate) use book_search::LibraryBook;
pub(crate) use book_search::{HeldFacts, LibraryIdentities};
pub use books::{BookSort, MergeReport};
/// Lets enrichment name columns the merge table knows rather than column-like strings.
pub(crate) use books::{
    field_pair, field_value, fields_said, merge_columns, merge_provider_record, provider_columns,
};
pub use copies::{in_library_sql, touched_sql};
pub use device_books::LinkedBy;
pub use field_provenance::{FieldSource, Source};
pub use flashcards::FlashcardRow;
pub use fts::{SearchHit, SearchSource};
pub(crate) use highlights::DeviceDigest;
pub use highlights::{Highlight, NewHighlight};
pub use history::{DayFact, MarkStamp, ReadFacts, ReadWindow};
pub use imported_highlights::imported_identity_hash;
pub(crate) use moments::run_cutoff;
pub use moments::{Moment, MomentKind, RUN_MIN_DAYS, runs_of_days};
pub use note_rows::{LinkEdge, NoteRowFacts, NoteRowFilter, NoteStamp};
pub(crate) use notes::file_owners;
pub use notes::{
    NewNoteMeta, NoteCitations, NoteFile, NoteRecord, NoteScope, OutgoingLink, RESURFACE_REST_DAYS,
    ResurfaceReason, Resurfaced, ResurfacedPassage, parse_local_date,
};
pub use paired_devices::PairedDevice;
pub use provenance::BookTag;
pub use query::{
    BookFilter, BookQuery, BookSummary, LibraryScope, ReadingFilter, ReadingQuery, StatusFilter,
};
pub use ratings::{GOODREADS_STEP, Rating, RatingScale, goodreads_point};
pub use reading_events::{
    ActivitySummary, Confidence, DayActivity, DayRange, FillStats, MonthActivity, NewReadingEvent,
    ReadingEvent, RefillReport, SOURCE_KOREADER, SOURCE_LOCAL, SOURCE_VAULT, day_of_ko_datetime,
    day_of_unix,
};
pub(crate) use readings::DeviceNews;
pub use readings::{
    ReadCount, ReadNumbering, Reading, ReadingRow, ReadingSort, ReadingYears, STATUS_ABANDONED,
    STATUS_FINISHED, STATUS_READING, ko_datetime_to_unix,
};
pub(crate) use search_cache::{KeptEntry, KeptRow};
pub use search_cache::{SEARCHES_KEPT_FOR, SearchCacheStatus};
pub use settings::{SETTING_KEY_MAX, Setting, is_setting_key};
pub use sidecar_seen::SidecarFacts;

use std::str::FromStr;

use fongo_sync_sqlite::Resume;
use sqlx::Connection;
use sqlx::migrate::Migrator;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection, SqlitePool, SqlitePoolOptions};

use crate::error::Result;

/// How long a statement waits for another connection's write lock before it fails. A sync, an
/// install or another process can hold the lock between its steps for a second or more, and
/// several writers waiting for the same gap can each miss a few.
pub(crate) const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

#[derive(Debug, Clone)]
pub struct Storage {
    pool: SqlitePool,
}

impl Storage {
    /// For tests outside the crate; behind `internals` so no frontend can open the database itself.
    #[cfg(any(test, feature = "internals"))]
    #[doc(hidden)]
    pub async fn connect(db_url: &str) -> Result<Storage> {
        Self::open(db_url).await
    }

    pub(crate) async fn open(db_url: &str) -> Result<Storage> {
        let opts = SqliteConnectOptions::from_str(db_url)?
            .create_if_missing(true)
            .foreign_keys(true)
            .busy_timeout(BUSY_TIMEOUT);
        // In-memory databases are per-connection: a >1 pool would migrate one
        // connection and query a fresh empty one.
        let max = if db_url.contains(":memory:") { 1 } else { 5 };
        let pool = SqlitePoolOptions::new()
            .max_connections(max)
            .connect_with(opts)
            .await?;
        {
            let mut conn = pool.acquire().await?;
            if crate::sync::installed(&mut conn).await? {
                open_synced(&mut conn).await?;
            } else {
                migrate(&mut conn).await?;
                rehash(&mut conn).await;
            }
        }
        tracing::info!(max_connections = max, "storage connected and migrated");
        Ok(Storage { pool })
    }

    pub(crate) fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// Applied migrations whose comments were amended: `(version, old SHA-384, current SHA-384)`.
/// The SQL must stay byte-identical; a new row also needs the file exempted from CI's
/// append-only check.
const AMENDED: &[(i64, &str, &str)] = &[
    (
        3,
        "3706bfd669cd67ae081429420a7a9b0f21e4044e576ca2dbf0f6160aaa23e34a79bea1ce2a1e251a163e1585795a1a6e",
        "85ab90eb4f36e1afe0e232ccfb1a5df5948f4fc6fdd2f4be2b590f03280c0fa89b49bd5df4c1b632d9bea562e691e45f",
    ),
    (
        4,
        "f096e0990b7824126befaa136ccc5f4e28d18f79f7aeb930000fc1ea2f092af76875e01a2c2a3c1bb453c0b87483b884",
        "5292c764c816986cd9382410e832ba23a4b343927f7b55d9e73dbc50359f63d14d196018186b15a746266c9209a10069",
    ),
    (
        8,
        "640152fd1170f9a1cdb346baf081542144edde15eac66cf18e4db55d820540b2d2de97af8122c328a3380009043cdec6",
        "a6b7d30eb2193949716606e151408ca01b30272652efaff0d26316d5bdb0b09c01c39a4423b9b0ddaa92149368b101a0",
    ),
    (
        9,
        "65fe27cf83a414c2763ef1e4ec22ca258c5b29f48a2f6e803f38340e7cd483d3327322c0d904c576ab89397554265133",
        "99dcd40b89244fba3a43e67c68e819eac7887d0797349a252bddee320c53b6daf14d4e244de99853aa2ff1557e228e8d",
    ),
    (
        11,
        "9a5f2034302deae7f72fa7f5e7e199b0e6ca97606c4c3e16a4743c26f59cb22cee1163170ac5e89041c3c8405d833d81",
        "1ef77b74096ce8721fc3b4ec43a3e2e2de031038e287aa0508f9c097d7857219a8ccd846b01a6a0fd0f0f12032ce9860",
    ),
    (
        12,
        "f293a1872552c727b9af8e31950dd0483df61974b7f42ec40f03ac58e426d776db2005d349bc38fc59685d0e3dca1d07",
        "897c1d4d36b2489c7be09e0ec3cf481b58d08a11c949f2f24ba3510eba2ec60ca9b557f78233aaad750bdd1ce281cf3a",
    ),
    (
        13,
        "eaf0bfe34cec6163f66372f68bcdfc1c167d8e7715b184e9ed7f1724f4f8e9b5a66157e826f7b7840467cf400c826f29",
        "3a62601de197f0883e7c435809efedb989ebaf2fe1c353484e8bb9d14bf82380d079824560748a5fc4eb1eacd776fd0c",
    ),
    (
        14,
        "c6e45e5e915c7f29c7297909dc0eb1c0f7ef65df3edfe54c3ab7064aa28ff36bb464db1f28122c92eb6afe894e011019",
        "d00e8afbf54ec804982e5cc0ada55b7b76d50729c87c6526a687e6a6655da7a9865bbf8900824b95ffba2d3394e2020d",
    ),
    (
        15,
        "4629afd41954a2b9695d69f2196d204743efb10efafb58a90d01993f3eac0db403ad64d4a307d32707c1dc7c93e8866b",
        "13a1c48973d20cc5eb7dbc6214c130fdbc0b7fc2d565407bb919ac26901e337450383a9f61b2d0e7c0f4a3be819bda6e",
    ),
    (
        16,
        "992d903be27a799fc9c61297704cf137df10c06b0eb401b222d42422971559b7211e8dc4cc80d24620a34b11333eedd0",
        "cb03d275966ee9e9584caf578fc1505b85c953436ed69a386f474424755ea98eae4bec1f58b88c641b8dccad58c0e5e4",
    ),
    (
        16,
        "d184936f1716d761084b21f09815a9212193969dcc2f60626e5a2b68e0476047cffe6956979500be2774239b205a926f",
        "cb03d275966ee9e9584caf578fc1505b85c953436ed69a386f474424755ea98eae4bec1f58b88c641b8dccad58c0e5e4",
    ),
    (
        17,
        "92c5eedff229d0a1c1d15cd561ac95bd41953f581938f83e2cb27769f64db2728f67ccc0dfa2673622d9f768f405275f",
        "1c3dbf229c7ad462f3cbbf07b5ec9284615b1415d1c852767cb275a678e674e0dd4365bcb515ca92e13489ede787314d",
    ),
    (
        18,
        "b6e43f31223fd4c89f60e3cc0f852eed337d41378ae93374d0ed808a3fdef0484485abd3f18be8397c203f34b3ea3441",
        "6c17d952b189c470486a40240de4ced50963782aecf604950a75f55dc4e03665bdd6145404a4057e4f01b56243cf5e09",
    ),
    (
        19,
        "42db9d65a1f6051586bead4d5ddbafde20f6eeec003f874d2e1a5ec5a148697bde33ec6e1f33b5faf80dfd897ee7ace6",
        "5ea5747e9ddf81b073f80117eaa14b27938f80393474e7ca2d47058f896150b3a6887d9a28d2c0af890c83ce2e2adda1",
    ),
    (
        19,
        "047b227bdd24968170bb10edc83e3fc778fd1590eb29c7f7a596adb13faf57b4da54518ffda11588de61a445fd94616c",
        "5ea5747e9ddf81b073f80117eaa14b27938f80393474e7ca2d47058f896150b3a6887d9a28d2c0af890c83ce2e2adda1",
    ),
    (
        20,
        "3648d8bb0e8c9959f25278a873da26d4dc3b7023edd180991c12995c282beeecad31773d5ffa66ef753121266c8a8f7c",
        "4e1a49a74473bd08df6ef54b073e9fbacf813c7a370c2ed8d9376cf14f2f33352c9de55b97743f7be8781aa5a63d0027",
    ),
    (
        21,
        "d8e4a0f3bdaef543b21bda59b53c73f23e8603313a4fb6b181fa679121056e747511ba0f1d8117fdc858ce9411fc6f2b",
        "e68f2ead7b872922dd2e3a0629530a63d77fcd9c4b4b2d9e202a279e96e940af678dee47878321c28803ccccb3676414",
    ),
    (
        22,
        "7343b9d264baddaef3cf15e610e00a55256062dd54bcc3db5854237f3ba085b0f54af28562c257243afa43c2d4b1aad5",
        "850cea6c4a586a0d3b48c0c81df0645c6ccdf1f652f637ae5443df789f70c66c260833c1a985da346f4d19da195d004f",
    ),
    (
        22,
        "35734ca3473d61e7eeee3481666b63de35232d3c2ca268e4fd7b85c7c593919327ec89c7ec14c6a532fd2c236d1d3135",
        "850cea6c4a586a0d3b48c0c81df0645c6ccdf1f652f637ae5443df789f70c66c260833c1a985da346f4d19da195d004f",
    ),
    (
        23,
        "5b279d4765ec390d8eed223cbafd188e1732a654979d0ecb8a5b6846946fb2c92b93cbcdd4bacfa994c2f6a7c8dcde10",
        "07fa6639c4460fd88068c7df8b635b729bd9458eb7cda1078b801611e461e896f666511a8c43a94c4241dcbac935ca3a",
    ),
    (
        24,
        "d5d304f34804bcec3f74abcb208eb1d3e221fcbdc8c9d561ae0782d3b88acd5d6524151982c4fcd1894a0ac13bf7c9c9",
        "f9f57dd108d0f035b276def5e0d7cc105ab2155217717f07b35a4523727e66a4a3df269af5268a8627a8d0ab10b0cdb6",
    ),
    (
        25,
        "680e7c78a2c2a5ecd716d8338a4ee76b59f57d80192443e05358271d803ee3474cbf8bf7518011bb707a4149d03154ee",
        "2b73e0091172461b4d596f339f9e7019f5a0b9182ab6ebc9e0f608a68fa7c5d148b46613fe30d789cb859cfd13ba9491",
    ),
    (
        26,
        "048532ce9d7696eeed52f9bd676fc97b87940d8658d8b3c90e4565ab8280e3b75da21813a0485bf56b198f5beeb6621a",
        "0119e7f111a2c1023f113c110b3704e6a5b71c5310a40e77c7c68ff7c8fce35e1ea320e4dcb5b2132646e1aae83ffa65",
    ),
    (
        26,
        "11fb4ddfac96c4089a720f05a4bf9c189e7bc9108bf6ca8227f43628ff700783c68166f7f72e467e476abd6497e6455a",
        "0119e7f111a2c1023f113c110b3704e6a5b71c5310a40e77c7c68ff7c8fce35e1ea320e4dcb5b2132646e1aae83ffa65",
    ),
    (
        27,
        "0bf72e682b0b4dd0e25617fc49947dccaac9f3f4f9a3a4357017f1ee1ab2cb570d2f7617b88ba46a59c5b7057f0e6e90",
        "0f6ba67be9ad51ed8cec007bd2c30fd8dd524a6e9efbe51a14a77bdabe3199ddd3bd16ab652eec92a8c3aa8e36f0e076",
    ),
    (
        28,
        "bc7afbf31bc0e22a74b6893dc8ffdfee66b15442e59b1d07073cb8879a546f68b93d667f5a9e93e37c4441542c77c29c",
        "ade7cbbb26588e7f9e8c0a88d1988c01ebe2949a8cc13b3fbf063678bf2b839cb170f93522d0e7a66ec099ad0c922084",
    ),
    (
        29,
        "38899bf5a8eb74f4e8f2303050e0608b280db18ed0762ba8fafb0bd0c70890a1a944ae6000097d4497e826ad72a6b031",
        "7895faedd98fdc8a967cb571b39d3b6f3715ef2000f37c3937ad744265d48bceac5dc6bb8e7acb500e2d40f2a13bc635",
    ),
    (
        30,
        "b2c90d3d4679815dfac170071237f394662169116a66eb8c90728da43e4b435cbe3aab2a43537cabc9c400849aa7edcc",
        "2e9a179caca2f83fb022ae13a0676a102473fa1630896485fb2fbb53633125a13466b0553ec468e2de2d80275471c1df",
    ),
    (
        31,
        "888124508c5efcbfb5203c1fb8d1f1509ff5b5f94617f5b89e91c1ec5716fe385e19f82174071d735e56f21c36899e2a",
        "8049fa6ad99a08802412ce3dba52a781e22d721059701031135c53afd227666c804a11b3935c15d72c1b987972db268e",
    ),
    (
        32,
        "452f680ead82dd1ad402fe533d24678f0932d0d5b4c42d5b54d82a526d7169a473abac97410166a4bc277f30a2b124fc",
        "ecff3236527ae424ed61552707fde685f52f40a91578fff25b1aa583ce039d8ea6e46a0f92b94e0a5163599f85343b84",
    ),
    (
        32,
        "fc799a1c0736eb9a762806d714af49d5098ebc734de0bbdf864701a20c097a48b7e670d5a97895a2ed6ef0fb7128d39c",
        "ecff3236527ae424ed61552707fde685f52f40a91578fff25b1aa583ce039d8ea6e46a0f92b94e0a5163599f85343b84",
    ),
    (
        33,
        "1ed250019f48a9dc74a68b93425d011cd97f53bad2257c1a142d9a3ee2c2376baf375781407620bdb7651fc772dbae57",
        "0f6e2548e6a9cfbb182de6edc3d241a1cbc866448d570671a5010114cdc6223f80a336f654f06235ffbb54e6b5330ba1",
    ),
];

/// Never fatal: a library must open even if the rehash cannot finish. Each batch commits alone, and
/// the next open carries on from the rows still left.
async fn rehash(conn: &mut SqliteConnection) {
    match highlights::rehash_old_identities(conn, highlights::REHASH_BATCH).await {
        Ok(0) => {}
        Ok(rehashed) => tracing::info!(
            rehashed,
            "moved highlights to the book-independent identity hash"
        ),
        Err(error) => tracing::warn!(
            %error,
            "could not move every highlight to the book-independent identity hash; \
             the next open tries again"
        ),
    }
}

/// Opens a library that syncs. Every device runs the same migrations, so the rows they rewrite
/// are kept as this device's own rather than sent; for that, change capture is suspended around
/// them while this connection holds the write lock, so no other process writes uncaptured.
/// Capture an earlier open left suspended resumes sending differences, since anything may have
/// been written meanwhile. A failure to resume is logged, not returned: the library still opens,
/// and the next open finds capture suspended and tries again. A failure to suspend is returned,
/// and nothing is migrated.
async fn open_synced(conn: &mut SqliteConnection) -> Result<()> {
    let capture_on = capture_installed(conn).await?;
    if !migrations_pending(conn).await? {
        rehash(conn).await;
        if !capture_on {
            resume(conn, Resume::Send).await;
        }
        return Ok(());
    }
    hold_write_lock(conn).await?;
    // Read again under the lock: another open may have suspended capture and stopped since.
    let capture_on = match capture_installed(conn).await {
        Ok(on) => on,
        Err(error) => {
            release_write_lock(conn).await?;
            return Err(error);
        }
    };
    // Migrating with capture on would send every row rewritten, and could leave sync refused.
    if capture_on && let Err(error) = suspend(conn).await {
        release_write_lock(conn).await?;
        return Err(crate::sync::SyncError::CannotPause(error.to_string()).into());
    }
    let migrated = migrate(conn).await;
    if migrated.is_ok() {
        rehash(conn).await;
    }
    resume(
        conn,
        if capture_on {
            Resume::Baseline
        } else {
            Resume::Send
        },
    )
    .await;
    let released = release_write_lock(conn).await;
    migrated?;
    released
}

async fn suspend(conn: &mut SqliteConnection) -> Result<()> {
    crate::sync::replica()?
        .suspend(conn)
        .await
        .map_err(crate::sync::kernel)
}

async fn resume(conn: &mut SqliteConnection, how: Resume) {
    if let Err(error) = resume_installed(conn, how).await {
        tracing::warn!(%error, "could not resume sync; the next open tries again");
    }
}

/// Resuming installs, so it is done only while the kernel's tables are there: another engine on
/// this database may have left sync since this open looked.
pub(crate) async fn resume_installed(conn: &mut SqliteConnection, how: Resume) -> Result<()> {
    let replica = crate::sync::replica()?;
    let mut tx = conn.begin_with("BEGIN IMMEDIATE").await?;
    if crate::sync::installed(&mut tx).await? {
        replica
            .resume(&mut tx, how)
            .await
            .map_err(crate::sync::kernel)?;
    }
    tx.commit().await?;
    Ok(())
}

pub(crate) async fn capture_installed(conn: &mut SqliteConnection) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master
                         WHERE type = 'trigger' AND name LIKE 'fongo!_%' ESCAPE '!')",
    )
    .fetch_one(conn)
    .await?)
}

async fn migrations_pending(conn: &mut SqliteConnection) -> Result<bool> {
    let applied: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success = 1")
            .fetch_all(conn)
            .await?;
    Ok(MIGRATOR.iter().any(|m| !applied.contains(&m.version)))
}

/// Other processes wait on their busy timeout, then fail busy, until [`release_write_lock`].
async fn hold_write_lock(conn: &mut SqliteConnection) -> Result<()> {
    sqlx::raw_sql("PRAGMA locking_mode = EXCLUSIVE; BEGIN EXCLUSIVE; COMMIT;")
        .execute(conn)
        .await?;
    Ok(())
}

/// The lock goes at the first read after the mode changes back.
async fn release_write_lock(conn: &mut SqliteConnection) -> Result<()> {
    sqlx::raw_sql("PRAGMA locking_mode = NORMAL; SELECT count(*) FROM sqlite_master;")
        .execute(conn)
        .await?;
    Ok(())
}

/// Repairs only the exact listed old checksums; any other mismatch is still refused.
async fn migrate(conn: &mut SqliteConnection) -> Result<()> {
    repair_amended_checksums(conn).await?;
    MIGRATOR.run(&mut *conn).await?;
    Ok(())
}

async fn repair_amended_checksums(conn: &mut SqliteConnection) -> Result<()> {
    let table: Option<(i64,)> = sqlx::query_as(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_optional(&mut *conn)
    .await?;
    if table.is_none() {
        return Ok(());
    }
    for &(version, old, _) in AMENDED {
        let Some(current) = MIGRATOR.iter().find(|m| m.version == version) else {
            continue;
        };
        let done = sqlx::query(
            "UPDATE _sqlx_migrations SET checksum = ? WHERE version = ? AND checksum = ?",
        )
        .bind(current.checksum.as_ref())
        .bind(version)
        .bind(from_hex(old))
        .execute(&mut *conn)
        .await?;
        if done.rows_affected() > 0 {
            tracing::info!(version, "recorded the amended checksum of a migration");
        }
    }
    Ok(())
}

fn from_hex(hex: &str) -> Vec<u8> {
    hex.as_bytes()
        .chunks(2)
        .map(|pair| {
            let digit = |c: u8| (c as char).to_digit(16).expect("hex digit") as u8;
            digit(pair[0]) << 4 | digit(pair[1])
        })
        .collect()
}

pub(crate) fn now_unix() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Doubles as the FTS5-availability canary: a migration creates an fts5 table.
    #[tokio::test]
    async fn migrates_in_memory() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let n: (i64,) = sqlx::query_as("SELECT count(*) FROM books")
            .fetch_one(s.pool())
            .await
            .unwrap();
        assert_eq!(n.0, 0);
    }

    async fn checksum(pool: &SqlitePool, version: i64) -> Vec<u8> {
        sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version = ?")
            .bind(version)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn set_checksum(pool: &SqlitePool, version: i64, to: &[u8]) {
        sqlx::query("UPDATE _sqlx_migrations SET checksum = ? WHERE version = ?")
            .bind(to)
            .bind(version)
            .execute(pool)
            .await
            .unwrap();
    }

    fn current(version: i64) -> Vec<u8> {
        MIGRATOR
            .iter()
            .find(|m| m.version == version)
            .unwrap()
            .checksum
            .to_vec()
    }

    /// The final pin is the append-only rule for amended files: a further edit is refused here.
    #[test]
    fn every_amended_entry_names_a_changed_migration() {
        for &(version, old, new) in AMENDED {
            let old = from_hex(old);
            assert_eq!(old.len(), 48, "version {version}: not a SHA-384");
            assert_ne!(old, current(version), "version {version} did not change");
            assert_eq!(
                from_hex(new),
                current(version),
                "migration {version} was edited again after its amendment; \
                 every library that applied it would now refuse to open"
            );
        }
    }

    /// The amended-migrations list must equal this table: a missing entry fails CI, an extra one
    /// lets an unrepaired edit through.
    #[test]
    fn the_amended_list_matches_the_repair_table() {
        let list = include_str!("../../../../.github/amended-migrations.txt");
        let listed: std::collections::BTreeSet<i64> = list
            .lines()
            .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
            .map(|l| {
                let name = l.rsplit('/').next().unwrap();
                name.split('_').next().unwrap().parse().unwrap()
            })
            .collect();
        let table: std::collections::BTreeSet<i64> = AMENDED.iter().map(|&(v, _, _)| v).collect();
        assert_eq!(listed, table);
    }

    #[tokio::test]
    async fn a_library_migrated_before_the_amendment_opens() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("app.db").display());
        let s = Storage::connect(&url).await.unwrap();
        for &(version, old, _) in AMENDED {
            set_checksum(s.pool(), version, &from_hex(old)).await;
        }
        s.pool().close().await;

        let s = Storage::connect(&url).await.unwrap();
        for &(version, _, _) in AMENDED {
            assert_eq!(checksum(s.pool(), version).await, current(version));
        }
    }

    #[tokio::test]
    async fn any_other_checksum_is_still_refused() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        for version in [19, 22, 26, 1] {
            let honest = checksum(s.pool(), version).await;
            set_checksum(s.pool(), version, &[0xab; 48]).await;
            let err = migrate(&mut s.pool().acquire().await.unwrap())
                .await
                .unwrap_err();
            assert!(
                matches!(
                    err,
                    crate::error::EngineError::Migrate(
                        sqlx::migrate::MigrateError::VersionMismatch(v)
                    ) if v == version
                ),
                "version {version}: {err:?}"
            );
            assert_eq!(checksum(s.pool(), version).await, [0xab; 48]);
            set_checksum(s.pool(), version, &honest).await;
        }
        migrate(&mut s.pool().acquire().await.unwrap())
            .await
            .unwrap();
    }

    /// Rows a migration or the open-time rehash rewrites are rewritten alike on every device, so
    /// a synced library sends none of them; a write after the open is still sent.
    #[tokio::test]
    async fn opening_a_synced_library_sends_nothing_a_migration_rewrote() {
        use sqlx::Connection;
        use sqlx::migrate::Migrate;

        const FIRST_LATER: i64 = 35;
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("app.db").display());
        let opts = SqliteConnectOptions::from_str(&url)
            .unwrap()
            .create_if_missing(true)
            .foreign_keys(true);
        let mut conn = SqliteConnection::connect_with(&opts).await.unwrap();
        conn.ensure_migrations_table().await.unwrap();
        for m in MIGRATOR.iter().filter(|m| m.version < FIRST_LATER) {
            conn.apply(m).await.unwrap();
        }
        let book: i64 = sqlx::query_scalar(
            "INSERT INTO books (title, created_at, last_modified)
             VALUES ('Middlemarch', 0, 0) RETURNING id",
        )
        .fetch_one(&mut conn)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO highlights (book_id, text, source, origin, identity_hash, created_at)
             VALUES (?1, 'Who can quit young lives after being long in company with them?',
                     'koreader', 'koreader', 'an earlier formula', 0)",
        )
        .bind(book)
        .execute(&mut conn)
        .await
        .unwrap();
        let replica = crate::sync::replica().unwrap();
        replica.install(&mut conn).await.unwrap();
        replica.collect(&mut conn).await.unwrap();
        let sent = replica.outbox(&mut conn, 10_000).await.unwrap();
        assert!(!sent.changes.is_empty());
        replica.acknowledge(&mut conn, sent.through).await.unwrap();
        conn.close().await.unwrap();

        let s = Storage::connect(&url).await.unwrap();
        let mut conn = s.pool().acquire().await.unwrap();
        let hash: String = sqlx::query_scalar("SELECT identity_hash FROM highlights")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        assert!(hash.starts_with(highlights::IDENTITY_PREFIX), "{hash}");
        assert_eq!(replica.collect(&mut conn).await.unwrap(), 0);
        assert!(
            replica
                .outbox(&mut conn, 10)
                .await
                .unwrap()
                .changes
                .is_empty()
        );

        sqlx::query("UPDATE books SET title = 'Middlemarch: A Study of Provincial Life'")
            .execute(&mut *conn)
            .await
            .unwrap();
        assert_eq!(replica.collect(&mut conn).await.unwrap(), 1);
    }

    /// An open that cannot suspend sync before migrating refuses to open and migrates nothing:
    /// here a first install in steps still waits to adopt rows, and a column it reads is gone.
    /// Once the column is back, the open suspends, migrates and resumes.
    #[tokio::test]
    async fn an_open_that_cannot_suspend_sync_migrates_nothing() {
        use sqlx::Connection;
        use sqlx::migrate::Migrate;

        const FIRST_LATER: i64 = 35;
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("app.db").display());
        let opts = SqliteConnectOptions::from_str(&url)
            .unwrap()
            .create_if_missing(true)
            .foreign_keys(true);
        let mut conn = SqliteConnection::connect_with(&opts).await.unwrap();
        conn.ensure_migrations_table().await.unwrap();
        for m in MIGRATOR.iter().filter(|m| m.version < FIRST_LATER) {
            conn.apply(m).await.unwrap();
        }
        for title in ["Middlemarch", "Romola", "Adam Bede", "Silas Marner"] {
            sqlx::query(
                "INSERT INTO books (title, publisher, created_at, last_modified)
                 VALUES (?, 'Blackwood', 0, 0)",
            )
            .bind(title)
            .execute(&mut conn)
            .await
            .unwrap();
        }
        let replica = crate::sync::replica().unwrap();
        assert!(replica.install_step(&mut conn, 1).await.unwrap().more);
        let rename =
            |from: &str, to: &str| format!("ALTER TABLE books RENAME COLUMN {from} TO {to}");
        sqlx::query(&rename("publisher", "imprint"))
            .execute(&mut conn)
            .await
            .unwrap();
        conn.close().await.unwrap();

        let refused = Storage::connect(&url).await.unwrap_err();
        assert!(
            matches!(
                refused,
                crate::EngineError::Sync(crate::sync::SyncError::CannotPause(_))
            ),
            "{refused}"
        );
        let mut conn = SqliteConnection::connect_with(&opts).await.unwrap();
        let latest: i64 = sqlx::query_scalar("SELECT max(version) FROM _sqlx_migrations")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert!(latest < FIRST_LATER, "{latest}");
        assert!(capture_installed(&mut conn).await.unwrap());
        assert!(replica.uncollected_rows(&mut conn, 10).await.unwrap() > 0);
        sqlx::query(&rename("imprint", "publisher"))
            .execute(&mut conn)
            .await
            .unwrap();
        conn.close().await.unwrap();

        let s = Storage::connect(&url).await.unwrap();
        let mut conn = s.pool().acquire().await.unwrap();
        assert!(!migrations_pending(&mut conn).await.unwrap());
        assert!(capture_installed(&mut conn).await.unwrap());
        assert_eq!(replica.uncollected_rows(&mut conn, 10).await.unwrap(), 0);
    }

    /// A library whose capture another open suspended: what was written meanwhile is sent.
    async fn synced_and_suspended(url: &str) -> fongo_sync_sqlite::Replica {
        let s = Storage::connect(url).await.unwrap();
        let replica = crate::sync::replica().unwrap();
        let mut conn = s.pool().acquire().await.unwrap();
        sqlx::query(
            "INSERT INTO books (title, created_at, last_modified) VALUES ('Middlemarch', 0, 0)",
        )
        .execute(&mut *conn)
        .await
        .unwrap();
        replica.install(&mut conn).await.unwrap();
        replica.collect(&mut conn).await.unwrap();
        let sent = replica.outbox(&mut conn, 10_000).await.unwrap();
        replica.acknowledge(&mut conn, sent.through).await.unwrap();
        replica.suspend(&mut conn).await.unwrap();
        sqlx::query("UPDATE books SET title = 'Middlemarch: A Study of Provincial Life'")
            .execute(&mut *conn)
            .await
            .unwrap();
        drop(conn);
        s.pool().close().await;
        replica
    }

    async fn sends_the_new_title(s: &Storage, replica: &fongo_sync_sqlite::Replica) {
        let mut conn = s.pool().acquire().await.unwrap();
        replica.collect(&mut conn).await.unwrap();
        let out = replica.outbox(&mut conn, 10_000).await.unwrap();
        assert!(
            out.changes.iter().any(|c| c.field == "title"
                && c.value
                    == fongo_sync_sqlite::core::Value::Text(
                        "Middlemarch: A Study of Provincial Life".into()
                    )),
            "{:?}",
            out.changes
        );
    }

    #[tokio::test]
    async fn a_write_made_while_capture_was_suspended_is_sent() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("app.db").display());
        let replica = synced_and_suspended(&url).await;
        let s = Storage::connect(&url).await.unwrap();
        sends_the_new_title(&s, &replica).await;
    }

    /// A schema the registry no longer fits keeps sync off but the library open, and the open
    /// after the schema fits again sends what was written meanwhile.
    #[tokio::test]
    async fn a_sync_failure_at_open_leaves_the_library_usable() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("app.db").display());
        let replica = synced_and_suspended(&url).await;
        let rename = |from: &str, to: &str| {
            format!("ALTER TABLE dismissed_shelves RENAME COLUMN {from} TO {to}")
        };
        let mut raw = <SqliteConnection as sqlx::Connection>::connect(&url)
            .await
            .unwrap();
        sqlx::query(&rename("dismissed_at", "dismissed_on"))
            .execute(&mut raw)
            .await
            .unwrap();
        sqlx::Connection::close(raw).await.unwrap();

        let s = Storage::connect(&url).await.unwrap();
        let mut conn = s.pool().acquire().await.unwrap();
        assert!(!capture_installed(&mut conn).await.unwrap());
        sqlx::query(&rename("dismissed_on", "dismissed_at"))
            .execute(&mut *conn)
            .await
            .unwrap();
        drop(conn);
        s.pool().close().await;

        let s = Storage::connect(&url).await.unwrap();
        sends_the_new_title(&s, &replica).await;
    }

    #[tokio::test]
    async fn every_connection_of_the_pool_waits_fifteen_seconds_for_the_write_lock() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("app.db").display());
        let s = Storage::connect(&url).await.unwrap();
        for _ in 0..3 {
            let waits: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
                .fetch_one(s.pool())
                .await
                .unwrap();
            assert_eq!(waits, 15_000);
        }
    }

    #[tokio::test]
    async fn no_other_connection_writes_while_the_lock_is_held() {
        use sqlx::Connection;

        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("app.db").display());
        let s = Storage::connect(&url).await.unwrap();
        let mut holder = s.pool().acquire().await.unwrap();
        hold_write_lock(&mut holder).await.unwrap();

        let other = SqliteConnectOptions::from_str(&url)
            .unwrap()
            .busy_timeout(std::time::Duration::from_millis(50));
        let mut other = SqliteConnection::connect_with(&other).await.unwrap();
        let write =
            "INSERT INTO books (title, created_at, last_modified) VALUES ('Middlemarch', 0, 0)";
        let refused = sqlx::query(write).execute(&mut other).await.unwrap_err();
        assert!(refused.to_string().contains("locked"), "{refused}");

        release_write_lock(&mut holder).await.unwrap();
        sqlx::query(write).execute(&mut other).await.unwrap();
    }

    /// An open that found sync on resumes only while it still is: another engine on the database
    /// may have left since, and resuming would install change capture again.
    #[tokio::test]
    async fn resuming_after_sync_was_left_installs_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("app.db").display());
        let s = Storage::connect(&url).await.unwrap();
        let mut conn = s.pool().acquire().await.unwrap();
        resume_installed(&mut conn, Resume::Send).await.unwrap();
        let kernel: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM sqlite_master WHERE name LIKE 'fongo!_%' ESCAPE '!'",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert_eq!(kernel, 0);
    }
}
