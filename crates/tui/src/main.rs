//! bookmole TUI, the ratatui frontend. All engine work goes through `bookmole::Engine`;
//! this crate owns every byte that reaches the terminal.

mod ambient;
mod app;
mod clipboard;
mod config;
mod event;
mod location;
mod logging;
mod perf;
mod render3d;
mod theme;
mod ui;

use std::io::{Stdout, Write, stdout};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use bookmole::{Book, BookQuery, BookSort, Engine, EngineConfig};
use clap::Parser;
use crossterm::event::{
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use render3d::{RenderParams, Scene};

#[derive(Parser)]
#[command(
    name = "bookmole-tui",
    version,
    about = "Reading companion, on screen: browse the library and view a book in 3D"
)]
struct Cli {
    /// Data directory root (default: BOOKMOLE_DATA_DIR, else the platform data dir)
    #[arg(long)]
    data_dir: Option<PathBuf>,

    /// Open straight into a book (id, ISBN, or title fragment)
    #[arg(long)]
    book: Option<String>,

    /// Render one frame of the object to stdout as WxH terminal cells and exit
    #[arg(long, value_name = "WxH")]
    dump_frame: Option<String>,

    /// Yaw,pitch in radians for --dump-frame (default: the standard pose)
    #[arg(long, value_name = "YAW,PITCH", requires = "dump_frame")]
    pose: Option<String>,

    /// Also write a PNG of the quantized cells (what the terminal would show)
    #[arg(long, value_name = "PATH", requires = "dump_frame")]
    dump_png: Option<PathBuf>,

    /// Block-glyph family: octant (default, 2x4, needs a Unicode-16 font) or
    /// quadrant (2x2, works on any truecolor terminal)
    #[arg(long, value_enum, default_value_t = GlyphArg::Octant)]
    glyphs: GlyphArg,

    /// Book-view renderer. `rich` is glyphs while the book turns, pixels when it parks; `auto` picks it
    /// where pixels work.
    #[arg(long, value_enum, default_value_t = RenderArg::Auto)]
    render: RenderArg,

    /// Compress the pixel payload. `auto` sends it raw on libghostty hosts (they crash on compressed
    /// images).
    #[arg(long, value_enum, default_value_t = CompressArg::Auto, value_name = "WHEN")]
    kitty_compress: CompressArg,

    /// Probe the terminal, print its capabilities and the renderer picked, then exit (needs a real tty)
    #[arg(long)]
    probe: bool,

    /// Panic on purpose, to check the crash hook restores the terminal (hidden)
    #[arg(long, hide = true)]
    panic_now: bool,

    /// Push N rich frames at the terminal as fast as it will take them and report the rate (real pane)
    #[arg(long, value_name = "N")]
    bench_rich: Option<u32>,

    /// Run a scripted spin/park/resize workload through the real presenter and report its cost (real,
    /// active pane)
    #[arg(long, value_enum, value_name = "MODE")]
    bench_render: Option<BenchArg>,

    /// Repeat the --bench-render script N times (default 1)
    #[arg(long, value_name = "N", requires = "bench_render")]
    bench_reps: Option<u32>,

    /// Draw as fast as possible instead of pacing to 20fps; inflates send rate beyond a real session
    #[arg(long, requires = "bench_render")]
    bench_free_run: bool,

    /// Environment name recorded in the history TSV (default `live`); rows from different environments
    /// are not comparable.
    #[arg(long, value_name = "NAME", requires = "bench_render")]
    bench_env: Option<String>,

    /// Append a JSON record per frame to this file (off by default)
    #[arg(long, value_name = "PATH", env = "BOOKMOLE_PERF_LOG")]
    perf_log: Option<PathBuf>,

    /// Append one summary row per benched mode to this TSV, to compare runs over time
    #[arg(long, value_name = "PATH", env = "BOOKMOLE_PERF_HISTORY")]
    perf_history: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum BenchArg {
    Glyph,
    Rich,
    /// Pixels even while moving; a known-bad diagnostic configuration
    RichAlways,
    /// All three, one after another, on identical work.
    All,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum RenderArg {
    /// Pixels wherever the terminal can take them, block glyphs otherwise.
    Auto,
    /// Block glyphs always. Works anywhere truecolor does.
    Glyph,
    /// Glyphs while the book turns, pixels when it parks; glyphs only without a pixel protocol.
    Rich,
    /// Pixels even while the book moves. Known bad; a diagnostic, not a usable setting.
    RichAlways,
}

impl RenderArg {
    /// A mode forced on a terminal that cannot take pixels still degrades to glyphs.
    fn resolve(self, caps: render3d::Caps) -> render3d::RenderMode {
        use render3d::RenderMode;
        match self {
            RenderArg::Glyph => RenderMode::Glyph,
            RenderArg::RichAlways if caps.supports_pixels() => RenderMode::RichAlways,
            _ if caps.supports_pixels() => RenderMode::Rich,
            _ => RenderMode::Glyph,
        }
    }
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum CompressArg {
    /// Raw on a libghostty host (Ghostty, herdr), zlib everywhere else.
    Auto,
    /// zlib always; what every terminal but a libghostty one wants.
    On,
    /// Raw always. Six times the bytes, so the pixel budget shrinks.
    Off,
}

impl CompressArg {
    fn apply(self, caps: render3d::Caps) -> render3d::Caps {
        use render3d::ImageWire;
        let image_wire = match self {
            CompressArg::Auto => return caps,
            CompressArg::On => ImageWire::Zlib,
            CompressArg::Off => ImageWire::Raw,
        };
        render3d::Caps { image_wire, ..caps }
    }
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum GlyphArg {
    Octant,
    Quadrant,
}

impl From<GlyphArg> for render3d::GlyphSet {
    fn from(g: GlyphArg) -> Self {
        match g {
            GlyphArg::Octant => render3d::GlyphSet::Octant,
            GlyphArg::Quadrant => render3d::GlyphSet::Quadrant,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Before anything opens the engine: the probe must not create a `database/` directory.
    if cli.probe {
        return report_caps(&cli);
    }

    let data_root = bookmole::data_dir::resolve(cli.data_dir.as_deref())?;
    let mut engine_config = EngineConfig::rooted_at(data_root);
    let config_dir = config::default_dir();
    if engine_config.google_api_key.is_none() {
        engine_config.google_api_key = config_dir.as_deref().and_then(config::load_google_key);
    }
    // Held for the whole run (dropping early truncates the log). Installed before `setup_terminal`:
    // the last hook installed runs first, so the terminal is restored before anything prints.
    bookmole::crash::install_hook(bookmole::CrashContext {
        app: "bookmole-tui",
        version: env!("CARGO_PKG_VERSION"),
        log_dir: engine_config.log_dir.clone(),
        log_file: Some(engine_config.log_dir.join("tui.log")),
    });

    let log = logging::init(&engine_config.log_dir, None);
    if let Some(h) = &log {
        tracing::info!(version = env!("CARGO_PKG_VERSION"), log = %h.path.display(), "bookmole-tui starting");
    }
    let engine = Engine::open(engine_config).await?;
    // Headless paths have no status line, so they put the cloud-folder notice on stderr.
    let cloud_line = engine
        .library_location()
        .cloud_folder
        .map(|f| location::cloud_folder_line(&f));

    if cli.panic_now {
        panic!("--panic-now: deliberate crash to exercise the crash hook");
    }

    let no_status_line =
        cli.dump_frame.is_some() || cli.bench_rich.is_some() || cli.bench_render.is_some();
    if no_status_line && let Some(line) = &cloud_line {
        eprintln!("{line}");
    }

    if let Some(spec) = &cli.dump_frame {
        return dump_frame(&engine, spec, &cli).await;
    }

    if let Some(frames) = cli.bench_rich {
        return bench_rich(&engine, frames, &cli).await;
    }

    if let Some(which) = cli.bench_render {
        return bench_render(engine, which, &cli).await;
    }

    let mut motif = ambient::Motif::default();
    let mut library_sort = ui::library::Sort::default();
    let loaded = config_dir
        .as_deref()
        .context("cannot locate home directory")
        .and_then(config::load);
    match loaded {
        Ok(cfg) => {
            if let Some(rgb) = cfg.accent.as_deref().and_then(theme::parse_hex) {
                theme::set_accent(rgb);
            }
            if let Some(s) = cfg
                .library_sort
                .as_deref()
                .and_then(ui::library::Sort::from_label)
            {
                library_sort = s;
            }
            if let Some(m) = cfg.ambient.as_deref().and_then(ambient::Motif::from_label) {
                motif = m;
            }
        }
        Err(e) => eprintln!("warning: {e:#}"),
    }

    let mut app = app::App::new(engine, config_dir).await?;
    app.ambient = ambient::Ambient::new(motif);
    app.set_library_sort(library_sort);
    app.params.glyphs = cli.glyphs.into();
    if let Some(selector) = &cli.book {
        let book = resolve_book(&app.engine, selector).await?;
        app.open_book(book).await?;
    }
    // After `--book`, whose `open_book` clears the status line.
    if let Some(line) = cloud_line {
        app.status = Some(line);
    }

    // One meter shared by the ratatui backend (text) and the graphics writer (image), counted apart.
    let meter = perf::Meter::new();
    if let Some(path) = &cli.perf_log {
        match perf::Recorder::open(path, meter.clone()) {
            Ok(rec) => app.perf = rec,
            Err(e) => eprintln!("warning: perf log {}: {e}", path.display()),
        }
    }

    // The probe needs raw mode and must run before ratatui reads input, so it follows terminal setup.
    let (mut terminal, caps) = setup_terminal(meter.clone())?;
    let caps = cli.kitty_compress.apply(caps);
    app.set_caps(caps);
    app.set_render_mode(cli.render.resolve(caps));
    app.rich.count_bytes(meter);
    // Warn to the log, not the screen: the alternate screen is already up and the app runs without a
    // watcher.
    let mounts = match bookmole::watch_mounts() {
        Ok(w) => Some(w),
        Err(e) => {
            tracing::warn!(error = %e, "not watching for devices");
            None
        }
    };
    // Without a watcher the vault is still swept once before the first frame; only live edits wait.
    let vault = match app.engine.watch_vault() {
        Ok(w) => Some(w),
        Err(e) => {
            tracing::warn!(error = %e, "not watching the vault");
            None
        }
    };
    let result = app::run(&mut terminal, &mut app, mounts, vault).await;
    restore_terminal();
    result
}

/// Raw mode is held only for the probe so replies arrive unechoed; no alternate screen.
fn report_caps(cli: &Cli) -> Result<()> {
    let raw_mode = enable_raw_mode().is_ok();
    let (caps, raw) = render3d::caps::probe_verbose();
    let caps = cli.kitty_compress.apply(caps);
    if raw_mode {
        let _ = disable_raw_mode();
    }
    render3d::caps::restore_passthrough();

    let mode = cli.render.resolve(caps);
    let (w, h) = caps.cell_px;
    println!("kitty graphics : {}", caps.kitty_graphics);
    println!(
        "cell size      : {w}x{h} px ({})",
        if caps.cell_px_measured {
            "reported"
        } else {
            "assumed"
        }
    );
    println!("tmux           : {}", caps.in_tmux);
    println!("passthrough    : {:?}", caps.passthrough);
    println!("pixels usable  : {}", caps.supports_pixels());
    // Not reported by the terminal (it is a guess), so print it beside what was reported.
    println!(
        "image payload  : {:?} (--kitty-compress {:?})",
        caps.image_wire, cli.kitty_compress
    );
    println!("renderer       : {mode:?} (--render {:?})", cli.render);
    println!(
        "raw reply      : {}",
        String::from_utf8_lossy(&raw).escape_debug()
    );
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum Phase {
    Spin(u32),
    Park(u32),
    Slide(f32),
}

/// Fixed, so runs and modes are comparable.
const SCRIPT: &[Phase] = &[
    Phase::Spin(100),
    Phase::Park(20),
    Phase::Spin(60),
    Phase::Slide(0.3),
    Phase::Spin(40),
    Phase::Park(20),
];

/// 8 gives ~30 samples per script, the floor for a meaningful p90.
const RTT_EVERY: u32 = 8;

const WARMUP: u32 = 25;

/// Shorter than a tick: a probe that outlives the frame budget changes the workload it measures.
const RTT_DEADLINE: std::time::Duration = std::time::Duration::from_millis(40);

const RTT_GIVE_UP: u32 = 2;

struct BenchSummary {
    mode: &'static str,
    frames: u32,
    wall: std::time::Duration,
    bytes_text: u64,
    bytes_image: u64,
    transmits: u32,
    rect: (u16, u16),
    trace_us: u64,
    draw_us: u64,
    /// Image bytes on frames where the book was moving; must be zero for the hybrid.
    moving_image_bytes: u64,
    rtt_kitty: Vec<u64>,
    rtt_tmux: Vec<u64>,
}

async fn bench_render(engine: Engine, which: BenchArg, cli: &Cli) -> Result<()> {
    if !std::io::IsTerminal::is_terminal(&stdout()) {
        bail!("--bench-render needs a real terminal — it measures the terminal, not us");
    }
    let reps = cli.bench_reps.unwrap_or(1).max(1);
    let modes: &[render3d::RenderMode] = match which {
        BenchArg::Glyph => &[render3d::RenderMode::Glyph],
        BenchArg::Rich => &[render3d::RenderMode::Rich],
        BenchArg::RichAlways => &[render3d::RenderMode::RichAlways],
        BenchArg::All => &[
            render3d::RenderMode::Glyph,
            render3d::RenderMode::Rich,
            render3d::RenderMode::RichAlways,
        ],
    };

    // Pick a book with a real cover, ranked by decoded width: procedural plates and upscaled thumbnails
    // compress far better than photographs and flatter the image numbers. An explicit `--book` wins.
    let cover_width = |b: &Book| -> u32 {
        b.cover_path
            .as_deref()
            .and_then(|p| render3d::resolve_cover(engine.images_dir(), p))
            .and_then(|p| image::image_dimensions(p).ok())
            .map(|(w, _)| w)
            .unwrap_or(0)
    };
    let book = match &cli.book {
        Some(sel) => resolve_book(&engine, sel).await?,
        None => {
            let library = engine
                .list_books(&BookQuery::new(200, BookSort::LastModified))
                .await?;
            library
                .iter()
                .max_by_key(|b| cover_width(b))
                .cloned()
                .context("no books in the library to bench with")?
        }
    };
    let cover_px = cover_width(&book);

    let meter = perf::Meter::new();
    // A bench changes no setting, so it has nowhere to save one.
    let mut app = app::App::new(engine, None).await?;
    app.params.glyphs = cli.glyphs.into();
    app.open_book(book.clone()).await?;
    if let Some(path) = &cli.perf_log {
        match perf::Recorder::open(path, meter.clone()) {
            Ok(rec) => app.perf = rec,
            Err(e) => eprintln!("warning: perf log {}: {e}", path.display()),
        }
    }
    app.perf.enable_in_memory();

    let (mut terminal, caps) = setup_terminal(meter.clone())?;
    let caps = cli.kitty_compress.apply(caps);
    app.set_caps(caps);
    app.rich.count_bytes(meter);

    // Reps outer, modes inner: interleaving keeps a transient from reading as one mode being slow.
    let mut summaries = Vec::new();
    let mut rtt_silent = 0u32;
    for _ in 0..reps {
        for &mode in modes {
            app.set_render_mode(mode);
            app.rich.drop_image();
            app.layout.divider_bias = 0.0;
            app.params.pose = render3d::Pose::default();
            let _ = app.perf.drain_frames();

            // Paced like `app::run`: free-running inflates the retransmit send rate far beyond a real session.
            let mut ticker = tokio::time::interval(app::TICK);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

            app.spinning = true;
            for _ in 0..WARMUP {
                if !cli.bench_free_run {
                    ticker.tick().await;
                }
                app.tick();
                app::redraw(&mut terminal, &mut app)?;
            }
            let _ = app.perf.drain_frames();

            let wall = std::time::Instant::now();
            let mut frame = 0u32;
            for phase in SCRIPT {
                match *phase {
                    Phase::Slide(delta) => {
                        app.layout.divider_bias =
                            (app.layout.divider_bias + delta).clamp(-1.0, 1.0);
                    }
                    Phase::Spin(n) | Phase::Park(n) => {
                        app.spinning = matches!(phase, Phase::Spin(_));
                        for _ in 0..n {
                            if !cli.bench_free_run {
                                ticker.tick().await;
                            }
                            app.tick();
                            app::redraw(&mut terminal, &mut app)?;
                            // Sample right after handing the terminal a frame: in the idle gap it has caught up and shows
                            // nothing.
                            if frame.is_multiple_of(RTT_EVERY) && rtt_silent < RTT_GIVE_UP {
                                let rtt = render3d::caps::rtt_probe(caps.in_tmux, RTT_DEADLINE);
                                // A silent terminal is not waited on, or its stalls become the workload.
                                rtt_silent = if rtt.kitty.is_none() {
                                    rtt_silent + 1
                                } else {
                                    0
                                };
                                app.rtt_sample = Some(rtt);
                            }
                            frame += 1;
                        }
                    }
                }
            }
            summaries.push(summarize(
                mode.label(),
                frame,
                wall.elapsed(),
                &mut app.perf,
            ));
        }
    }

    app.rich.drop_image();
    app.perf.flush();
    restore_terminal();
    report_bench(&summaries, cover_px, caps, cli.bench_free_run);
    if let Some(path) = &cli.perf_history {
        let env = cli.bench_env.as_deref().unwrap_or("live");
        match append_history(path, &summaries, cover_px, caps, env) {
            Ok(()) => eprintln!("history     : {}", path.display()),
            Err(e) => eprintln!("warning: perf history {}: {e}", path.display()),
        }
    }
    Ok(())
}

/// A file whose header does not match the current columns is rotated aside, never appended to.
const HISTORY_HEADER: &str = "epoch_s\tmode\tframes\tfps\ttext_MBps\timage_MBps\tmoving_KB\tsends\t\
     trace_us\tdraw_us\trtt_kitty_p50_us\trtt_kitty_p90_us\trtt_tmux_p50_us\t\
     rtt_tmux_p90_us\tcols\trows\tin_tmux\tcover_px\tenv\tload1";

/// The 1-minute load average, or 0 where unavailable; recorded to tell contention from regression.
fn load1() -> f64 {
    #[cfg(unix)]
    {
        let mut avg = [0.0f64; 3];
        // SAFETY: getloadavg writes at most `nelem` doubles into the buffer.
        let n = unsafe { libc::getloadavg(avg.as_mut_ptr(), 1) };
        if n >= 1 { avg[0] } else { 0.0 }
    }
    #[cfg(not(unix))]
    {
        0.0
    }
}

fn append_history(
    path: &std::path::Path,
    summaries: &[BenchSummary],
    cover_px: u32,
    caps: render3d::Caps,
    env: &str,
) -> std::io::Result<()> {
    use std::io::Write as _;

    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    if path.exists() {
        let stale = std::fs::read_to_string(path)
            .map(|t| t.lines().next().map(str::to_owned))
            .ok()
            .flatten()
            .is_none_or(|h| h != HISTORY_HEADER);
        if stale {
            let aside = path.with_extension("tsv.old");
            std::fs::rename(path, &aside)?;
            eprintln!(
                "note: history columns changed; previous file kept at {}",
                aside.display()
            );
        }
    }
    let fresh = !path.exists();
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    if fresh {
        writeln!(f, "{HISTORY_HEADER}")?;
    }
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let load = load1();
    for s in summaries {
        let secs = s.wall.as_secs_f64().max(1e-9);
        writeln!(
            f,
            "{epoch}\t{}\t{}\t{:.1}\t{:.3}\t{:.3}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:.2}",
            s.mode,
            s.frames,
            s.frames as f64 / secs,
            s.bytes_text as f64 / secs / 1.0e6,
            s.bytes_image as f64 / secs / 1.0e6,
            s.moving_image_bytes / 1024,
            s.transmits,
            s.trace_us,
            s.draw_us,
            perf::percentile(&s.rtt_kitty, 50.0),
            perf::percentile(&s.rtt_kitty, 90.0),
            perf::percentile(&s.rtt_tmux, 50.0),
            perf::percentile(&s.rtt_tmux, 90.0),
            s.rect.0,
            s.rect.1,
            caps.in_tmux,
            cover_px,
            env,
            load,
        )?;
    }
    Ok(())
}

fn summarize(
    mode: &'static str,
    frames: u32,
    wall: std::time::Duration,
    rec: &mut perf::Recorder,
) -> BenchSummary {
    let recs = rec.drain_frames();
    let n = recs.len().max(1) as u64;
    BenchSummary {
        mode,
        frames,
        wall,
        bytes_text: recs.iter().map(|r| r.bytes_text).sum(),
        bytes_image: recs.iter().map(|r| r.bytes_image).sum(),
        transmits: recs.iter().filter(|r| r.transmitted).count() as u32,
        rect: recs
            .iter()
            .map(|r| (r.cols, r.rows))
            .max()
            .unwrap_or((0, 0)),
        trace_us: recs.iter().map(|r| r.trace_us).sum::<u64>() / n,
        draw_us: recs.iter().map(|r| r.draw_us).sum::<u64>() / n,
        moving_image_bytes: recs
            .iter()
            .filter(|r| r.moving)
            .map(|r| r.bytes_image)
            .sum(),
        rtt_kitty: sorted(recs.iter().filter_map(|r| r.rtt_kitty_us)),
        rtt_tmux: sorted(recs.iter().filter_map(|r| r.rtt_tmux_us)),
    }
}

fn sorted(it: impl Iterator<Item = u64>) -> Vec<u64> {
    let mut v: Vec<u64> = it.collect();
    v.sort_unstable();
    v
}

fn report_bench(summaries: &[BenchSummary], cover_px: u32, caps: render3d::Caps, free_run: bool) {
    // No cover and an upscaled thumbnail both compress well and understate the image bytes.
    if cover_px == 0 {
        eprintln!(
            "WARNING: this book has no cover image on disk, so the render used the\n\
             procedural plate. It compresses several times better than a photograph —\n\
             the image byte rates below are optimistic. Bench a book with a real cover."
        );
    } else if cover_px < 400 {
        eprintln!(
            "WARNING: the best cover in the library is only {cover_px}px wide (an\n\
             OpenLibrary '-M' thumbnail is 180px). Upscaled into the render it is\n\
             smooth, so it compresses far better than a real cover and the image\n\
             byte rates below are optimistic."
        );
    }
    if !caps.supports_pixels() {
        eprintln!(
            "WARNING: this terminal reports no usable pixel protocol, so every mode\n\
             fell back to glyphs and the comparison is meaningless."
        );
    }
    if free_run {
        eprintln!(
            "WARNING: --bench-free-run. These rates are throughput, NOT what the app\n\
             costs: the retransmit throttle keys off pose, so drawing faster than the\n\
             20fps tick inflates the send rate well above any real session."
        );
    }
    eprintln!(
        "in tmux     : {}   cell px: {:?}   paced: {}",
        caps.in_tmux,
        caps.cell_px,
        if free_run { "no (free-run)" } else { "20fps" }
    );
    eprintln!();
    eprintln!(
        "{:<12} {:>6} {:>6} {:>9} {:>10} {:>9} {:>7} {:>8} {:>8}",
        "mode", "frames", "fps", "text MB/s", "image MB/s", "moving-KB", "sends", "trace", "draw"
    );
    for s in summaries {
        let secs = s.wall.as_secs_f64().max(1e-9);
        eprintln!(
            "{:<12} {:>6} {:>6.1} {:>9.2} {:>10.2} {:>9} {:>7} {:>7}µs {:>7}µs",
            s.mode,
            s.frames,
            s.frames as f64 / secs,
            s.bytes_text as f64 / secs / 1.0e6,
            s.bytes_image as f64 / secs / 1.0e6,
            s.moving_image_bytes / 1024,
            s.transmits,
            s.trace_us,
            s.draw_us,
        );
    }
    eprintln!();
    eprintln!("object rect : {:?}", summaries.first().map(|s| s.rect));
    eprintln!(
        "'moving-KB' is image bytes sent while the book was animating. For the\n\
         hybrid this must be 0 — that is the invariant, not a budget."
    );
    eprintln!();
    eprintln!(
        "{:<12} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10}",
        "mode", "kitty p50", "kitty p90", "kitty max", "tmux p50", "tmux p90", "tmux max"
    );
    for s in summaries {
        let ms = |v: &[u64], p: f64| perf::percentile(v, p) as f64 / 1000.0;
        eprintln!(
            "{:<12} {:>9.1}m {:>9.1}m {:>9.1}m {:>9.1}m {:>9.1}m {:>9.1}m",
            s.mode,
            ms(&s.rtt_kitty, 50.0),
            ms(&s.rtt_kitty, 90.0),
            ms(&s.rtt_kitty, 100.0),
            ms(&s.rtt_tmux, 50.0),
            ms(&s.rtt_tmux, 90.0),
            ms(&s.rtt_tmux, 100.0),
        );
    }
    if summaries.iter().all(|s| s.rtt_kitty.is_empty()) {
        eprintln!(
            "\nNo graphics round-trips came back, so the latency columns are empty and\n\
             every mode almost certainly fell back to glyphs. Inside tmux this usually\n\
             means the pane was not the active one — tmux routes input to the focused\n\
             pane only. Check with `--probe` from the same pane."
        );
    }
    if !free_run {
        let target = 1.0 / app::TICK.as_secs_f64();
        if let Some(worst) = summaries
            .iter()
            .map(|s| s.frames as f64 / s.wall.as_secs_f64().max(1e-9))
            .min_by(|a, b| a.partial_cmp(b).expect("finite"))
            && worst < target * 0.9
        {
            eprintln!(
                "\nWARNING: a run managed only {worst:.1} fps against the {target:.0} fps tick.\n\
                 The frame budget was missed, so these columns describe a slower\n\
                 workload than the app's. Re-run on an idle machine."
            );
        }
    }
}

async fn bench_rich(engine: &Engine, frames: u32, cli: &Cli) -> Result<()> {
    use std::time::Instant;

    let book = match &cli.book {
        Some(sel) => resolve_book(engine, sel).await?,
        None => engine
            .list_books(&BookQuery::new(1, BookSort::LastModified))
            .await?
            .into_iter()
            .next()
            .context("no books in the library to bench with")?,
    };

    let caps = cli.kitty_compress.apply(render3d::caps::probe());
    render3d::caps::restore_passthrough();
    let (cols, rows) = cli
        .dump_frame
        .as_deref()
        .map(parse_size)
        .transpose()?
        .unwrap_or((50, 26));

    let mut scene = Scene::new(engine.images_dir().to_path_buf());
    let mut params = RenderParams::default();
    let mut out = stdout();
    let id = render3d::kitty::image_id();

    let (mut trace_ns, mut encode_ns, mut write_ns, mut bytes) = (0u128, 0u128, 0u128, 0usize);
    let wall = Instant::now();
    for i in 0..frames {
        // Advance like the real spin does, so every frame is a cache miss.
        params.pose.yaw += 0.01164;
        let target = render3d::raster::target_for(
            cols,
            rows,
            caps.cell_px,
            render3d::raster::Quality::Motion,
            caps.image_wire,
        );

        let t0 = Instant::now();
        let img = {
            let cover = scene.cover(&book, target.width.clamp(24, 2048));
            let model = render3d::Model::new(&book, cover);
            render3d::raster::render_rgba(target, &model, cover, params)
        };
        trace_ns += t0.elapsed().as_nanos();

        let t1 = Instant::now();
        let esc = render3d::kitty::transmit(&img, id, cols, rows, caps.in_tmux, caps.image_wire);
        encode_ns += t1.elapsed().as_nanos();

        let t2 = Instant::now();
        out.write_all(esc.as_bytes())?;
        out.flush()?;
        write_ns += t2.elapsed().as_nanos();
        bytes += esc.len();

        if i == 0 {
            eprintln!(
                "frame is {}x{} px, {} KB",
                target.width,
                target.height,
                esc.len() / 1024
            );
        }
    }
    let total = wall.elapsed();
    let _ = out.write_all(render3d::kitty::delete(id, caps.in_tmux).as_bytes());
    let _ = out.flush();

    let ms = |ns: u128| ns as f64 / 1.0e6 / frames as f64;
    eprintln!("book        : {}", book.display_title());
    eprintln!("in tmux     : {}", caps.in_tmux);
    eprintln!("cell px     : {:?}", caps.cell_px);
    eprintln!("trace       : {:.2} ms/frame", ms(trace_ns));
    eprintln!("encode      : {:.2} ms/frame", ms(encode_ns));
    eprintln!("write+flush : {:.2} ms/frame", ms(write_ns));
    eprintln!(
        "achieved    : {:.1} fps ({:.1} MB/s)",
        frames as f64 / total.as_secs_f64(),
        bytes as f64 / total.as_secs_f64() / 1.0e6
    );
    Ok(())
}

async fn resolve_book(engine: &Engine, selector: &str) -> Result<Book> {
    let mut found = engine.resolve_books(selector).await?;
    match found.len() {
        0 => bail!("no book matched '{selector}'"),
        _ => Ok(found.remove(0)),
    }
}

/// No alternate screen and no raw mode, so it works in a pipe.
async fn dump_frame(engine: &Engine, spec: &str, cli: &Cli) -> Result<()> {
    let (w, h) = parse_size(spec)?;
    let book = match cli.book.as_deref() {
        Some(s) => resolve_book(engine, s).await?,
        None => engine
            .list_books(&BookQuery::new(1, BookSort::LastModified))
            .await?
            .into_iter()
            .next()
            .unwrap_or_else(|| Book {
                title: Some("Untitled".into()),
                ..Book::default()
            }),
    };

    let mut params = RenderParams {
        glyphs: cli.glyphs.into(),
        ..RenderParams::default()
    };
    if let Some(spec) = cli.pose.as_deref() {
        let (yaw, pitch) = spec
            .split_once(',')
            .context("--pose expects YAW,PITCH in radians")?;
        params.pose.yaw = yaw.trim().parse().context("bad yaw")?;
        params.pose.pitch = pitch.trim().parse().context("bad pitch")?;
    }

    let mut scene = Scene::new(engine.images_dir().to_path_buf());
    let fb = scene.frame(&book, w, h, params);
    if let Some(path) = &cli.dump_png {
        render3d::blit::to_png(fb, params.glyphs, path)?;
        eprintln!("wrote {}", path.display());
    }
    let mut out = stdout().lock();
    writeln!(out, "{}", book.display_title())?;
    write!(out, "{}", render3d::blit::to_ansi(fb, params.glyphs))?;
    Ok(())
}

fn parse_size(spec: &str) -> Result<(u16, u16)> {
    let (w, h) = spec
        .split_once(['x', 'X'])
        .context("expected WxH, e.g. 100x30")?;
    Ok((w.trim().parse()?, h.trim().parse()?))
}

/// Wrapping the writer is the only way to measure the glyph path: ratatui's diff produces its
/// bytes.
type CountedTerminal = Terminal<CrosstermBackend<perf::CountingWriter<Stdout>>>;

fn setup_terminal(meter: perf::Meter) -> Result<(CountedTerminal, render3d::Caps)> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    // Probe only here: raw mode is on, the alternate screen is up, and the event reader has not
    // started, so replies cannot leak into it.
    let caps = render3d::caps::probe();
    if supports_keyboard_enhancement().unwrap_or(false) {
        let _ = execute!(
            stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        previous(info);
    }));
    let counted = perf::CountingWriter::new(stdout(), meter, perf::ByteClass::Text);
    let mut terminal = Terminal::new(CrosstermBackend::new(counted))?;
    terminal.hide_cursor()?;
    terminal.clear()?;
    Ok((terminal, caps))
}

fn restore_terminal() {
    // First, on the alternate screen: the image placement lives there and ghosts if left.
    render3d::kitty::teardown();
    // Terminals that never got a push ignore the pop, so it is safe unconditionally.
    let _ = execute!(stdout(), PopKeyboardEnhancementFlags);
    let _ = disable_raw_mode();
    let _ = execute!(stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
    render3d::caps::restore_passthrough();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(mode: &'static str, image_bytes: u64) -> BenchSummary {
        BenchSummary {
            mode,
            frames: 240,
            wall: std::time::Duration::from_secs(12),
            bytes_text: 1_000_000,
            bytes_image: image_bytes,
            transmits: 3,
            rect: (50, 26),
            trace_us: 2_100,
            draw_us: 3_400,
            moving_image_bytes: image_bytes,
            rtt_kitty: vec![900, 1_200, 4_000],
            rtt_tmux: vec![300, 400],
        }
    }

    #[test]
    fn the_history_file_gets_a_header_once_and_a_row_per_run() {
        // Append, never truncate, and write the header once: a mid-file header breaks every reader.
        let dir = std::env::temp_dir().join(format!("bookmole-history-{}", std::process::id()));
        let path = dir.join("history.tsv");
        let _ = std::fs::remove_file(&path);

        let caps = render3d::Caps::default();
        append_history(
            &path,
            &[summary("glyph", 0), summary("rich", 0)],
            525,
            caps,
            "live",
        )
        .unwrap();
        append_history(&path, &[summary("glyph", 4_096)], 0, caps, "box").unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4, "expected header + 3 rows:\n{text}");
        assert!(lines[0].starts_with("epoch_s\tmode\t"), "{}", lines[0]);
        assert_eq!(
            lines.iter().filter(|l| l.starts_with("epoch_s")).count(),
            1,
            "the header was written more than once"
        );
        // Every row needs as many fields as the header, or `cut`/`column` silently misalign.
        let cols = lines[0].split('\t').count();
        for row in &lines[1..] {
            assert_eq!(row.split('\t').count(), cols, "ragged row: {row}");
        }
        assert!(
            lines[1].contains("\t0\t"),
            "moving_KB missing: {}",
            lines[1]
        );
        assert!(lines[3].contains("\t4\t"), "moving_KB wrong: {}", lines[3]);
        // Rows carry their environment so sandbox and live-pane runs do not trend together.
        assert!(lines[1].contains("\tlive\t"), "env missing: {}", lines[1]);
        assert!(lines[3].contains("\tbox\t"), "env missing: {}", lines[3]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_history_file_with_stale_columns_is_rotated_not_appended_to() {
        // Rows meaning different things must not share a header, and old rows must survive.
        let dir = std::env::temp_dir().join(format!("bookmole-rotate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.tsv");
        std::fs::write(&path, "epoch_s\tmode\told_column\n1\tglyph\ttrue\n").unwrap();

        append_history(
            &path,
            &[summary("glyph", 0)],
            525,
            render3d::Caps::default(),
            "live",
        )
        .unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with(HISTORY_HEADER),
            "the new file did not get the current header"
        );
        assert_eq!(text.lines().count(), 2, "old rows leaked into the new file");
        let kept = std::fs::read_to_string(dir.join("history.tsv.old")).unwrap();
        assert!(
            kept.contains("old_column"),
            "the old run data was destroyed"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_frame_sizes() {
        assert_eq!(parse_size("100x30").unwrap(), (100, 30));
        assert_eq!(parse_size(" 40 X 20 ").unwrap(), (40, 20));
        assert!(parse_size("100").is_err());
        assert!(parse_size("axb").is_err());
    }
}
