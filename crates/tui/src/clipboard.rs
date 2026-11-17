//! System clipboard access via `arboard`, so the API-key box can bind a plain Ctrl+V.

use anyhow::{Context, Result};

pub fn read() -> Result<String> {
    let mut clipboard = arboard::Clipboard::new().context("open clipboard")?;
    let text = clipboard.get_text().context("read clipboard")?;
    Ok(text.trim().to_string())
}
