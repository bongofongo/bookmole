//! The engine installs nothing itself; a frontend calls [`install_hook`].

use std::backtrace::Backtrace;
use std::io::Write;
use std::panic::Location;
use std::path::{Path, PathBuf};

const MAX_LOG_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone)]
pub struct CrashContext {
    pub app: &'static str,
    pub version: &'static str,
    pub log_dir: PathBuf,
    /// The path, not a tail of events: a ring buffer needs a lock, and a panic hook must not take one.
    pub log_file: Option<PathBuf>,
}

/// Takes the panic's parts, not a `&PanicHookInfo`, which cannot be constructed outside `std` and
/// so could not be tested.
pub fn format_report(
    timestamp: &str,
    msg: &str,
    location: Option<&Location<'_>>,
    backtrace: &str,
    thread: &str,
    ctx: &CrashContext,
) -> String {
    let loc = location
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
        .unwrap_or_else(|| "<unknown>".to_string());

    let mut out = String::new();
    out.push_str(&format!("time:      {timestamp}\n"));
    out.push_str(&format!("app:       {} {}\n", ctx.app, ctx.version));
    out.push_str(&format!(
        "platform:  {} {}\n",
        std::env::consts::OS,
        std::env::consts::ARCH
    ));
    out.push_str(&format!("thread:    {thread}\n"));
    out.push_str(&format!("location:  {loc}\n"));
    if let Some(f) = &ctx.log_file {
        out.push_str(&format!("log:       {}\n", f.display()));
    }
    out.push_str(&format!("panic:     {msg}\n"));
    out.push_str("backtrace:\n");
    for line in backtrace.lines() {
        out.push_str("  ");
        out.push_str(line);
        out.push('\n');
    }

    // A panic message can carry a URL with an API key, so the whole report goes through the scrubber.
    crate::providers::googlebooks::scrub_key(&out)
}

/// Every step is best-effort: a panic inside a panic hook aborts the process and destroys the
/// report.
fn append_report(log_dir: &Path, report: &str) -> Option<PathBuf> {
    std::fs::create_dir_all(log_dir).ok()?;
    let path = log_dir.join("crash.log");

    if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > MAX_LOG_BYTES {
        let _ = std::fs::rename(&path, log_dir.join("crash.log.1"));
    }

    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;
    let _ = f.write_all(b"\n--- crash ---\n");
    let _ = f.write_all(report.as_bytes());
    let _ = f.flush();
    Some(path)
}

pub fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

/// Install before the terminal enters raw mode: `set_hook` chains via `take_hook`, so the last
/// hook installed runs first, and the order on panic must be the TUI's restore hook, then this
/// one.
pub fn install_hook(ctx: CrashContext) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = panic_message(info.payload());
        let thread = std::thread::current()
            .name()
            .unwrap_or("<unnamed>")
            .to_string();
        // `force_capture` so it works without RUST_BACKTRACE; release builds must keep debug info or it
        // is bare addresses.
        let bt = Backtrace::force_capture().to_string();
        let ts = timestamp();

        let report = format_report(&ts, &msg, info.location(), &bt, &thread, &ctx);
        if let Some(path) = append_report(&ctx.log_dir, &report) {
            // Without this line the report is written and never found; it runs after the TUI has restored the
            // terminal.
            announce(&path);
        }
        previous(info);
    }));
}

#[allow(clippy::print_stderr)]
fn announce(path: &Path) {
    eprintln!("\nbookmole crashed. Report written to {}", path.display());
}

fn timestamp() -> String {
    use time::format_description::well_known::Rfc3339;
    time::OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "<unknown time>".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> CrashContext {
        CrashContext {
            app: "bookmole-tui",
            version: "0.1.0",
            log_dir: PathBuf::from("/tmp/rb/logs"),
            log_file: Some(PathBuf::from("/tmp/rb/logs/tui.log")),
        }
    }

    #[test]
    fn a_report_carries_what_a_bug_report_needs() {
        let r = format_report(
            "2026-07-27T10:00:00Z",
            "index out of bounds",
            None,
            "0: foo\n1: bar",
            "main",
            &ctx(),
        );
        assert!(r.contains("bookmole-tui 0.1.0"));
        assert!(r.contains("index out of bounds"));
        assert!(r.contains("thread:    main"));
        assert!(r.contains(std::env::consts::OS));
        assert!(r.contains("/tmp/rb/logs/tui.log"));
        assert!(r.contains("  0: foo"));
    }

    #[test]
    fn a_report_never_leaks_an_api_key() {
        let r = format_report(
            "2026-07-27T10:00:00Z",
            "failed on https://googleapis.com/books?q=x&key=SECRETVALUE123",
            None,
            "0: somewhere?key=ALSOSECRET",
            "main",
            &ctx(),
        );
        assert!(!r.contains("SECRETVALUE123"), "{r}");
        assert!(!r.contains("ALSOSECRET"), "{r}");
        assert!(r.contains("key=REDACTED"));
    }

    #[test]
    fn missing_location_is_reported_not_panicked_on() {
        let r = format_report("t", "boom", None, "", "main", &ctx());
        assert!(r.contains("location:  <unknown>"));
    }

    #[test]
    fn panic_payloads_of_both_shapes_are_recovered() {
        let literal: Box<dyn std::any::Any + Send> = Box::new("literal panic");
        assert_eq!(panic_message(&*literal), "literal panic");

        let formatted: Box<dyn std::any::Any + Send> = Box::new(String::from("formatted 42"));
        assert_eq!(panic_message(&*formatted), "formatted 42");

        let weird: Box<dyn std::any::Any + Send> = Box::new(7u32);
        assert_eq!(panic_message(&*weird), "<non-string panic payload>");
    }

    #[test]
    fn reports_append_rather_than_overwrite_and_rotate_when_large() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        append_report(dir, "first").unwrap();
        append_report(dir, "second").unwrap();
        let body = std::fs::read_to_string(dir.join("crash.log")).unwrap();
        assert!(
            body.contains("first") && body.contains("second"),
            "a second crash overwrote the first: {body}"
        );
        assert_eq!(
            body.matches("--- crash ---").count(),
            2,
            "crashes must stay separable"
        );

        std::fs::write(
            dir.join("crash.log"),
            vec![b'x'; (MAX_LOG_BYTES + 1) as usize],
        )
        .unwrap();
        append_report(dir, "after rotation").unwrap();
        assert!(dir.join("crash.log.1").exists(), "old log was not kept");
        let fresh = std::fs::read_to_string(dir.join("crash.log")).unwrap();
        assert!(fresh.contains("after rotation"));
        assert!(!fresh.contains("xxxx"), "rotation did not start a new file");
    }

    #[test]
    fn an_unwritable_log_dir_is_survivable() {
        let tmp = tempfile::tempdir().unwrap();
        let blocked = tmp.path().join("not-a-dir");
        std::fs::write(&blocked, b"x").unwrap();
        assert_eq!(append_report(&blocked, "report"), None);
    }
}
