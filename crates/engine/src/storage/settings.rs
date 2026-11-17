//! The settings store: a frontend's choices, kept beside the library.
//! The engine stores and does not interpret: it validates only the key alphabet and that the value
//! is JSON. No secrets.

use sqlx::Row;

use super::{Storage, now_unix};
use crate::error::{EngineError, Result};

pub const SETTING_KEY_MAX: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setting {
    pub key: String,
    pub value: String,
}

/// Key alphabet: `[a-z0-9._-]{1,64}`.
pub fn is_setting_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= SETTING_KEY_MAX
        && key.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

fn check_key(key: &str) -> Result<()> {
    if is_setting_key(key) {
        Ok(())
    } else {
        Err(EngineError::InvalidInput(format!(
            "a setting key is 1–{SETTING_KEY_MAX} characters of a-z, 0-9, '.', '_' and '-', not {key:?}"
        )))
    }
}

impl Storage {
    pub(crate) async fn get_setting(&self, key: &str) -> Result<Option<String>> {
        check_key(key)?;
        Ok(
            sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
                .bind(key)
                .fetch_optional(self.pool())
                .await?,
        )
    }

    /// Refuses a bad key or a non-JSON value (`InvalidInput`); the value is stored as written, not
    /// re-serialised.
    pub(crate) async fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        check_key(key)?;
        if serde_json::from_str::<serde_json::Value>(value).is_err() {
            return Err(EngineError::InvalidInput(format!(
                "the value for setting {key:?} is not JSON"
            )));
        }
        sqlx::query(
            "INSERT INTO settings (key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        )
        .bind(key)
        .bind(value)
        .bind(now_unix())
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Prefix is compared with `substr`, not `LIKE`, so `_` is a literal underscore.
    pub(crate) async fn list_settings(&self, prefix: &str) -> Result<Vec<Setting>> {
        let rows = sqlx::query(
            "SELECT key, value FROM settings
              WHERE substr(key, 1, length(?1)) = ?1
              ORDER BY key",
        )
        .bind(prefix)
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|r| {
                Ok(Setting {
                    key: r.try_get("key")?,
                    value: r.try_get("value")?,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_alphabet_is_the_documented_one() {
        for ok in ["history.chart.pages-week", "a", "x_1", &"k".repeat(64)] {
            assert!(is_setting_key(ok), "{ok:?}");
        }
        for bad in ["", "History", "a b", "a/b", "é", &"k".repeat(65)] {
            assert!(!is_setting_key(bad), "{bad:?}");
        }
    }

    #[tokio::test]
    async fn a_setting_round_trips_and_a_prefix_is_literal() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        assert_eq!(s.get_setting("history.overview.cards").await.unwrap(), None);
        s.set_setting("history.overview.cards", r#"["pages","notes"]"#)
            .await
            .unwrap();
        s.set_setting("history.chart.a", "1").await.unwrap();
        s.set_setting("historyxchart", "true").await.unwrap();
        assert_eq!(
            s.get_setting("history.overview.cards")
                .await
                .unwrap()
                .as_deref(),
            Some(r#"["pages","notes"]"#)
        );
        s.set_setting("history.chart.a", "2").await.unwrap();
        let keys: Vec<String> = s
            .list_settings("history.")
            .await
            .unwrap()
            .into_iter()
            .map(|x| x.key)
            .collect();
        assert_eq!(keys, vec!["history.chart.a", "history.overview.cards"]);
        assert_eq!(s.list_settings("").await.unwrap().len(), 3);
        assert!(s.list_settings("history_").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn junk_is_refused_and_writes_nothing() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        assert!(matches!(
            s.set_setting("Bad Key", "1").await,
            Err(EngineError::InvalidInput(_))
        ));
        assert!(matches!(
            s.set_setting("ok", "not json").await,
            Err(EngineError::InvalidInput(_))
        ));
        assert!(s.list_settings("").await.unwrap().is_empty());
    }
}
