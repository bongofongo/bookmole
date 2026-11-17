use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub db_url: String,
    pub images_dir: PathBuf,
    pub vault_dir: PathBuf,
    /// Content-addressed: `<files_dir>/<ab>/<sha256>.<ext>`. Not beside the vault, which is the
    /// user's to edit and sync.
    pub files_dir: PathBuf,
    /// Under the data root, not `$XDG_STATE_HOME`, so a sandbox run with `--data-dir` logs into the
    /// sandbox.
    pub log_dir: PathBuf,
    /// Files written for other software (the Anki export). Beside the vault, not in it, or exports
    /// show up as notes.
    pub exports_dir: PathBuf,
    /// Without a key Google Books is not asked at all: keyless requests are refused with a 429.
    pub google_api_key: Option<String>,
    /// Searched before `PATH` and the known install dirs; lets tests use fake binaries without
    /// mutating `PATH`.
    pub calibre_bin_dir: Option<PathBuf>,
}

impl EngineConfig {
    pub fn rooted_at(data_root: impl Into<PathBuf>) -> Self {
        let root: PathBuf = data_root.into();
        EngineConfig {
            db_url: format!("sqlite://{}", root.join("database/app.db").display()),
            images_dir: root.join("database/images"),
            files_dir: root.join("database/files"),
            vault_dir: root.join("vault"),
            log_dir: root.join("logs"),
            exports_dir: root.join("exports"),
            google_api_key: std::env::var("GOOGLE_BOOKS_API_KEY").ok(),
            calibre_bin_dir: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rooted_at_puts_everything_under_the_given_root() {
        let c = EngineConfig::rooted_at("/tmp/rb");
        assert_eq!(c.db_url, "sqlite:///tmp/rb/database/app.db");
        assert_eq!(c.images_dir, PathBuf::from("/tmp/rb/database/images"));
        assert_eq!(c.files_dir, PathBuf::from("/tmp/rb/database/files"));
        assert_eq!(c.vault_dir, PathBuf::from("/tmp/rb/vault"));
        assert_eq!(c.log_dir, PathBuf::from("/tmp/rb/logs"));
        assert_eq!(c.exports_dir, PathBuf::from("/tmp/rb/exports"));
    }

    #[test]
    fn a_relative_root_stays_relative() {
        let c = EngineConfig::rooted_at(".");
        assert_eq!(c.db_url, "sqlite://./database/app.db");
        assert_eq!(c.log_dir, PathBuf::from("./logs"));
    }
}
