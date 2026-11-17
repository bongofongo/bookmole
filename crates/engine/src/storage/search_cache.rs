//! Kept provider answers, kept searches, and which providers are switched on
//! (migration `0032`).
//!
//! **Records are kept long; searches are kept short.** A record is what a
//! provider sent about one work or edition, kept for the provider's own term
//! (`ProviderDefaults::kept_for`) and found again by its key or, offline, by its
//! words (`provider_record_index`). A search is the ordered list of record keys
//! a query returned, kept thirty days under a **hash** of the query
//! (`search::session::query_key`), never its text.
//!
//! Three rules are enforced here rather than by callers:
//!
//! - **A provider whose records may not be kept is refused** by
//!   [`Storage::keep_answer`], whatever a caller hands it — Google Books' terms
//!   allow no durable copy, and a rule that every caller has to
//!   remember is a rule that holds until the first one forgets.
//! - **A kept search naming a record that has expired is no answer at all**
//!   ([`Storage::kept_answer`] returns `None`): half an answer, served as the
//!   whole of it, would be a list with rows silently missing.
//! - **Purging happens at open and on every write**, so a library nobody
//!   searches in still forgets on schedule.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sqlx::Row;

use super::{Storage, now_unix};
use crate::error::Result;
use crate::providers::{KeptRecord, ProviderId, RecordForm, RecordKind};

/// How long a search is kept: thirty days. Provider records are kept long
/// (their own terms, months); what you searched for is kept short.
pub const SEARCHES_KEPT_FOR: i64 = 30 * 86_400;

/// One result of a kept search, as `search_cache.results` stores it: the keys of
/// the records it was kept as, in the order they are read back, and whether the
/// provider returned it when asked for the query **as an author** (so a repeat
/// reads it as that author's book, as the first answer did).
///
/// `by_author` is a flag and not the name: the name is the query, and the
/// query is never stored: what somebody searches for is their private reading
/// interest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct KeptEntry {
    #[serde(rename = "r")]
    pub records: Vec<String>,
    #[serde(rename = "a", default, skip_serializing_if = "std::ops::Not::not")]
    pub by_author: bool,
}

/// A kept record read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeptRow {
    pub key: String,
    pub kind: RecordKind,
    pub form: RecordForm,
    pub body: String,
    pub fetched_at: i64,
}

/// A kept search, with every record it names.
#[derive(Debug, Clone)]
pub(crate) struct KeptAnswer {
    pub fetched_at: i64,
    pub results: Vec<(KeptEntry, Vec<KeptRow>)>,
}

/// What is kept, for a settings view of the search providers.
/// **No count of searches**: a number of things looked for is a history in
/// all but name, and a settings view has no use for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SearchCacheStatus {
    /// Any kept search or kept record at all.
    pub anything_kept: bool,
    /// When the oldest kept record was fetched (unix seconds).
    pub oldest_record: Option<i64>,
}

fn kind_of(word: &str) -> RecordKind {
    if word == "work" {
        RecordKind::Work
    } else {
        RecordKind::Edition
    }
}

fn row_of(r: &sqlx::sqlite::SqliteRow) -> Option<KeptRow> {
    Some(KeptRow {
        key: r.get("key"),
        kind: kind_of(r.get::<String, _>("kind").as_str()),
        form: RecordForm::parse(r.get::<String, _>("form").as_str())?,
        body: r.get("body"),
        fetched_at: r.get("fetched_at"),
    })
}

/// A word for fts5 `MATCH`, quoted so nothing in it is syntax (`storage/fts.rs`'
/// rule): a `"` inside is doubled.
fn phrase(word: &str) -> String {
    format!("\"{}\"", word.replace('"', "\"\""))
}

impl Storage {
    /// The kept answer to the search `query_key` from `provider`, when one is
    /// kept, is younger than [`SEARCHES_KEPT_FOR`], and every record it names
    /// is still kept. `None` otherwise — including when a record has expired,
    /// since a partial list read as a whole one is rows silently missing.
    pub(crate) async fn kept_answer(
        &self,
        query_key: &str,
        provider: ProviderId,
        now: i64,
    ) -> Result<Option<KeptAnswer>> {
        let Some(row) = sqlx::query(
            "SELECT results, fetched_at FROM search_cache
              WHERE query_key = ? AND provider = ? AND fetched_at > ?",
        )
        .bind(query_key)
        .bind(provider.as_str())
        .bind(now - SEARCHES_KEPT_FOR)
        .fetch_optional(self.pool())
        .await?
        else {
            return Ok(None);
        };
        let fetched_at: i64 = row.get("fetched_at");
        let Ok(entries) = serde_json::from_str::<Vec<KeptEntry>>(&row.get::<String, _>("results"))
        else {
            tracing::debug!(provider = %provider, "a kept search no longer reads; asking again");
            return Ok(None);
        };
        let keys: Vec<&str> = entries
            .iter()
            .flat_map(|e| e.records.iter().map(String::as_str))
            .collect();
        let rows = sqlx::query(
            "SELECT key, kind, form, body, fetched_at FROM provider_records
              WHERE provider = ? AND expires_at > ?
                AND key IN (SELECT value FROM json_each(?))",
        )
        .bind(provider.as_str())
        .bind(now)
        .bind(serde_json::to_string(&keys)?)
        .fetch_all(self.pool())
        .await?;
        let by_key: HashMap<String, KeptRow> = rows
            .iter()
            .filter_map(row_of)
            .map(|r| (r.key.clone(), r))
            .collect();
        let mut results = Vec::with_capacity(entries.len());
        for entry in entries {
            let mut records = Vec::with_capacity(entry.records.len());
            for key in &entry.records {
                match by_key.get(key) {
                    Some(r) => records.push(r.clone()),
                    None => {
                        tracing::debug!(provider = %provider,
                            "a kept search names a record no longer kept; asking again");
                        return Ok(None);
                    }
                }
            }
            results.push((entry, records));
        }
        Ok(Some(KeptAnswer {
            fetched_at,
            results,
        }))
    }

    /// Keep what `provider` answered to the search `query_key`: every record,
    /// then the ordered list naming them. One transaction, so a kept search
    /// never names a record that was not written with it.
    ///
    /// **Refuses a provider whose records may not be kept**, with a
    /// `debug!` and no write: the caller is told nothing went wrong, because
    /// nothing did — the answer is still shown, it is just not kept.
    pub(crate) async fn keep_answer(
        &self,
        query_key: &str,
        provider: ProviderId,
        results: &[(KeptEntry, Vec<KeptRecord>)],
        now: i64,
    ) -> Result<()> {
        let Some(kept_for) = provider.defaults().kept_for else {
            tracing::debug!(provider = %provider, "this provider's answers are never kept");
            return Ok(());
        };
        let expires_at = now + kept_for.as_secs() as i64;
        let mut tx = self.pool().begin().await?;
        for record in results.iter().flat_map(|(_, r)| r) {
            sqlx::query(
                "INSERT INTO provider_records
                     (provider, key, kind, form, title, authors, body, fetched_at, expires_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT (provider, key) DO UPDATE SET
                     kind = excluded.kind, form = excluded.form, title = excluded.title,
                     authors = excluded.authors, body = excluded.body,
                     fetched_at = excluded.fetched_at, expires_at = excluded.expires_at",
            )
            .bind(provider.as_str())
            .bind(&record.key)
            .bind(record.kind.as_str())
            .bind(record.form.as_str())
            .bind(&record.title)
            .bind((!record.authors.is_empty()).then(|| record.authors.join("; ")))
            .bind(&record.body)
            .bind(now)
            .bind(expires_at)
            .execute(&mut *tx)
            .await?;
        }
        let entries: Vec<&KeptEntry> = results.iter().map(|(e, _)| e).collect();
        sqlx::query(
            "INSERT INTO search_cache (query_key, provider, results, fetched_at)
             VALUES (?, ?, ?, ?)
             ON CONFLICT (query_key, provider) DO UPDATE SET
                 results = excluded.results, fetched_at = excluded.fetched_at",
        )
        .bind(query_key)
        .bind(provider.as_str())
        .bind(serde_json::to_string(&entries)?)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        purge_in(&mut tx, now).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Kept **work** records of `provider` whose title or authors hold any of
    /// `words` — the offline index: an offline search finds a book seen before
    /// by its own words. Best fts5 rank first. Words under three
    /// characters cannot use a trigram index and are left out; with none left
    /// there is nothing to ask and nothing is returned.
    pub(crate) async fn kept_records_matching(
        &self,
        provider: ProviderId,
        words: &[String],
        now: i64,
        limit: u32,
    ) -> Result<Vec<KeptRow>> {
        let terms: Vec<String> = words
            .iter()
            .filter(|w| w.chars().count() >= 3)
            .map(|w| phrase(w))
            .collect();
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "SELECT r.key, r.kind, r.form, r.body, r.fetched_at
               FROM provider_record_index
               JOIN provider_records r ON r.id = provider_record_index.rowid
              WHERE provider_record_index MATCH ?
                AND r.provider = ? AND r.kind = 'work' AND r.expires_at > ?
              ORDER BY provider_record_index.rank, r.id
              LIMIT ?",
        )
        .bind(terms.join(" OR "))
        .bind(provider.as_str())
        .bind(now)
        .bind(i64::from(limit))
        .fetch_all(self.pool())
        .await?;
        Ok(rows.iter().filter_map(row_of).collect())
    }

    /// The language most of the library's books are in, when any say: the
    /// default edition's second language preference, after the query's own
    /// `lang:` and before English.
    pub(crate) async fn library_language(&self) -> Result<Option<String>> {
        Ok(sqlx::query_scalar(
            "SELECT language FROM books WHERE language IS NOT NULL AND language <> ''
              GROUP BY language ORDER BY count(*) DESC, language LIMIT 1",
        )
        .fetch_optional(self.pool())
        .await?)
    }

    /// Drop one kept record that no longer parses, so it is fetched again. The
    /// kept searches naming it stop being answers ([`Storage::kept_answer`])
    /// and are asked again.
    pub(crate) async fn drop_kept_record(&self, provider: ProviderId, key: &str) -> Result<()> {
        sqlx::query("DELETE FROM provider_records WHERE provider = ? AND key = ?")
            .bind(provider.as_str())
            .bind(key)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Delete searches older than thirty days and records past their expiry.
    /// Called when the engine opens; [`Storage::keep_answer`] does the same in
    /// its own transaction.
    pub(crate) async fn purge_search_cache(&self, now: i64) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        purge_in(&mut tx, now).await?;
        tx.commit().await?;
        Ok(())
    }

    /// *Forget searches*: empty `search_cache`; with `records`,
    /// the kept records and their index as well. **Neither touches a book** —
    /// a book added from a search is the reader's, not the cache's.
    pub async fn forget_searches(&self, records: bool) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("DELETE FROM search_cache")
            .execute(&mut *tx)
            .await?;
        if records {
            // The delete trigger keeps the index in step, row by row.
            sqlx::query("DELETE FROM provider_records")
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        tracing::info!(records, "kept searches forgotten");
        Ok(())
    }

    /// Whether anything is kept, and the oldest kept record's date.
    pub async fn search_cache_status(&self) -> Result<SearchCacheStatus> {
        let row = sqlx::query(
            "SELECT (SELECT min(fetched_at) FROM provider_records) AS oldest,
                    EXISTS (SELECT 1 FROM search_cache) AS searches",
        )
        .fetch_one(self.pool())
        .await?;
        let oldest: Option<i64> = row.get("oldest");
        let searches: bool = row.get("searches");
        Ok(SearchCacheStatus {
            anything_kept: searches || oldest.is_some(),
            oldest_record: oldest,
        })
    }

    /// The providers the reader has switched on or off. A provider with no row
    /// has its default (`providers::PROVIDER_DEFAULTS`); a row naming a provider
    /// this build does not know (written by a newer one) is ignored.
    pub async fn provider_settings(&self) -> Result<HashMap<ProviderId, bool>> {
        let rows = sqlx::query("SELECT provider, enabled FROM provider_settings")
            .fetch_all(self.pool())
            .await?;
        Ok(rows
            .iter()
            .filter_map(|r| {
                let p = ProviderId::parse(r.get::<String, _>("provider").as_str())?;
                Some((p, r.get::<bool, _>("enabled")))
            })
            .collect())
    }

    /// Switch a provider on or off.
    pub async fn set_provider_enabled(&self, provider: ProviderId, enabled: bool) -> Result<()> {
        sqlx::query(
            "INSERT INTO provider_settings (provider, enabled, updated_at) VALUES (?, ?, ?)
             ON CONFLICT (provider) DO UPDATE SET
                 enabled = excluded.enabled, updated_at = excluded.updated_at",
        )
        .bind(provider.as_str())
        .bind(enabled)
        .bind(now_unix())
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Move every kept search `by` seconds into the past, for the engine's own
    /// tests and the benchmark's offline run: a kept answer that is no longer
    /// fresh is the one a session refreshes, and nothing else makes one without
    /// waiting a day. Records keep their dates.
    #[cfg(any(test, feature = "internals"))]
    #[doc(hidden)]
    pub async fn age_kept_searches(&self, by: i64) -> Result<()> {
        sqlx::query("UPDATE search_cache SET fetched_at = fetched_at - ?")
            .bind(by)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Every `(table, provider)` pair with a row, for the test that proves a
    /// provider that may not be kept never was.
    #[cfg(any(test, feature = "internals"))]
    #[doc(hidden)]
    pub async fn kept_providers(&self) -> Result<Vec<(String, String)>> {
        let rows = sqlx::query(
            "SELECT DISTINCT 'provider_records' AS t, provider FROM provider_records
             UNION SELECT DISTINCT 'search_cache', provider FROM search_cache
             ORDER BY 1, 2",
        )
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .iter()
            .map(|r| (r.get::<String, _>(0), r.get::<String, _>(1)))
            .collect())
    }
}

async fn purge_in(tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>, now: i64) -> Result<()> {
    sqlx::query("DELETE FROM search_cache WHERE fetched_at <= ?")
        .bind(now - SEARCHES_KEPT_FOR)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM provider_records WHERE expires_at <= ?")
        .bind(now)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(key: &str, kind: RecordKind, title: &str) -> KeptRecord {
        KeptRecord {
            key: key.into(),
            kind,
            form: RecordForm::Doc,
            title: Some(title.into()),
            authors: vec!["Frank Herbert".into()],
            body: format!("{{\"key\":\"{key}\"}}"),
        }
    }

    fn entry(keys: &[&str]) -> KeptEntry {
        KeptEntry {
            records: keys.iter().map(|k| k.to_string()).collect(),
            by_author: false,
        }
    }

    async fn storage() -> Storage {
        Storage::connect("sqlite::memory:").await.unwrap()
    }

    const NOW: i64 = 1_800_000_000;

    #[tokio::test]
    async fn a_kept_answer_reads_back_in_order() {
        let s = storage().await;
        let results = vec![
            (
                entry(&["/works/W1", "/books/M1"]),
                vec![
                    record("/works/W1", RecordKind::Work, "Dune"),
                    record("/books/M1", RecordKind::Edition, "Dune (40th)"),
                ],
            ),
            (
                entry(&["/works/W2"]),
                vec![record("/works/W2", RecordKind::Work, "Dune Messiah")],
            ),
        ];
        s.keep_answer("q1", ProviderId::OpenLibrary, &results, NOW)
            .await
            .unwrap();
        let back = s
            .kept_answer("q1", ProviderId::OpenLibrary, NOW + 60)
            .await
            .unwrap()
            .expect("kept");
        assert_eq!(back.fetched_at, NOW);
        let keys: Vec<Vec<&str>> = back
            .results
            .iter()
            .map(|(_, rows)| rows.iter().map(|r| r.key.as_str()).collect())
            .collect();
        assert_eq!(
            keys,
            vec![vec!["/works/W1", "/books/M1"], vec!["/works/W2"]]
        );
        assert_eq!(back.results[0].1[1].kind, RecordKind::Edition);
        // Another query, another provider: nothing.
        assert!(
            s.kept_answer("q2", ProviderId::OpenLibrary, NOW)
                .await
                .unwrap()
                .is_none()
        );
    }

    /// Thirty days for a search, the provider's term for a record — and a
    /// search naming an expired record is no answer, never half of one.
    #[tokio::test]
    async fn searches_expire_at_thirty_days_and_records_at_their_term() {
        let s = storage().await;
        let results = vec![(
            entry(&["/works/W1"]),
            vec![record("/works/W1", RecordKind::Work, "Dune")],
        )];
        s.keep_answer("q", ProviderId::OpenLibrary, &results, NOW)
            .await
            .unwrap();
        let day = 86_400;
        assert!(
            s.kept_answer("q", ProviderId::OpenLibrary, NOW + 29 * day)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            s.kept_answer("q", ProviderId::OpenLibrary, NOW + 30 * day)
                .await
                .unwrap()
                .is_none(),
            "a search is kept thirty days"
        );
        // The purge deletes the search and keeps the record (180 days).
        s.purge_search_cache(NOW + 31 * day).await.unwrap();
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM search_cache")
            .fetch_one(s.pool())
            .await
            .unwrap();
        assert_eq!(n, 0);
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM provider_records")
            .fetch_one(s.pool())
            .await
            .unwrap();
        assert_eq!(n, 1);

        // A fresh search naming a record past its term is no answer.
        s.keep_answer("q", ProviderId::OpenLibrary, &results, NOW)
            .await
            .unwrap();
        sqlx::query("UPDATE provider_records SET expires_at = ?")
            .bind(NOW + 10)
            .execute(s.pool())
            .await
            .unwrap();
        assert!(
            s.kept_answer("q", ProviderId::OpenLibrary, NOW + 20)
                .await
                .unwrap()
                .is_none()
        );
        s.purge_search_cache(NOW + 20).await.unwrap();
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM provider_records")
            .fetch_one(s.pool())
            .await
            .unwrap();
        assert_eq!(n, 0, "an expired record is purged");
    }

    /// Google Books' terms allow no durable copy: handed to the writer, its
    /// answer is not written — asserted on the tables, not on a return value.
    #[tokio::test]
    async fn a_provider_that_may_not_be_kept_is_never_written() {
        let s = storage().await;
        let results = vec![(
            entry(&["gb1"]),
            vec![record("gb1", RecordKind::Edition, "Dune")],
        )];
        s.keep_answer("q", ProviderId::GoogleBooks, &results, NOW)
            .await
            .unwrap();
        assert!(s.kept_providers().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_offline_index_finds_a_kept_work_by_part_of_a_word() {
        let s = storage().await;
        let results = vec![
            (
                entry(&["/works/W1", "/books/M1"]),
                vec![
                    record("/works/W1", RecordKind::Work, "The Dispossessed"),
                    record("/books/M1", RecordKind::Edition, "The Dispossessed"),
                ],
            ),
            (
                entry(&["/works/W2"]),
                vec![record("/works/W2", RecordKind::Work, "Café Society")],
            ),
        ];
        s.keep_answer("q", ProviderId::OpenLibrary, &results, NOW)
            .await
            .unwrap();
        let words = |w: &[&str]| w.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let found = s
            .kept_records_matching(ProviderId::OpenLibrary, &words(&["possess"]), NOW, 10)
            .await
            .unwrap();
        assert_eq!(found.len(), 1, "works only, not their editions");
        assert_eq!(found[0].key, "/works/W1");
        let found = s
            .kept_records_matching(ProviderId::OpenLibrary, &words(&["cafe"]), NOW, 10)
            .await
            .unwrap();
        assert_eq!(found[0].key, "/works/W2", "diacritics removed");
        let found = s
            .kept_records_matching(ProviderId::OpenLibrary, &words(&["herbert"]), NOW, 10)
            .await
            .unwrap();
        assert_eq!(found.len(), 2, "authors are indexed");
        // Too short for a trigram, and syntax is not syntax.
        for w in [&["du"][..], &["\"dune OR"][..]] {
            assert!(
                s.kept_records_matching(ProviderId::OpenLibrary, &words(w), NOW, 10)
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[tokio::test]
    async fn forgetting_searches_keeps_records_unless_asked_and_never_touches_books() {
        let s = storage().await;
        let book = s
            .upsert_book(
                &crate::book::Book {
                    title: Some("Dune".into()),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
        let results = vec![(
            entry(&["/works/W1"]),
            vec![record("/works/W1", RecordKind::Work, "Dune")],
        )];
        s.keep_answer("q", ProviderId::OpenLibrary, &results, NOW)
            .await
            .unwrap();
        let status = s.search_cache_status().await.unwrap();
        assert!(status.anything_kept);
        assert_eq!(status.oldest_record, Some(NOW));

        s.forget_searches(false).await.unwrap();
        assert!(
            s.kept_answer("q", ProviderId::OpenLibrary, NOW)
                .await
                .unwrap()
                .is_none()
        );
        let status = s.search_cache_status().await.unwrap();
        assert!(status.anything_kept, "the records are still kept");

        s.forget_searches(true).await.unwrap();
        assert_eq!(
            s.search_cache_status().await.unwrap(),
            SearchCacheStatus::default()
        );
        // The index forgot too.
        assert!(
            s.kept_records_matching(ProviderId::OpenLibrary, &["dune".to_string()], NOW, 10)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(s.get_book(book).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn provider_settings_hold_only_what_was_changed() {
        let s = storage().await;
        assert!(s.provider_settings().await.unwrap().is_empty());
        s.set_provider_enabled(ProviderId::OpenLibrary, false)
            .await
            .unwrap();
        s.set_provider_enabled(ProviderId::OpenLibrary, true)
            .await
            .unwrap();
        s.set_provider_enabled(ProviderId::GoogleBooks, false)
            .await
            .unwrap();
        let got = s.provider_settings().await.unwrap();
        assert_eq!(got.get(&ProviderId::OpenLibrary), Some(&true));
        assert_eq!(got.get(&ProviderId::GoogleBooks), Some(&false));
        // A provider a newer build wrote is not an error.
        sqlx::query("INSERT INTO provider_settings VALUES ('wikidata', 1, 0)")
            .execute(s.pool())
            .await
            .unwrap();
        assert_eq!(s.provider_settings().await.unwrap().len(), 2);
    }
}
