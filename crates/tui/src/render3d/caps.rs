//! Startup probe of terminal capabilities: kitty graphics, cell pixel size, tmux passthrough.
//! A DA1 reply with no kitty reply means no graphics; replies can arrive out of order.

use std::io::{IsTerminal, Write, stdin, stdout};
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use super::kitty::ImageWire;

const DEADLINE: Duration = Duration::from_millis(200);
/// Wait this long after DA1 lands: the kitty reply can arrive after tmux's local DA1 answer.
const GRACE: Duration = Duration::from_millis(60);
const PROBE_ID: u32 = 31;
const ASSUMED_CELL_PX: (u16, u16) = (8, 16);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Passthrough {
    NotTmux,
    Already,
    EnabledByUs,
    Unavailable,
}

impl Passthrough {
    pub fn is_open(self) -> bool {
        !matches!(self, Passthrough::Unavailable)
    }
}

/// `Copy` so `ui::book::present_book` can copy it out of `App` before the `&mut app.scene` borrow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    pub kitty_graphics: bool,
    pub cell_px: (u16, u16),
    pub cell_px_measured: bool,
    pub in_tmux: bool,
    pub passthrough: Passthrough,
    pub image_wire: ImageWire,
}

impl Default for Caps {
    fn default() -> Self {
        Caps {
            kitty_graphics: false,
            cell_px: ASSUMED_CELL_PX,
            cell_px_measured: false,
            in_tmux: false,
            passthrough: Passthrough::NotTmux,
            image_wire: ImageWire::Zlib,
        }
    }
}

impl Caps {
    pub fn supports_pixels(self) -> bool {
        self.kitty_graphics && self.passthrough.is_open()
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ProbeReplies {
    pub kitty_ok: bool,
    pub cell_px: Option<(u16, u16)>,
    pub da1: bool,
}

pub fn wrap_tmux(payload: &str) -> String {
    let mut out = String::with_capacity(payload.len() + 16);
    out.push_str("\x1bPtmux;");
    for ch in payload.chars() {
        if ch == '\x1b' {
            out.push('\x1b');
        }
        out.push(ch);
    }
    out.push_str("\x1b\\");
    out
}

pub fn maybe_wrap(payload: &str, in_tmux: bool) -> String {
    if in_tmux {
        wrap_tmux(payload)
    } else {
        payload.to_string()
    }
}

pub fn parse_replies(bytes: &[u8], probe_id: u32) -> ProbeReplies {
    let mut out = ProbeReplies::default();
    let expected = format!("\x1b_Gi={probe_id};OK");
    out.kitty_ok = find(bytes, expected.as_bytes()).is_some();

    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != 0x1b {
            i += 1;
            continue;
        }
        if let Some(rest) = bytes.get(i + 1..)
            && rest.starts_with(b"[6;")
            && let Some(end) = rest.iter().position(|&b| b == b't')
            && let Ok(text) = std::str::from_utf8(&rest[3..end])
        {
            let mut parts = text.split(';');
            if let (Some(h), Some(w)) = (parts.next(), parts.next())
                && let (Ok(h), Ok(w)) = (h.trim().parse::<u16>(), w.trim().parse::<u16>())
                && let Some(px) = sane_cell_px(w, h)
            {
                out.cell_px = Some(px);
            }
        }
        if let Some(rest) = bytes.get(i + 1..)
            && rest.starts_with(b"[?")
            && rest.iter().take_while(|&&b| b != 0x1b).any(|&b| b == b'c')
        {
            out.da1 = true;
        }
        i += 1;
    }
    out
}

/// Rejects cell sizes no real font produces; a garbage report would scale every frame wrongly.
fn sane_cell_px(w: u16, h: u16) -> Option<(u16, u16)> {
    if !(4..=64).contains(&w) || !(8..=128).contains(&h) {
        return None;
    }
    let ratio = h as f32 / w as f32;
    (1.2..=3.0).contains(&ratio).then_some((w, h))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&i| &haystack[i..i + needle.len()] == needle)
}

/// Must run with raw mode on and before the event reader starts, or replies leak into it.
pub fn probe() -> Caps {
    probe_verbose().0
}

pub fn probe_verbose() -> (Caps, Vec<u8>) {
    let in_tmux = std::env::var_os("TMUX").is_some();
    let mut caps = Caps {
        in_tmux,
        image_wire: if env_suggests_libghostty() {
            ImageWire::Raw
        } else {
            ImageWire::Zlib
        },
        ..Caps::default()
    };

    if !stdin().is_terminal() || !stdout().is_terminal() {
        return (caps, Vec::new());
    }

    if in_tmux {
        caps.passthrough = enable_passthrough();
        if caps.passthrough == Passthrough::Unavailable {
            return (caps, Vec::new());
        }
    }

    let kitty_query = format!("\x1b_Gi={PROBE_ID},s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\");
    let mut out = String::new();
    out.push_str(&maybe_wrap(&kitty_query, in_tmux));
    out.push_str(&maybe_wrap("\x1b[16t", in_tmux));
    // DA1 goes unwrapped: tmux answers it itself, so it arrives even when passthrough is broken.
    out.push_str("\x1b[c");

    let mut stdout = stdout();
    if stdout.write_all(out.as_bytes()).is_err() || stdout.flush().is_err() {
        return (caps, Vec::new());
    }

    let raw = read_replies();
    let replies = parse_replies(&raw, PROBE_ID);
    if replies.kitty_ok {
        caps.kitty_graphics = true;
    } else if !replies.da1 {
        caps.kitty_graphics = env_suggests_kitty();
    }

    if let Some(px) = replies.cell_px {
        caps.cell_px = px;
        caps.cell_px_measured = true;
    } else if let Some(px) = cell_px_from_window_size() {
        caps.cell_px = px;
        caps.cell_px_measured = true;
    }

    (caps, raw)
}

#[cfg(unix)]
fn read_replies() -> Vec<u8> {
    use std::os::fd::AsRawFd;

    let fd = stdin().as_raw_fd();
    let start = Instant::now();
    let mut buf = Vec::with_capacity(256);
    let mut chunk = [0u8; 256];
    let mut sentinel_at: Option<Instant> = None;

    loop {
        let elapsed = start.elapsed();
        if elapsed >= DEADLINE {
            break;
        }
        let mut budget = DEADLINE - elapsed;
        if let Some(seen) = sentinel_at {
            let grace_left = GRACE.saturating_sub(seen.elapsed());
            if grace_left.is_zero() {
                break;
            }
            budget = budget.min(grace_left);
        }

        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: a single valid pollfd for a fd we own, count matches.
        let ready = unsafe { libc::poll(&mut pfd, 1, budget.as_millis() as libc::c_int) };
        if ready <= 0 {
            break;
        }
        // SAFETY: reading into a stack buffer we own, length matches.
        let n = unsafe { libc::read(fd, chunk.as_mut_ptr() as *mut libc::c_void, chunk.len()) };
        if n <= 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n as usize]);

        let replies = parse_replies(&buf, PROBE_ID);
        if replies.kitty_ok && replies.cell_px.is_some() && replies.da1 {
            break;
        }
        if replies.da1 && sentinel_at.is_none() {
            sentinel_at = Some(Instant::now());
        }
    }

    buf
}

#[cfg(not(unix))]
fn read_replies() -> Vec<u8> {
    Vec::new()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rtt {
    pub kitty: Option<Duration>,
    pub tmux: Option<Duration>,
}

const RTT_ID: u32 = 37;

/// Bench-only: reads stdin, which the running app's `EventStream` owns, and needs raw mode on.
pub fn rtt_probe(in_tmux: bool, deadline: Duration) -> Rtt {
    if !stdin().is_terminal() || !stdout().is_terminal() {
        return Rtt::default();
    }
    let query = format!("\x1b_Gi={RTT_ID},s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\");
    let mut out = String::new();
    out.push_str(&maybe_wrap(&query, in_tmux));
    out.push_str("\x1b[c");

    let mut stdout = stdout();
    let start = Instant::now();
    if stdout.write_all(out.as_bytes()).is_err() || stdout.flush().is_err() {
        return Rtt::default();
    }
    read_rtt(start, deadline)
}

#[cfg(unix)]
fn read_rtt(start: Instant, deadline: Duration) -> Rtt {
    use std::os::fd::AsRawFd;

    let fd = stdin().as_raw_fd();
    let mut buf = Vec::with_capacity(128);
    let mut chunk = [0u8; 256];
    let mut rtt = Rtt::default();

    loop {
        let elapsed = start.elapsed();
        if elapsed >= deadline || (rtt.kitty.is_some() && rtt.tmux.is_some()) {
            break;
        }
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: a single valid pollfd for a fd we own, count matches.
        let ready = unsafe { libc::poll(&mut pfd, 1, (deadline - elapsed).as_millis() as i32) };
        if ready <= 0 {
            break;
        }
        // SAFETY: reading into a stack buffer we own, length matches.
        let n = unsafe { libc::read(fd, chunk.as_mut_ptr() as *mut libc::c_void, chunk.len()) };
        if n <= 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n as usize]);

        // Timestamp on first sight only; a later chunk must not overwrite the arrival time.
        let replies = parse_replies(&buf, RTT_ID);
        let now = start.elapsed();
        if replies.kitty_ok && rtt.kitty.is_none() {
            rtt.kitty = Some(now);
        }
        if replies.da1 && rtt.tmux.is_none() {
            rtt.tmux = Some(now);
        }
    }
    rtt
}

#[cfg(not(unix))]
fn read_rtt(_start: Instant, _deadline: Duration) -> Rtt {
    Rtt::default()
}

fn cell_px_from_window_size() -> Option<(u16, u16)> {
    let ws = crossterm::terminal::window_size().ok()?;
    if ws.width == 0 || ws.height == 0 || ws.columns == 0 || ws.rows == 0 {
        return None;
    }
    Some((ws.width / ws.columns, ws.height / ws.rows))
}

/// Used only when the terminal answered nothing; `TERM_PROGRAM` reads `tmux` inside tmux.
fn env_suggests_kitty() -> bool {
    let has = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
    if has("KITTY_WINDOW_ID") || has("GHOSTTY_RESOURCES_DIR") || has("WEZTERM_EXECUTABLE") {
        return true;
    }
    let term = std::env::var("TERM").unwrap_or_default();
    if term.contains("kitty") || term.contains("ghostty") {
        return true;
    }
    matches!(
        std::env::var("TERM_PROGRAM").unwrap_or_default().as_str(),
        "ghostty" | "WezTerm"
    )
}

/// Identification, not capability: a libghostty host (including herdr) crashes on compressed
/// payloads while answering OK to the graphics query, so the probe cannot detect it.
fn env_suggests_libghostty() -> bool {
    let has = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
    host_is_libghostty(
        &has,
        &std::env::var("TERM_PROGRAM").unwrap_or_default(),
        &std::env::var("TERM").unwrap_or_default(),
    )
}

/// Pure so it can be tested; `set_var` is unsafe in edition 2024 and races other tests.
fn host_is_libghostty(has: &dyn Fn(&str) -> bool, term_program: &str, term: &str) -> bool {
    if has("HERDR_ENV") || has("HERDR_PANE_ID") || has("HERDR_SOCKET_PATH") {
        return true;
    }
    if has("GHOSTTY_RESOURCES_DIR") || has("GHOSTTY_BIN_DIR") {
        return true;
    }
    term_program == "ghostty" || term.contains("ghostty")
}

/// An atomic, not a `Mutex`: the panic hook runs this and a held lock would deadlock it.
/// The displaced pane value can only have been unset or off, since we set it only when not on.
const OWE_NOTHING: u8 = 0;
const OWE_UNSET: u8 = 1;
const OWE_OFF: u8 = 2;
static PRIOR_PASSTHROUGH: AtomicU8 = AtomicU8::new(OWE_NOTHING);

fn tmux(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("tmux")
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn enable_passthrough() -> Passthrough {
    let pane = std::env::var("TMUX_PANE").unwrap_or_default();
    if pane.is_empty() {
        return Passthrough::Unavailable;
    }

    match tmux(&["show", "-pAv", "-t", &pane, "allow-passthrough"]).as_deref() {
        Some("on") | Some("all") => return Passthrough::Already,
        None => return Passthrough::Unavailable,
        _ => {}
    }

    // An empty answer means the pane inherited, so restore by unsetting, not by writing "off".
    let owed = match tmux(&["show", "-pv", "-t", &pane, "allow-passthrough"]).as_deref() {
        Some("") | None => OWE_UNSET,
        Some(_) => OWE_OFF,
    };
    if tmux(&["set", "-p", "-t", &pane, "allow-passthrough", "on"]).is_none() {
        return Passthrough::Unavailable;
    }
    PRIOR_PASSTHROUGH.store(owed, Ordering::SeqCst);
    Passthrough::EnabledByUs
}

/// Idempotent, never panics, takes no locks: it is called from the panic hook.
pub fn restore_passthrough() {
    let owed = PRIOR_PASSTHROUGH.swap(OWE_NOTHING, Ordering::SeqCst);
    if owed == OWE_NOTHING {
        return;
    }
    let pane = std::env::var("TMUX_PANE").unwrap_or_default();
    if pane.is_empty() {
        return;
    }
    if owed == OWE_UNSET {
        tmux(&["set", "-p", "-t", &pane, "-u", "allow-passthrough"]);
    } else {
        tmux(&["set", "-p", "-t", &pane, "allow-passthrough", "off"]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_libghostty_host_is_identified_by_any_of_its_markers() {
        let none = |_: &str| false;
        let only = |k: &'static str| move |q: &str| q == k;

        for marker in ["HERDR_ENV", "HERDR_PANE_ID", "HERDR_SOCKET_PATH"] {
            assert!(
                host_is_libghostty(&only(marker), "", ""),
                "{marker} did not identify herdr"
            );
        }
        assert!(host_is_libghostty(&only("GHOSTTY_RESOURCES_DIR"), "", ""));
        assert!(host_is_libghostty(&none, "ghostty", ""));
        assert!(host_is_libghostty(&none, "tmux", "xterm-ghostty"));

        assert!(!host_is_libghostty(
            &only("KITTY_WINDOW_ID"),
            "",
            "xterm-kitty"
        ));
        assert!(!host_is_libghostty(&none, "WezTerm", "wezterm"));
        assert!(!host_is_libghostty(&none, "", ""));
    }

    #[test]
    fn the_safe_floor_still_compresses() {
        assert_eq!(Caps::default().image_wire, ImageWire::Zlib);
    }

    #[test]
    fn wraps_for_tmux_and_doubles_escapes() {
        assert_eq!(wrap_tmux("\x1b[16t"), "\x1bPtmux;\x1b\x1b[16t\x1b\\");
        assert_eq!(wrap_tmux("plain"), "\x1bPtmux;plain\x1b\\");
        assert_eq!(
            wrap_tmux("\x1ba\x1bb"),
            "\x1bPtmux;\x1b\x1ba\x1b\x1bb\x1b\\"
        );
    }

    #[test]
    fn maybe_wrap_is_identity_outside_tmux() {
        assert_eq!(maybe_wrap("\x1b[c", false), "\x1b[c");
        assert_eq!(maybe_wrap("\x1b[c", true), wrap_tmux("\x1b[c"));
    }

    #[test]
    fn parses_a_full_kitty_reply() {
        let raw = b"\x1b_Gi=31;OK\x1b\\\x1b[6;39;19t\x1b[?62;c";
        let r = parse_replies(raw, 31);
        assert!(r.kitty_ok);
        assert_eq!(r.cell_px, Some((19, 39)));
        assert!(r.da1);
    }

    #[test]
    fn da1_without_graphics_is_a_definitive_no() {
        let r = parse_replies(b"\x1b[?62;22c", 31);
        assert!(!r.kitty_ok);
        assert!(r.da1);
        assert_eq!(r.cell_px, None);
    }

    #[test]
    fn silence_reports_nothing() {
        assert_eq!(parse_replies(b"", 31), ProbeReplies::default());
    }

    #[test]
    fn ignores_a_reply_for_a_different_image_id() {
        let r = parse_replies(b"\x1b_Gi=7;OK\x1b\\\x1b[?62;c", 31);
        assert!(!r.kitty_ok, "another image's OK must not count as ours");
        assert!(r.da1);
    }

    #[test]
    fn tolerates_interleaved_and_out_of_order_replies() {
        let raw = b"\x1b[?62;c\x1b[6;32;16t\x1b_Gi=31;OK\x1b\\";
        let r = parse_replies(raw, 31);
        assert!(r.kitty_ok && r.da1);
        assert_eq!(r.cell_px, Some((16, 32)));
    }

    #[test]
    fn tolerates_unrelated_input_around_the_replies() {
        let raw = b"qqq\x1b[?62;c\x1bOA\x1b_Gi=31;OK\x1b\\zz";
        let r = parse_replies(raw, 31);
        assert!(r.kitty_ok && r.da1);
    }

    #[test]
    fn a_truncated_cell_report_is_no_report() {
        assert_eq!(parse_replies(b"\x1b[6;39;19", 31).cell_px, None);
        assert_eq!(parse_replies(b"\x1b[6;0;0t", 31).cell_px, None);
    }

    #[test]
    fn rejects_cell_sizes_no_font_produces() {
        assert_eq!(
            parse_replies(b"\x1b[6;3;40t", 31).cell_px,
            None,
            "wider than tall"
        );
        assert_eq!(parse_replies(b"\x1b[6;400;2t", 31).cell_px, None, "absurd");
        assert_eq!(
            parse_replies(b"\x1b[6;16;16t", 31).cell_px,
            None,
            "square cell"
        );
        assert_eq!(parse_replies(b"\x1b[6;39;19t", 31).cell_px, Some((19, 39)));
        assert_eq!(parse_replies(b"\x1b[6;16;8t", 31).cell_px, Some((8, 16)));
    }

    #[test]
    fn default_caps_are_the_safe_floor() {
        let caps = Caps::default();
        assert!(!caps.supports_pixels());
        assert!(!caps.cell_px_measured);
        assert_eq!(caps.passthrough, Passthrough::NotTmux);
    }

    #[test]
    fn pixels_need_both_protocol_and_an_open_path() {
        let base = Caps {
            kitty_graphics: true,
            in_tmux: true,
            passthrough: Passthrough::EnabledByUs,
            ..Caps::default()
        };
        assert!(base.supports_pixels());
        assert!(
            !Caps {
                passthrough: Passthrough::Unavailable,
                ..base
            }
            .supports_pixels(),
            "a blocked tmux must veto pixels even on a capable terminal"
        );
        assert!(
            !Caps {
                kitty_graphics: false,
                ..base
            }
            .supports_pixels()
        );
    }
}
