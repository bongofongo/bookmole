//! Paired readers the plugin is installed on. Identity is `device_id`; `last_mount_path` is a
//! breadcrumb, never a key.

use super::{Storage, now_unix};
use crate::error::Result;
use sqlx::Row;

/// `Debug` is hand-written so `token` never reaches a log.
#[derive(Clone, PartialEq, Eq)]
pub struct PairedDevice {
    pub device_id: String,
    pub label: Option<String>,
    pub token: String,
    pub plugin_version: i64,
    pub installed_at: i64,
    pub last_mount_path: Option<String>,
    pub last_seen_at: Option<i64>,
    /// `None` means not since recording began, not never synced.
    pub last_synced_at: Option<i64>,
    /// Last network contact, distinct from `last_seen_at` (cable); `None` means not since recording
    /// began.
    pub last_wireless_at: Option<i64>,
    /// A breadcrumb: nothing may join on it, since a lease moves.
    pub last_lan_addr: Option<String>,
}

impl std::fmt::Debug for PairedDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairedDevice")
            .field("device_id", &self.device_id)
            .field("label", &self.label)
            .field("token", &"<redacted>")
            .field("plugin_version", &self.plugin_version)
            .field("installed_at", &self.installed_at)
            .field("last_mount_path", &self.last_mount_path)
            .field("last_seen_at", &self.last_seen_at)
            .field("last_synced_at", &self.last_synced_at)
            .field("last_wireless_at", &self.last_wireless_at)
            .field("last_lan_addr", &self.last_lan_addr)
            .finish()
    }
}

const COLUMNS: &str = "device_id, label, token, plugin_version, installed_at, \
                       last_mount_path, last_seen_at, last_synced_at, \
                       last_wireless_at, last_lan_addr";

fn row_to_device(row: &sqlx::sqlite::SqliteRow) -> Result<PairedDevice> {
    Ok(PairedDevice {
        device_id: row.try_get("device_id")?,
        label: row.try_get("label")?,
        token: row.try_get("token")?,
        plugin_version: row.try_get("plugin_version")?,
        installed_at: row.try_get("installed_at")?,
        last_mount_path: row.try_get("last_mount_path")?,
        last_seen_at: row.try_get("last_seen_at")?,
        last_synced_at: row.try_get("last_synced_at")?,
        last_wireless_at: row.try_get("last_wireless_at")?,
        last_lan_addr: row.try_get("last_lan_addr")?,
    })
}

impl Storage {
    /// An upgrade keeps the token and `installed_at`; an existing label wins over the mount name;
    /// `paired_at` comes from the caller so the device file and the row share one instant.
    pub async fn record_pairing(
        &self,
        device_id: &str,
        label: Option<&str>,
        token: &str,
        plugin_version: i64,
        mount_path: Option<&str>,
        paired_at: i64,
    ) -> Result<()> {
        let now = now_unix();
        sqlx::query(
            "INSERT INTO paired_devices
                 (device_id, label, token, plugin_version, installed_at,
                  last_mount_path, last_seen_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(device_id) DO UPDATE SET
                 label           = COALESCE(paired_devices.label, excluded.label),
                 plugin_version  = excluded.plugin_version,
                 last_mount_path = excluded.last_mount_path,
                 last_seen_at    = excluded.last_seen_at",
        )
        .bind(device_id)
        .bind(label)
        .bind(token)
        .bind(plugin_version)
        .bind(paired_at)
        .bind(mount_path)
        .bind(now)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Wireless pairing stamps neither `last_seen_at` nor a mount path, and re-keys the token because
    /// it is a real re-pairing.
    pub async fn record_wireless_pairing(
        &self,
        device_id: &str,
        label: Option<&str>,
        token: &str,
        plugin_version: i64,
        paired_at: i64,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO paired_devices
                 (device_id, label, token, plugin_version, installed_at)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(device_id) DO UPDATE SET
                 label          = COALESCE(paired_devices.label, excluded.label),
                 token          = excluded.token,
                 plugin_version = excluded.plugin_version",
        )
        .bind(device_id)
        .bind(label)
        .bind(token)
        .bind(plugin_version)
        .bind(paired_at)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn paired_device(&self, device_id: &str) -> Result<Option<PairedDevice>> {
        let sql = format!("SELECT {COLUMNS} FROM paired_devices WHERE device_id = ?");
        let row = sqlx::query(&sql)
            .bind(device_id)
            .fetch_optional(self.pool())
            .await?;
        row.as_ref().map(row_to_device).transpose()
    }

    pub async fn list_paired_devices(&self) -> Result<Vec<PairedDevice>> {
        let sql = format!(
            "SELECT {COLUMNS} FROM paired_devices
             ORDER BY COALESCE(last_seen_at, installed_at) DESC, device_id"
        );
        let rows = sqlx::query(&sql).fetch_all(self.pool()).await?;
        rows.iter().map(row_to_device).collect()
    }

    /// Returns whether a row existed; `false` is ordinary (a reader paired with another copy of
    /// bookmole).
    pub async fn touch_device_seen(
        &self,
        device_id: &str,
        mount_path: Option<&str>,
    ) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE paired_devices
                SET last_seen_at    = ?,
                    last_mount_path = COALESCE(?, last_mount_path)
              WHERE device_id = ?",
        )
        .bind(now_unix())
        .bind(mount_path)
        .bind(device_id)
        .execute(self.pool())
        .await?;
        Ok(done.rows_affected() > 0)
    }

    /// Separate from `touch_device_seen`: plugging a reader in to charge is not a sync.
    pub async fn stamp_device_sync(&self, device_id: &str) -> Result<bool> {
        let done = sqlx::query("UPDATE paired_devices SET last_synced_at = ? WHERE device_id = ?")
            .bind(now_unix())
            .bind(device_id)
            .execute(self.pool())
            .await?;
        Ok(done.rows_affected() > 0)
    }

    /// Stamps no sync and not `last_seen_at`; `addr` of `None` keeps the previous hint. Returns
    /// whether a row existed.
    pub async fn stamp_wireless_contact(
        &self,
        device_id: &str,
        addr: Option<&str>,
    ) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE paired_devices
                SET last_wireless_at = ?,
                    last_lan_addr    = COALESCE(?, last_lan_addr)
              WHERE device_id = ?",
        )
        .bind(now_unix())
        .bind(addr)
        .bind(device_id)
        .execute(self.pool())
        .await?;
        Ok(done.rows_affected() > 0)
    }

    /// An empty or whitespace-only label clears it rather than storing a blank.
    pub async fn set_device_label(&self, device_id: &str, label: &str) -> Result<bool> {
        let trimmed = label.trim();
        let value = (!trimmed.is_empty()).then_some(trimmed);
        let done = sqlx::query("UPDATE paired_devices SET label = ? WHERE device_id = ?")
            .bind(value)
            .bind(device_id)
            .execute(self.pool())
            .await?;
        Ok(done.rows_affected() > 0)
    }

    pub async fn forget_pairing(&self, device_id: &str) -> Result<bool> {
        let done = sqlx::query("DELETE FROM paired_devices WHERE device_id = ?")
            .bind(device_id)
            .execute(self.pool())
            .await?;
        Ok(done.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use crate::storage::Storage;

    async fn store() -> Storage {
        Storage::connect("sqlite::memory:").await.unwrap()
    }

    #[tokio::test]
    async fn a_pairing_round_trips() {
        let s = store().await;
        s.record_pairing(
            "dev-1",
            Some("KOBOeReader"),
            "tok",
            1,
            Some("/mnt/k"),
            1_700_000_000,
        )
        .await
        .unwrap();
        let d = s.paired_device("dev-1").await.unwrap().unwrap();
        assert_eq!(d.label.as_deref(), Some("KOBOeReader"));
        assert_eq!(d.token, "tok");
        assert_eq!(d.plugin_version, 1);
        assert_eq!(d.last_mount_path.as_deref(), Some("/mnt/k"));
    }

    #[tokio::test]
    async fn an_upgrade_keeps_the_token_and_the_install_date() {
        let s = store().await;
        s.record_pairing(
            "dev-1",
            Some("KOBOeReader"),
            "tok",
            1,
            Some("/mnt/k"),
            1_700_000_000,
        )
        .await
        .unwrap();
        let first = s.paired_device("dev-1").await.unwrap().unwrap();

        s.record_pairing(
            "dev-1",
            None,
            "a-different-token",
            2,
            Some("/mnt/other"),
            1_700_086_400,
        )
        .await
        .unwrap();
        let second = s.paired_device("dev-1").await.unwrap().unwrap();

        assert_eq!(second.token, first.token, "an upgrade is not a re-pairing");
        assert_eq!(second.installed_at, first.installed_at);
        assert_eq!(second.plugin_version, 2);
        assert_eq!(second.last_mount_path.as_deref(), Some("/mnt/other"));
        // A label is not un-set by an install that could not read one.
        assert_eq!(second.label.as_deref(), Some("KOBOeReader"));
    }

    #[tokio::test]
    async fn a_rename_survives_a_plugin_upgrade() {
        let s = store().await;
        s.record_pairing(
            "dev-1",
            Some("KOBOeReader"),
            "tok",
            1,
            Some("/mnt/k"),
            1_700_000_000,
        )
        .await
        .unwrap();
        assert!(
            s.set_device_label("dev-1", "the bedside Kobo")
                .await
                .unwrap()
        );

        s.record_pairing(
            "dev-1",
            Some("KOBOeReader"),
            "tok",
            2,
            Some("/mnt/k"),
            1_700_000_000,
        )
        .await
        .unwrap();

        let d = s.paired_device("dev-1").await.unwrap().unwrap();
        assert_eq!(
            d.label.as_deref(),
            Some("the bedside Kobo"),
            "an upgrade must not restore the mount's directory name over a rename"
        );
        assert_eq!(d.plugin_version, 2, "and the upgrade still happened");
    }

    #[tokio::test]
    async fn a_blank_name_clears_rather_than_stores() {
        let s = store().await;
        s.record_pairing("dev-1", Some("Kindle"), "tok", 1, None, 1_700_000_000)
            .await
            .unwrap();
        assert!(s.set_device_label("dev-1", "   ").await.unwrap());
        assert_eq!(s.paired_device("dev-1").await.unwrap().unwrap().label, None);

        s.record_pairing("dev-1", Some("Kindle"), "tok", 1, None, 1_700_000_000)
            .await
            .unwrap();
        assert_eq!(
            s.paired_device("dev-1")
                .await
                .unwrap()
                .unwrap()
                .label
                .as_deref(),
            Some("Kindle")
        );
    }

    #[tokio::test]
    async fn seeing_a_device_is_not_syncing_with_it() {
        let s = store().await;
        s.record_pairing("dev-1", None, "tok", 1, Some("/mnt/k"), 1_700_000_000)
            .await
            .unwrap();
        assert_eq!(
            s.paired_device("dev-1")
                .await
                .unwrap()
                .unwrap()
                .last_synced_at,
            None
        );

        assert!(
            s.touch_device_seen("dev-1", Some("/mnt/elsewhere"))
                .await
                .unwrap()
        );
        let seen = s.paired_device("dev-1").await.unwrap().unwrap();
        assert_eq!(
            seen.last_synced_at, None,
            "plugging a reader in to charge it is not a sync"
        );
        assert_eq!(seen.last_mount_path.as_deref(), Some("/mnt/elsewhere"));
        assert_eq!(
            seen.installed_at, 1_700_000_000,
            "and the pairing did not restart"
        );

        assert!(s.stamp_device_sync("dev-1").await.unwrap());
        assert!(
            s.paired_device("dev-1")
                .await
                .unwrap()
                .unwrap()
                .last_synced_at
                .is_some()
        );
    }

    #[tokio::test]
    async fn reaching_us_over_the_network_is_neither_a_cable_nor_a_sync() {
        let s = store().await;
        s.record_pairing("dev-1", None, "tok", 1, Some("/mnt/k"), 1_700_000_000)
            .await
            .unwrap();
        let before = s.paired_device("dev-1").await.unwrap().unwrap();
        assert_eq!(before.last_wireless_at, None);
        assert_eq!(before.last_lan_addr, None);

        assert!(
            s.stamp_wireless_contact("dev-1", Some("192.0.2.20"))
                .await
                .unwrap()
        );
        let after = s.paired_device("dev-1").await.unwrap().unwrap();
        assert!(after.last_wireless_at.is_some());
        assert_eq!(after.last_lan_addr.as_deref(), Some("192.0.2.20"));
        assert_eq!(
            after.last_seen_at, before.last_seen_at,
            "a push from the next room is not a reader in your hand"
        );
        assert_eq!(
            after.last_synced_at, None,
            "what it chose to send is not everything it had"
        );
        assert_eq!(
            after.last_mount_path.as_deref(),
            Some("/mnt/k"),
            "and nothing over the wire says where a cable last was"
        );

        assert!(s.stamp_wireless_contact("dev-1", None).await.unwrap());
        assert_eq!(
            s.paired_device("dev-1")
                .await
                .unwrap()
                .unwrap()
                .last_lan_addr
                .as_deref(),
            Some("192.0.2.20")
        );
    }

    #[tokio::test]
    async fn a_device_we_never_paired_with_stamps_nothing() {
        let s = store().await;
        assert!(!s.touch_device_seen("dev-x", Some("/mnt/k")).await.unwrap());
        assert!(!s.stamp_device_sync("dev-x").await.unwrap());
        assert!(!s.set_device_label("dev-x", "nope").await.unwrap());
        assert!(
            !s.stamp_wireless_contact("dev-x", Some("203.0.113.1"))
                .await
                .unwrap()
        );
        assert!(s.list_paired_devices().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn forgetting_says_whether_there_was_anything_to_forget() {
        let s = store().await;
        s.record_pairing("dev-1", None, "tok", 1, None, 1_700_000_000)
            .await
            .unwrap();
        assert!(s.forget_pairing("dev-1").await.unwrap());
        assert!(!s.forget_pairing("dev-1").await.unwrap());
        assert!(s.paired_device("dev-1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn the_token_is_not_in_the_debug_output() {
        let s = store().await;
        s.record_pairing("dev-1", None, "super-secret", 1, None, 1_700_000_000)
            .await
            .unwrap();
        let d = s.paired_device("dev-1").await.unwrap().unwrap();
        let printed = format!("{d:?}");
        assert!(
            !printed.contains("super-secret"),
            "a derived Debug is how the token reaches a log: {printed}"
        );
    }
}
