//! `sort-keys`: back-fills the derived sort keys (`sort_title`, `sort_author`) SQLite cannot
//! compute. Idempotent: only rows never computed (`sort_author IS NULL`) are filled.

use anyhow::Result;
use bookmole::Engine;

pub async fn rebuild(engine: &Engine) -> Result<()> {
    let filed = engine.rebuild_sort_keys().await?;
    println!("{}", summary(filed));
    Ok(())
}

/// Pure so the wording is testable; never prints `filed 0 books` or a count of pending work.
fn summary(filed: usize) -> String {
    match filed {
        0 => "every book already files under a sort key.".to_string(),
        1 => "filed 1 book.".to_string(),
        n => format!("filed {n} books."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_already_filed_library_is_never_reported_as_zero() {
        let s = summary(0);
        assert!(!s.contains('0'), "absence is not zero: {s}");
        assert!(s.contains("already files"), "{s}");
    }

    #[test]
    fn the_count_is_of_what_it_did() {
        assert_eq!(summary(1), "filed 1 book.");
        assert_eq!(summary(7), "filed 7 books.");
        for s in [summary(0), summary(1), summary(7)] {
            for banned in ["remaining", "left", "still", "waiting", "of "] {
                assert!(!s.contains(banned), "completion framing in {s:?}: {banned}");
            }
        }
    }
}
