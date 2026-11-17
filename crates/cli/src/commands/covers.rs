//! A command, not a migration, because SQLite cannot decode a PNG; unmeasured covers are retried.

use anyhow::Result;
use bookmole::Engine;

pub async fn measure(engine: &Engine) -> Result<()> {
    let (measured, unreadable) = engine.measure_stored_covers().await?;
    println!("{}", summary(measured, unreadable));
    Ok(())
}

/// Pure so its wording is testable; never prints `measured 0 covers` or a count of pending work.
fn summary(measured: usize, unreadable: usize) -> String {
    let covers = |n: usize| if n == 1 { "cover" } else { "covers" };
    let retry = "left as they were, and the next run tries them again";
    match (measured, unreadable) {
        (0, 0) => "every stored cover is already measured.".to_string(),
        (m, 0) => format!("measured {m} {}.", covers(m)),
        (0, u) => format!("{u} {} would not decode — {retry}.", covers(u)),
        (m, u) => format!(
            "measured {m} {}; {u} would not decode — {retry}.",
            covers(m)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_already_measured_library_is_never_reported_as_zero() {
        let s = summary(0, 0);
        assert!(!s.contains('0'), "absence is not zero: {s}");
        assert!(s.contains("already measured"), "{s}");
    }

    #[test]
    fn an_unreadable_cover_states_what_happened_to_it() {
        for s in [summary(0, 1), summary(3, 2)] {
            assert!(s.contains("would not decode"), "{s}");
            assert!(s.contains("next run tries them again"), "{s}");
        }
        assert!(summary(3, 0).contains("measured 3 covers"));
        assert!(summary(1, 0).contains("measured 1 cover"));
    }
}
