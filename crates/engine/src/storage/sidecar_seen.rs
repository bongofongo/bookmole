//! The device-scan pre-filter cache: every column is something the file said, never a verdict about
//! the library.

use sqlx::Row;

use super::Storage;
use crate::error::Result;

/// `size` and `mtime` are the freshness key. Highlights are deliberately not stored: that would be
/// a second copy.
#[derive(Debug, Clone, PartialEq)]
pub struct SidecarFacts {
    pub size: i64,
    /// Nanoseconds since the epoch.
    pub mtime: i64,
    pub partial_md5: Option<String>,
    pub entry_count: i64,
    /// The file's annotations through `DeviceDigest`; a count alone could not detect an edit.
    pub entry_digest: String,
    /// `doc_props.title` as the matcher sees it, so a cache hit reproduces a fresh parse's match.
    pub title: Option<String>,
    pub authors: Option<String>,
    pub ko_percent: Option<f64>,
    /// Kept as text so a status we do not model round-trips unchanged.
    pub ko_status: Option<String>,
}

impl Storage {
    /// Returns the row whatever its `size`/`mtime`; freshness is the caller's call.
    pub async fn sidecar_facts(&self, path: &str) -> Result<Option<SidecarFacts>> {
        let row = sqlx::query(
            "SELECT size, mtime, partial_md5, entry_count, entry_digest, title, authors,
                    ko_percent, ko_status
             FROM sidecar_seen WHERE path = ?",
        )
        .bind(path)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.map(|r| SidecarFacts {
            size: r.get("size"),
            mtime: r.get("mtime"),
            partial_md5: r.get("partial_md5"),
            entry_count: r.get("entry_count"),
            entry_digest: r.get("entry_digest"),
            title: r.get("title"),
            authors: r.get("authors"),
            ko_percent: r.get("ko_percent"),
            ko_status: r.get("ko_status"),
        }))
    }

    /// Overwrites in full: a sidecar is the complete state, so a vanished field is gone.
    /// `seen_at` is the scan's token, not the clock.
    pub async fn record_sidecar_facts(
        &self,
        path: &str,
        f: &SidecarFacts,
        seen_at: i64,
    ) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO sidecar_seen
                 (path, size, mtime, partial_md5, entry_count, entry_digest, title, authors,
                  ko_percent, ko_status, last_seen)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
               ON CONFLICT(path) DO UPDATE SET
                   size         = excluded.size,
                   mtime        = excluded.mtime,
                   partial_md5  = excluded.partial_md5,
                   entry_count  = excluded.entry_count,
                   entry_digest = excluded.entry_digest,
                   title       = excluded.title,
                   authors     = excluded.authors,
                   ko_percent  = excluded.ko_percent,
                   ko_status   = excluded.ko_status,
                   last_seen   = excluded.last_seen"#,
        )
        .bind(path)
        .bind(f.size)
        .bind(f.mtime)
        .bind(f.partial_md5.as_ref())
        .bind(f.entry_count)
        .bind(&f.entry_digest)
        .bind(f.title.as_ref())
        .bind(f.authors.as_ref())
        .bind(f.ko_percent)
        .bind(f.ko_status.as_ref())
        .bind(seen_at)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// A cache hit must still be stamped seen, or the prune would delete the rows doing their job.
    pub async fn touch_sidecar_seen(&self, path: &str, seen_at: i64) -> Result<()> {
        sqlx::query("UPDATE sidecar_seen SET last_seen = ? WHERE path = ?")
            .bind(seen_at)
            .bind(path)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Drops rows under `root` that this scan did not see; other mounts' rows are left alone.
    /// `seen_at` is a token compared with `<>`, not `<`: at second resolution consecutive scans tie.
    /// The prefix test uses `substr`, not `LIKE`, since mount names may contain `%` or `_`.
    pub async fn forget_sidecars_under(&self, root: &str, seen_at: i64) -> Result<u64> {
        let prefix = format!("{}/", root.trim_end_matches('/'));
        let done =
            sqlx::query("DELETE FROM sidecar_seen WHERE substr(path, 1, ?) = ? AND last_seen <> ?")
                .bind(prefix.chars().count() as i64)
                .bind(&prefix)
                .bind(seen_at)
                .execute(self.pool())
                .await?;
        Ok(done.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::now_unix;

    fn facts(entry_count: i64) -> SidecarFacts {
        SidecarFacts {
            size: 100,
            mtime: 1_700_000_000_000_000_000,
            partial_md5: Some("abc".into()),
            entry_count,
            entry_digest: format!("digest-of-{entry_count}"),
            title: Some("Middlemarch".into()),
            authors: Some("George Eliot".into()),
            ko_percent: Some(0.5),
            ko_status: Some("reading".into()),
        }
    }

    const FIRST: i64 = 1_000;
    const SECOND: i64 = 2_000;

    #[tokio::test]
    async fn facts_round_trip_and_a_rewrite_replaces_them() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        assert!(s.sidecar_facts("/x").await.unwrap().is_none());

        s.record_sidecar_facts("/x", &facts(3), FIRST)
            .await
            .unwrap();
        assert_eq!(s.sidecar_facts("/x").await.unwrap(), Some(facts(3)));

        let mut second = facts(4);
        second.ko_status = None;
        second.title = None;
        s.record_sidecar_facts("/x", &second, SECOND).await.unwrap();
        assert_eq!(s.sidecar_facts("/x").await.unwrap(), Some(second));

        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM sidecar_seen")
            .fetch_one(s.pool())
            .await
            .unwrap();
        assert_eq!(n, 1);
    }

    #[tokio::test]
    async fn a_prune_forgets_only_unseen_rows_under_its_own_root() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        for p in ["/mnt/a/one.lua", "/mnt/a/two.lua", "/mnt/b/three.lua"] {
            s.record_sidecar_facts(p, &facts(1), FIRST).await.unwrap();
        }
        s.touch_sidecar_seen("/mnt/a/one.lua", SECOND)
            .await
            .unwrap();

        assert_eq!(s.forget_sidecars_under("/mnt/a", SECOND).await.unwrap(), 1);
        assert!(s.sidecar_facts("/mnt/a/one.lua").await.unwrap().is_some());
        assert!(s.sidecar_facts("/mnt/a/two.lua").await.unwrap().is_none());
        assert!(
            s.sidecar_facts("/mnt/b/three.lua").await.unwrap().is_some(),
            "a scan of one mount must not forget another device"
        );
    }

    #[tokio::test]
    async fn a_prune_is_not_defeated_by_two_scans_in_the_same_second() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let one_second = now_unix();
        s.record_sidecar_facts("/mnt/a/gone.lua", &facts(1), one_second)
            .await
            .unwrap();
        assert_eq!(
            s.forget_sidecars_under("/mnt/a", one_second).await.unwrap(),
            0,
            "same token: this scan did see it"
        );
        assert_eq!(
            s.forget_sidecars_under("/mnt/a", one_second + 1)
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn a_sibling_mount_with_a_longer_name_is_not_under_this_root() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        s.record_sidecar_facts("/mnt/abc/one.lua", &facts(1), FIRST)
            .await
            .unwrap();
        assert_eq!(s.forget_sidecars_under("/mnt/a", SECOND).await.unwrap(), 0);
        assert!(s.sidecar_facts("/mnt/abc/one.lua").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn a_wildcard_in_a_mount_name_does_not_widen_the_prune() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        s.record_sidecar_facts("/mnt/b/three.lua", &facts(1), FIRST)
            .await
            .unwrap();
        assert_eq!(s.forget_sidecars_under("/mnt/%", SECOND).await.unwrap(), 0);
        assert!(s.sidecar_facts("/mnt/b/three.lua").await.unwrap().is_some());
    }
}
