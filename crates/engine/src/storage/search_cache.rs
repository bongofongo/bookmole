//! Kept provider answers and searches, and which providers are on.
//! Records are kept for the provider's term; searches thirty days, under a hash of the query, never
//! its text.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sqlx::Row;

use super::{Storage, now_unix};
use crate::error::Result;
use crate::providers::{KeptRecord, ProviderId, RecordForm, RecordKind};

pub const SEARCHES_KEPT_FOR: i64 = 30 * 86_400;

/// One result of a kept search. `by_author` is a flag, not the name: the query is never stored,
/// being private reading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct KeptEntry {
    #[serde(rename = "r")]
    pub records: Vec<String>,
    #[serde(rename = "a", default, skip_serializing_if = "std::ops::Not::not")]
    pub by_author: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeptRow {
    pub key: String,
    pub kind: RecordKind,
    pub form: RecordForm,
    pub body: String,
    pub fetched_at: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct KeptAnswer {
    pub fetched_at: i64,
    pub results: Vec<(KeptEntry, Vec<KeptRow>)>,
}

/// No count of searches: that would be a history in all but name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SearchCacheStatus {
    pub anything_kept: bool,
    /// Fetch time of the oldest kept record (unix seconds).
    pub oldest_record: Option<i64>,
}

fn kind_of(word: &str) -> RecordKind {
    if word == "work" {
        RecordKind::Work
    } else {
        RecordKind::Edition
    }
}

/// `None` for a form this build does not know.
fn row_of(r: &sqlx::sqlite::SqliteRow) -> Result<Option<KeptRow>> {
    let key = r.try_get("key")?;
    let kind = kind_of(r.try_get::<String, _>("kind")?.as_str());
    let Some(form) = RecordForm::parse(r.try_get::<String, _>("form")?.as_str()) else {
        return Ok(None);
    };
    Ok(Some(KeptRow {
        key,
        kind,
        form,
        body: r.try_get("body")?,
        fetched_at: r.try_get("fetched_at")?,
    }))
}

fn phrase(word: &str) -> String {
    format!("\"{}\"", word.replace('"', "\"\""))
}

impl Storage {
    /// `None` if a named record has expired: a partial list read as a whole one silently drops rows.
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
        let fetched_at: i64 = row.try_get("fetched_at")?;
        let Ok(entries) =
            serde_json::from_str::<Vec<KeptEntry>>(&row.try_get::<String, _>("results")?)
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
        let kept: Vec<KeptRow> = rows
            .iter()
            .map(row_of)
            .filter_map(Result::transpose)
            .collect::<Result<_>>()?;
        let by_key: HashMap<String, KeptRow> =
            kept.into_iter().map(|r| (r.key.clone(), r)).collect();
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

    /// Refuses a provider whose records may not be kept (no write, `debug!` only).
    /// One transaction, so a kept search never names an unwritten record.
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

    /// Words under three characters cannot use the trigram index and are dropped; with none left
    /// nothing is returned.
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
        rows.iter()
            .map(row_of)
            .filter_map(Result::transpose)
            .collect()
    }

    pub(crate) async fn library_language(&self) -> Result<Option<String>> {
        Ok(sqlx::query_scalar(
            "SELECT language FROM books WHERE language IS NOT NULL AND language <> ''
              GROUP BY language ORDER BY count(*) DESC, language LIMIT 1",
        )
        .fetch_optional(self.pool())
        .await?)
    }

    pub(crate) async fn drop_kept_record(&self, provider: ProviderId, key: &str) -> Result<()> {
        sqlx::query("DELETE FROM provider_records WHERE provider = ? AND key = ?")
            .bind(provider.as_str())
            .bind(key)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub(crate) async fn purge_search_cache(&self, now: i64) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        purge_in(&mut tx, now).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Empty the search cache; with `records`, the kept records and their index too. Never touches a
    /// book.
    pub async fn forget_searches(&self, records: bool) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("DELETE FROM search_cache")
            .execute(&mut *tx)
            .await?;
        if records {
            sqlx::query("DELETE FROM provider_records")
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        tracing::info!(records, "kept searches forgotten");
        Ok(())
    }

    pub async fn search_cache_status(&self) -> Result<SearchCacheStatus> {
        let row = sqlx::query(
            "SELECT (SELECT min(fetched_at) FROM provider_records) AS oldest,
                    EXISTS (SELECT 1 FROM search_cache) AS searches",
        )
        .fetch_one(self.pool())
        .await?;
        let oldest: Option<i64> = row.try_get("oldest")?;
        let searches: bool = row.try_get("searches")?;
        Ok(SearchCacheStatus {
            anything_kept: searches || oldest.is_some(),
            oldest_record: oldest,
        })
    }

    /// A provider with no row has its default; a row naming an unknown provider (a newer build's) is
    /// ignored.
    pub async fn provider_settings(&self) -> Result<HashMap<ProviderId, bool>> {
        let rows = sqlx::query("SELECT provider, enabled FROM provider_settings")
            .fetch_all(self.pool())
            .await?;
        let mut settings = HashMap::new();
        for r in &rows {
            if let Some(p) = ProviderId::parse(r.try_get::<String, _>("provider")?.as_str()) {
                settings.insert(p, r.try_get::<bool, _>("enabled")?);
            }
        }
        Ok(settings)
    }

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

    /// Test and benchmark hook: moves every kept search `by` seconds into the past; records keep their
    /// dates.
    #[cfg(any(test, feature = "internals"))]
    #[doc(hidden)]
    pub async fn age_kept_searches(&self, by: i64) -> Result<()> {
        sqlx::query("UPDATE search_cache SET fetched_at = fetched_at - ?")
            .bind(by)
            .execute(self.pool())
            .await?;
        Ok(())
    }

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
        rows.iter()
            .map(|r| Ok((r.try_get::<String, _>(0)?, r.try_get::<String, _>(1)?)))
            .collect()
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
        assert!(
            s.kept_answer("q2", ProviderId::OpenLibrary, NOW)
                .await
                .unwrap()
                .is_none()
        );
    }

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
        sqlx::query("INSERT INTO provider_settings VALUES ('wikidata', 1, 0)")
            .execute(s.pool())
            .await
            .unwrap();
        assert_eq!(s.provider_settings().await.unwrap().len(), 2);
    }
}
