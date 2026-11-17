use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use bookmole::{
    Book, BookQuery, BookSort, CalibreRowState, DeviceBook, DeviceState, Diagnostic, Engine,
    EngineError, FlashcardRow, Highlight, MatchCandidate, MountEvent, MountWatcher, NewNoteInput,
    NoteKind, NoteRecord, NoteScope, RankedResult, ReadNumbering, Reading, SearchRequest,
    VaultEvent, VaultWatcher,
};
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind};
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::layout::Position;
use ratatui::widgets::ListState;

use crossterm::event::KeyModifiers;

use crate::config::{self, TuiConfig};
use crate::event::Action;
use crate::render3d::{Caps, GlyphSet, Pose, RenderMode, RenderParams, Scene};
use crate::theme;
use crate::ui;
use crate::ui::input::InputState;
use crate::ui::textedit::TextEditor;

/// Radians per tick at 20fps.
pub const SPIN_SPEED: f32 = 0.01164;
const NOD: f32 = 0.06;
const NOD_SPEED: f32 = 0.011;
pub const TICK: Duration = Duration::from_millis(50);

const HOME_LIMIT: i64 = 50;
const RATING_PROMPT: &str = "rating (enter to skip)";

/// Fixed row count, not the drawn height: lists are shrink-wrapped, so a screenful varies.
const PAGE: isize = 12;

/// Cap on the back trail; `App::go` drops the second entry at the cap so the menu at the bottom
/// survives.
const TRAIL_MAX: usize = 32;

/// A row wraps; a page clamps at the ends, so paging never jumps to the opposite end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    Row(isize),
    Page(isize),
}

impl Move {
    fn land(self, cur: usize, len: usize) -> usize {
        if len == 0 {
            return 0;
        }
        let (cur, len) = (cur as isize, len as isize);
        match self {
            Move::Row(d) => (cur + d).rem_euclid(len) as usize,
            Move::Page(d) => (cur + d * PAGE).clamp(0, len - 1) as usize,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// Not the opening screen: the app opens on the menu so the bottom of the back trail is always
    /// the menu.
    Home,
    Menu,
    Library,
    Book,
    Search,
    Settings,
    Device,
    Calibre,
    Goodreads,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuItem {
    Home,
    Library,
    Continue,
    Search,
    AddIsbn,
    ImportKo,
    Device,
    Calibre,
    Goodreads,
    Cards,
    Settings,
    Quit,
}

pub const MENU: [(MenuItem, &str, &str); 12] = [
    (
        MenuItem::Home,
        "Currently reading",
        "the books you have open",
    ),
    (
        MenuItem::Library,
        "Library",
        "browse everything you've saved",
    ),
    (
        MenuItem::Continue,
        "Continue reading",
        "jump to the most recent book",
    ),
    (
        MenuItem::Search,
        "Search books",
        "find and add from OpenLibrary + Google Books",
    ),
    (
        MenuItem::AddIsbn,
        "Add by ISBN",
        "look up a single edition and save it",
    ),
    (
        MenuItem::ImportKo,
        "Import KOReader",
        "pull highlights from a .sdr / library path",
    ),
    (
        MenuItem::Device,
        "Device",
        "the shelf on your mounted reader",
    ),
    (
        MenuItem::Calibre,
        "Calibre",
        "the shelf in your calibre library",
    ),
    (
        MenuItem::Goodreads,
        "Goodreads",
        "read a CSV export, or write one",
    ),
    (
        MenuItem::Cards,
        "Flashcards",
        "count the cards waiting for export",
    ),
    (
        MenuItem::Settings,
        "Settings",
        "data locations, API key, glyph set",
    ),
    (MenuItem::Quit, "Quit", ""),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookTab {
    Info,
    Notes,
    Highlights,
    Cards,
}

pub const BOOK_TABS: [(BookTab, &str); 4] = [
    (BookTab::Info, "Info"),
    (BookTab::Notes, "Notes"),
    (BookTab::Highlights, "Highlights"),
    (BookTab::Cards, "Cards"),
];

pub struct BookView {
    pub book: Book,
    pub notes: Vec<NoteRecord>,
    pub highlights: Vec<Highlight>,
    pub cards: Vec<FlashcardRow>,
    /// Oldest first, matching `Engine::list_readings`; read numbers are counted in this order.
    pub readings: Vec<Reading>,
}

impl BookView {
    pub fn tab_len(&self, tab: BookTab) -> usize {
        match tab {
            BookTab::Info => 0,
            BookTab::Notes => self.notes.len(),
            BookTab::Highlights => self.highlights.len(),
            BookTab::Cards => self.cards.len(),
        }
    }

    fn reads(&self) -> ReadNumbering<'_> {
        ReadNumbering::new(&self.readings)
    }

    pub fn read_number(&self, h: &Highlight) -> Option<usize> {
        self.reads().number_of(h.reading_id)
    }

    pub fn shows_read_gutter(&self) -> bool {
        self.reads().is_meaningful()
    }
}

pub struct DeviceRow {
    pub book: DeviceBook,
    pub note: Option<String>,
    pub notices: Vec<Diagnostic>,
}

impl DeviceRow {
    pub(crate) fn new(book: DeviceBook) -> DeviceRow {
        DeviceRow {
            book,
            note: None,
            notices: Vec::new(),
        }
    }
}

pub struct CalibreRow {
    pub calibre_id: i64,
    /// `None` rows cannot be linked; `l` must refuse them.
    pub uuid: Option<String>,
    pub title: String,
    pub authors: Vec<String>,
    pub state: CalibreRowState,
    pub note: Option<String>,
    pub notices: Vec<Diagnostic>,
}

impl CalibreRow {
    pub fn is_importable(&self) -> bool {
        self.state.is_importable()
    }

    pub fn authors_line(&self) -> Option<String> {
        (!self.authors.is_empty()).then(|| self.authors.join(", "))
    }
}

pub struct GoodreadsPreview {
    pub path: PathBuf,
    /// Not `rows.len()`: a row the importer skipped outright appears only in `warnings`.
    pub read: usize,
    pub rows: Vec<GoodreadsPreviewRow>,
    pub warnings: Vec<Diagnostic>,
    pub dry_run: bool,
    pub state: ListState,
}

impl GoodreadsPreview {
    pub fn unmatched(&self) -> usize {
        self.rows
            .iter()
            .filter(|r| matches!(r, GoodreadsPreviewRow::Unmatched { .. }))
            .count()
    }

    pub fn would_create(&self) -> usize {
        self.rows
            .iter()
            .filter(|r| {
                matches!(
                    r,
                    GoodreadsPreviewRow::Book {
                        matched_by: bookmole::GoodreadsMatch::New,
                        ..
                    }
                )
            })
            .count()
    }

    pub fn selected(&self) -> Option<&GoodreadsPreviewRow> {
        self.state.selected().and_then(|i| self.rows.get(i))
    }
}

pub enum GoodreadsPreviewRow {
    Book {
        title: String,
        matched_by: bookmole::GoodreadsMatch,
        readings: usize,
        shelves: usize,
        rating: Option<f64>,
        review: bookmole::TextOutcome,
        private_notes: bookmole::TextOutcome,
    },
    Unmatched {
        row: usize,
        title: String,
        authors: Vec<String>,
        /// Goodreads' `Book Id`; `None` for the eight-column format, whose rows cannot be linked.
        external_id: Option<String>,
        candidates: Vec<MatchCandidate>,
    },
}

/// Offers exactly the engine's candidate band (`CANDIDATE_MIN..AUTO_MATCH`); do not re-derive it.
pub struct LinkPicker {
    /// Held by the far side's durable key so a rescan cannot repoint the picker.
    pub target: LinkTarget,
    pub title: String,
    pub candidates: Vec<MatchCandidate>,
    pub state: ListState,
}

/// Each variant carries its importer's own key: sidecar path, calibre uuid (never its reusable
/// `id`), Goodreads `Book Id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    Sidecar(PathBuf),
    Calibre { uuid: String, calibre_id: i64 },
    Goodreads { external_id: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputContext {
    SearchQuery,
    LibraryFind,
    IsbnAdd,
    ProgressPage,
    KoPath,
    DevicePath,
    NotePage,
    /// Empty (or Esc) keeps the existing rating.
    ReviewRating,
    AccentHex,
    /// Empty is meaningful here only: it means calibre's own configured library.
    CalibreLibrary,
    GoodreadsPath,
    GoodreadsOut,
    ConvertInput,
    ConvertOutput,
}

#[derive(Debug, Clone)]
pub enum LinkRow {
    /// `to` is `None` for a dangling `[[wikilink]]`; kept so it resolves once that note exists.
    Out {
        title: String,
        to: Option<NoteRecord>,
    },
    In(NoteRecord),
}

pub struct LinksPane {
    /// Held whole, not by id: it may be a note outside this book's list.
    pub note: NoteRecord,
    pub rows: Vec<LinkRow>,
    pub state: ListState,
}

impl LinksPane {
    pub fn out_count(&self) -> usize {
        self.rows
            .iter()
            .filter(|r| matches!(r, LinkRow::Out { .. }))
            .count()
    }

    pub fn in_count(&self) -> usize {
        self.rows.len() - self.out_count()
    }

    fn selected(&self) -> Option<&LinkRow> {
        self.state.selected().and_then(|i| self.rows.get(i))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalibreImport {
    pub calibre_id: i64,
    pub create_ambiguous: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoodreadsJob {
    pub path: PathBuf,
    pub apply: bool,
    pub create_ambiguous: bool,
}

/// An absent calibre is not a failure: `CalibreMissing` gets its own wording, not a "couldn't
/// read" frame.
fn describe_calibre_failure(e: &EngineError) -> String {
    match e {
        EngineError::CalibreMissing { .. } => {
            "calibre's library tools aren't on this machine".to_string()
        }
        e => format!("couldn't read the library: {e}"),
    }
}

pub(crate) fn calibre_gains(tags: usize, cover: bool, files: usize) -> String {
    let mut parts = Vec::new();
    if tags > 0 {
        parts.push(match tags {
            1 => "1 tag".to_string(),
            n => format!("{n} tags"),
        });
    }
    if cover {
        parts.push("cover".to_string());
    }
    if files > 0 {
        parts.push(match files {
            1 => "1 file identified".to_string(),
            n => format!("{n} files identified"),
        });
    }
    if parts.is_empty() {
        parts.push("nothing new".to_string());
    }
    parts.join(", ")
}

pub struct PendingNote {
    pub book_id: Option<i64>,
    pub location: Option<String>,
    pub body: String,
}

pub struct Input {
    pub context: InputContext,
    pub state: InputState,
    pub prompt: &'static str,
}

pub enum ApiKeyStage {
    Entry { value: String },
    Verifying,
    Result { ok: bool, message: String },
}

pub struct ApiKeyModal {
    pub stage: ApiKeyStage,
}

#[derive(Debug, Clone)]
pub enum Confirm {
    RemoveBook {
        id: i64,
        title: String,
    },
    DeleteNote(NoteRecord),
    DiscardDraft,
    /// Asked, not done: the fallback reaches the network and adds books.
    SearchOnline(String),
    /// Every key but `y` means no: losing a file is the one outcome with no undo.
    OverwriteConversion {
        input: PathBuf,
        output: PathBuf,
    },
}

pub enum NoteTarget {
    New {
        book_id: Option<i64>,
        page: Option<i64>,
        location: Option<String>,
        highlight_id: Option<i64>,
    },
    Edit(NoteRecord),
}

pub struct NoteDraft {
    pub target: NoteTarget,
    pub editor: TextEditor,
}

impl NoteDraft {
    pub fn title(&self) -> &'static str {
        match &self.target {
            NoteTarget::New { .. } => "new note",
            NoteTarget::Edit(n) if n.kind == NoteKind::Reflection.as_str() => "reflection",
            NoteTarget::Edit(n) if n.kind == NoteKind::Review.as_str() => "review",
            NoteTarget::Edit(_) => "edit note",
        }
    }
}

#[expect(clippy::struct_excessive_bools)]
pub struct App {
    pub engine: Engine,
    pub screen: Screen,
    /// Oldest first. The menu is always the bottom: the app opens there, departures push it, going
    /// to it clears the trail.
    pub trail: Vec<Screen>,
    pub menu_index: usize,
    pub library: Vec<Book>,
    pub library_state: ListState,
    /// Applied in `refresh_library`, not at draw time, so `self.library` is what is on screen and
    /// indices into it stay valid.
    pub library_filter: Option<String>,
    /// Applied in `refresh_library` like the filter, so indices into `self.library` stay valid.
    pub library_sort: crate::ui::library::Sort,
    /// The `Reading` is the open one, not `Book`'s projection of the current reading; it carries
    /// the device's percentage.
    pub reading: Vec<(Book, Reading)>,
    pub reading_state: ListState,
    pub view: Option<BookView>,
    pub book_tab: BookTab,
    pub in_section: bool,
    pub tab_state: ListState,
    pub scene: Scene,
    pub params: RenderParams,
    pub render_mode: RenderMode,
    /// `Copy`: `present_book` reads it before taking the `&mut app.scene` borrow.
    pub caps: Caps,
    /// A direct field: `present_book` borrows it disjointly from `scene` and `view`.
    pub rich: crate::render3d::present::RichState,
    pub term_size: Option<(u16, u16)>,
    pub perf: crate::perf::Recorder,
    /// Only the bench sets this: sampling needs exclusive stdin, which the live loop's
    /// `EventStream` owns.
    pub rtt_sample: Option<crate::render3d::caps::Rtt>,
    pub layout: crate::ui::LayoutPrefs,
    pub ambient: crate::ambient::Ambient,
    pub spinning: bool,
    pub show_options: bool,
    pub help: bool,
    pub status: Option<String>,
    pub input: Option<Input>,
    pub note_editor: Option<NoteDraft>,
    /// Not a modal: routed by the book view's own handler, not `dispatch_key`.
    pub links: Option<LinksPane>,
    pub pending_note: Option<PendingNote>,
    pub pending_discard: Option<NoteDraft>,
    pub api_key: Option<ApiKeyModal>,
    /// Drained by the event loop after it draws the "verifying" frame.
    pub pending_verify: Option<String>,
    pub confirm: Option<Confirm>,
    pub pending_rating: Option<i64>,
    pub search_results: Vec<RankedResult>,
    pub search_state: ListState,
    pub device: Vec<DeviceRow>,
    pub device_state: ListState,
    /// Indices into `device`; cleared by every scan, since they only mean something against their
    /// own list.
    pub device_marks: HashSet<usize>,
    pub device_root: Option<PathBuf>,
    pub link_picker: Option<LinkPicker>,
    pub calibre: Vec<CalibreRow>,
    pub calibre_state: ListState,
    /// Indices into `calibre`; cleared by every scan, since they only mean something against their
    /// own list.
    pub calibre_marks: HashSet<usize>,
    /// `None` is calibre's default library, a real answer; `calibre_scanned` says whether a scan
    /// happened.
    pub calibre_library: Option<PathBuf>,
    pub calibre_scanned: bool,
    pub goodreads: Option<GoodreadsPreview>,
    pub pending_convert: Option<PathBuf>,
    /// Drained after the frame saying "scanning…" is drawn.
    pub pending_scan: Option<PathBuf>,
    /// Drained one per loop iteration with a redraw between; a `for` loop would freeze drawing for
    /// the whole sync.
    pub pending_pull: Option<VecDeque<PathBuf>>,
    /// `Some(None)` scans calibre's default library, hence not an `Option<PathBuf>` like
    /// `pending_scan`.
    pub pending_calibre: Option<Option<PathBuf>>,
    /// Each carries its own `create_ambiguous`; a queue of ids could not express it.
    pub pending_calibre_import: Option<VecDeque<CalibreImport>>,
    pub pending_goodreads: Option<GoodreadsJob>,
    pub dirty: bool,
    pub quit: bool,
    base_pitch: f32,
    phase: f32,
    pub accent_idx: usize,
    /// Where settings and the API key are saved; `None` when there is no home.
    config_dir: Option<PathBuf>,
}

impl App {
    pub async fn new(engine: Engine, config_dir: Option<PathBuf>) -> Result<App> {
        let scene = Scene::new(engine.images_dir().to_path_buf());
        let mut app = App {
            engine,
            screen: Screen::Menu,
            trail: Vec::new(),
            menu_index: 0,
            library: Vec::new(),
            library_state: ListState::default(),
            library_filter: None,
            library_sort: crate::ui::library::Sort::default(),
            reading: Vec::new(),
            reading_state: ListState::default(),
            view: None,
            book_tab: BookTab::Info,
            in_section: false,
            tab_state: ListState::default(),
            scene,
            params: RenderParams::default(),
            render_mode: RenderMode::default(),
            caps: Caps::default(),
            rich: crate::render3d::present::RichState::default(),
            term_size: None,
            perf: crate::perf::Recorder::disabled(crate::perf::Meter::new()),
            rtt_sample: None,
            layout: crate::ui::LayoutPrefs::default(),
            ambient: crate::ambient::Ambient::default(),
            spinning: true,
            show_options: false,
            help: false,
            status: None,
            input: None,
            note_editor: None,
            links: None,
            pending_note: None,
            pending_discard: None,
            api_key: None,
            pending_verify: None,
            confirm: None,
            pending_rating: None,
            search_results: Vec::new(),
            search_state: ListState::default(),
            device: Vec::new(),
            device_state: ListState::default(),
            device_marks: HashSet::new(),
            device_root: None,
            link_picker: None,
            calibre: Vec::new(),
            calibre_state: ListState::default(),
            calibre_marks: HashSet::new(),
            calibre_library: None,
            calibre_scanned: false,
            goodreads: None,
            pending_convert: None,
            pending_scan: None,
            pending_pull: None,
            pending_calibre: None,
            pending_calibre_import: None,
            pending_goodreads: None,
            dirty: true,
            quit: false,
            base_pitch: Pose::default().pitch,
            phase: 0.0,
            accent_idx: theme::PRESETS
                .iter()
                .position(|(_, rgb)| *rgb == theme::accent_rgb())
                .unwrap_or(0),
            config_dir,
        };
        app.refresh_library().await?;
        Ok(app)
    }

    pub fn set_render_mode(&mut self, mode: RenderMode) {
        self.render_mode = mode;
        self.dirty = true;
    }

    /// Tests and `--dump-frame` never call this, so they keep `Caps::default` and never emit graphics.
    pub fn set_caps(&mut self, caps: Caps) {
        self.caps = caps;
        self.rich.set_caps(caps);
        self.dirty = true;
    }

    pub async fn refresh_library(&mut self) -> Result<()> {
        // Fetched by recency and sorted below: SQL `ORDER BY … LIMIT 200` would let the sort key pick which
        // 200 books show. Sound over one page only; a second page needs the sort moved into SQL.
        self.library = self
            .engine
            .list_books(&BookQuery::new(200, BookSort::LastModified))
            .await?;
        if let Some(q) = self.library_filter.clone() {
            self.library.retain(|b| matches_book(b, &q));
        }
        self.library_sort.apply(&mut self.library);
        if !self.library.is_empty() && self.library_state.selected().is_none() {
            self.library_state.select(Some(0));
        }
        if self
            .library_state
            .selected()
            .is_some_and(|i| i >= self.library.len())
        {
            self.library_state
                .select((!self.library.is_empty()).then(|| self.library.len() - 1));
        }
        self.refresh_reading().await?;
        self.dirty = true;
        Ok(())
    }

    pub async fn refresh_reading(&mut self) -> Result<()> {
        self.reading = self.engine.currently_reading(HOME_LIMIT).await?;
        if !self.reading.is_empty() && self.reading_state.selected().is_none() {
            self.reading_state.select(Some(0));
        }
        if self
            .reading_state
            .selected()
            .is_some_and(|i| i >= self.reading.len())
        {
            self.reading_state
                .select((!self.reading.is_empty()).then(|| self.reading.len() - 1));
        }
        self.dirty = true;
        Ok(())
    }

    pub async fn open_book(&mut self, book: Book) -> Result<()> {
        let view = self.load_view(book).await?;
        self.view = Some(view);
        self.book_tab = BookTab::Info;
        self.in_section = false;
        self.tab_state.select(None);
        // A pane left open would show the previous book's graph under this book's title.
        self.links = None;
        self.go(Screen::Book);
        self.status = None;
        self.dirty = true;
        Ok(())
    }

    async fn load_view(&self, book: Book) -> Result<BookView> {
        let (notes, highlights, cards, readings) = match book.id {
            Some(id) => (
                self.engine.list_notes(NoteScope::Book(id), None).await?,
                self.engine.list_highlights(id).await?,
                self.engine.list_flashcards_for_book(id).await?,
                self.engine.list_readings(id).await?,
            ),
            None => (Vec::new(), Vec::new(), Vec::new(), Vec::new()),
        };
        Ok(BookView {
            book,
            notes,
            highlights,
            cards,
            readings,
        })
    }

    async fn reload_view(&mut self) -> Result<()> {
        let Some(id) = self.view.as_ref().and_then(|v| v.book.id) else {
            return Ok(());
        };
        if let Some(book) = self.engine.get_book(id).await? {
            self.view = Some(self.load_view(book).await?);
            self.clamp_tab_selection();
        }
        // A mutation can move the graph (saving rewrites edges, resolving dangling targets), so refresh it.
        self.refresh_links().await?;
        Ok(())
    }

    /// Leaving rich mode must delete the image explicitly, or it stays composited under the glyphs.
    fn toggle_renderer(&mut self) {
        if self.render_mode.is_rich() {
            self.rich.drop_image();
            self.render_mode = RenderMode::Glyph;
            self.status = Some("renderer: glyphs".into());
        } else if self.caps.supports_pixels() {
            // Always the hybrid: `RichAlways` is flag-only, never reachable by keypress.
            self.render_mode = RenderMode::Rich;
            self.status = Some("renderer: pixels when still".into());
        } else {
            self.status = Some("this terminal has no pixel protocol".into());
        }
        self.dirty = true;
    }

    fn reset_pose(&mut self) {
        self.params.pose = Pose::default();
        self.base_pitch = self.params.pose.pitch;
        self.phase = 0.0;
    }

    fn rotate_layout(&mut self) {
        self.layout.rotation = (self.layout.rotation + 1) % 4;
        self.status = Some("panes rotated ↻".into());
    }

    fn toggle_panel(&mut self) {
        self.layout.panel = !self.layout.panel;
        self.status = Some(if self.layout.panel {
            "sections shown".into()
        } else {
            "sections hidden — t to bring them back".into()
        });
    }

    fn ensure_panel(&mut self) {
        if !self.layout.panel {
            self.layout.panel = true;
            self.status = None;
        }
    }

    fn slide_divider(&mut self, delta: f32) {
        self.layout.divider_bias = (self.layout.divider_bias + delta).clamp(-1.0, 1.0);
        self.status = Some("panes resized".into());
    }

    /// The single source of truth for "moving": a parked book stops redrawing, so the pixel path
    /// cannot infer it.
    pub fn animating(&self) -> bool {
        self.spinning
            && self.screen == Screen::Book
            && self.input.is_none()
            && self.note_editor.is_none()
            && self.api_key.is_none()
            && !self.help
    }

    /// Not folded into `animating`: widening that leaves a parked book reporting `moving` forever,
    /// so the hybrid never transmits its crisp frame.
    pub fn ambient_visible(&self) -> bool {
        self.ambient.motif != crate::ambient::Motif::Off && self.screen != Screen::Book
    }

    /// Under a modal the field is still drawn but stops drifting.
    fn ambient_animating(&self) -> bool {
        self.ambient_visible()
            && self.input.is_none()
            && self.note_editor.is_none()
            && self.api_key.is_none()
            && self.link_picker.is_none()
            && !self.help
    }

    pub fn tick(&mut self) -> bool {
        // Both are evaluated before the `||`; do not short-circuit.
        let book = self.tick_book();
        let ambient = self.tick_ambient();
        book || ambient
    }

    fn tick_book(&mut self) -> bool {
        if !self.animating() {
            return false;
        }
        self.phase += NOD_SPEED;
        self.params.pose.yaw = wrap_angle(self.params.pose.yaw + SPIN_SPEED);
        self.params.pose.pitch = self.base_pitch + self.phase.sin() * NOD;
        true
    }

    fn tick_ambient(&mut self) -> bool {
        if !self.ambient_animating() {
            return false;
        }
        self.ambient.advance(TICK.as_secs_f32())
    }

    /// Arriving at the menu clears the trail; re-entering the current screen pushes nothing, or `r`
    /// on the device screen would fill the trail with itself.
    fn go(&mut self, screen: Screen) {
        if screen == Screen::Menu {
            self.trail.clear();
        } else if screen != self.screen {
            self.trail.push(self.screen);
            // Drop element 1, never 0: element 0 is the menu, the bottom of the trail.
            if self.trail.len() > TRAIL_MAX {
                self.trail.remove(1);
            }
        }
        self.screen = screen;
        self.dirty = true;
    }

    /// An empty trail is a no-op: `b` must never quit the app.
    fn back(&mut self) {
        if let Some(screen) = self.trail.pop() {
            self.screen = screen;
        }
        self.dirty = true;
    }

    pub async fn handle(&mut self, action: Action) -> Result<()> {
        self.dirty = true;
        match (self.screen, action) {
            (_, Action::Quit) => self.quit = true,
            (_, Action::Menu) => {
                self.go(Screen::Menu);
                self.status = None;
            }
            (_, Action::Refresh) => self.refresh_library().await?,
            (_, Action::Help) => self.help = true,

            (Screen::Home, Action::Up) => self.step_reading(Move::Row(-1)),
            (Screen::Home, Action::Down) => self.step_reading(Move::Row(1)),
            (Screen::Home, Action::PageUp) => self.step_reading(Move::Page(-1)),
            (Screen::Home, Action::PageDown) => self.step_reading(Move::Page(1)),
            (Screen::Home, Action::Select) => self.open_selected_reading().await?,
            (Screen::Home, Action::Reflect) => self.open_reading_note(NoteKind::Reflection).await?,
            (Screen::Home, Action::Review) => self.open_reading_note(NoteKind::Review).await?,
            (Screen::Home, Action::Query) => {
                self.start_input(InputContext::LibraryFind, "find in library", "")
            }
            (Screen::Home, Action::Back) => self.back(),

            (Screen::Menu, Action::Up) => {
                self.menu_index = (self.menu_index + MENU.len() - 1) % MENU.len();
            }
            (Screen::Menu, Action::Down) => {
                self.menu_index = (self.menu_index + 1) % MENU.len();
            }
            (Screen::Menu, Action::Select) => self.activate_menu().await?,
            // Back on the menu does nothing; `q` is the only way out, so repeated Esc cannot leave the app.
            (Screen::Menu, Action::Back) => self.back(),

            (Screen::Library, Action::Up) => self.step_library(Move::Row(-1)),
            (Screen::Library, Action::Down) => self.step_library(Move::Row(1)),
            (Screen::Library, Action::PageUp) => self.step_library(Move::Page(-1)),
            (Screen::Library, Action::PageDown) => self.step_library(Move::Page(1)),
            (Screen::Library, Action::Query) => {
                let initial = self.library_filter.clone().unwrap_or_default();
                self.start_input(InputContext::LibraryFind, "find in library", &initial)
            }
            // Esc drops the filter before leaving, so books are not hidden behind a word typed elsewhere.
            (Screen::Library, Action::Back) => {
                if self.library_filter.is_some() {
                    self.clear_library_filter().await?;
                } else {
                    self.back();
                }
            }
            (Screen::Library, Action::CycleSort) => self.cycle_library_sort().await?,
            (Screen::Library, Action::Delete) => self.ask_remove_selected(),
            (Screen::Library, Action::Select) => {
                if let Some(book) = self
                    .library_state
                    .selected()
                    .and_then(|i| self.library.get(i))
                    .cloned()
                {
                    self.open_book(book).await?;
                }
            }

            (Screen::Search, Action::Up) => self.step_search(Move::Row(-1)),
            (Screen::Search, Action::Down) => self.step_search(Move::Row(1)),
            (Screen::Search, Action::PageUp) => self.step_search(Move::Page(-1)),
            (Screen::Search, Action::PageDown) => self.step_search(Move::Page(1)),
            (Screen::Search, Action::Query) => {
                self.start_input(InputContext::SearchQuery, "search", "")
            }
            (Screen::Search, Action::Select) => self.add_search_result().await?,
            (Screen::Search, Action::Back) => self.back(),

            (Screen::Settings, Action::Select | Action::ToggleSpin) => self.toggle_glyphs(),
            (Screen::Settings, Action::Left) => self.cycle_accent(-1),
            (Screen::Settings, Action::Right) => self.cycle_accent(1),
            (Screen::Settings, Action::Query) => self.start_input(
                InputContext::AccentHex,
                "accent #RRGGBB",
                &theme::to_hex(theme::accent_rgb()),
            ),
            (Screen::Settings, Action::EditApiKey) => self.open_api_key(),
            (Screen::Settings, Action::CycleAmbient) => self.cycle_ambient(),
            (Screen::Settings, Action::Back) => self.back(),

            (Screen::Device, action) => self.handle_device(action).await?,
            (Screen::Calibre, action) => self.handle_calibre(action).await?,
            (Screen::Goodreads, action) => self.handle_goodreads(action).await?,

            (Screen::Book, action) => self.handle_book(action).await?,

            _ => self.dirty = false,
        }
        Ok(())
    }

    async fn handle_book(&mut self, action: Action) -> Result<()> {
        // The links pane replaces the Notes list: it takes the list's keys while open and is not in
        // `dispatch_key`'s modal chain. Esc backs out one level.
        if self.links.is_some() {
            match action {
                Action::Up => {
                    self.ensure_panel();
                    self.step_links(Move::Row(-1));
                    return Ok(());
                }
                Action::Down => {
                    self.ensure_panel();
                    self.step_links(Move::Row(1));
                    return Ok(());
                }
                Action::PageUp | Action::PageDown => {
                    self.ensure_panel();
                    let d = if action == Action::PageUp { -1 } else { 1 };
                    self.step_links(Move::Page(d));
                    return Ok(());
                }
                Action::Select | Action::Right => {
                    self.ensure_panel();
                    self.follow_link().await?;
                    return Ok(());
                }
                Action::Back | Action::Left | Action::Links => {
                    self.links = None;
                    self.status = None;
                    return Ok(());
                }
                _ => {}
            }
        }
        match action {
            Action::Links => self.open_links().await?,
            Action::ToggleOptions => self.show_options = !self.show_options,
            Action::Reset => {
                self.reset_pose();
                self.spinning = true;
            }
            Action::ToggleSpin => self.spinning = !self.spinning,
            Action::RotateLayout => self.rotate_layout(),
            Action::TogglePanel => self.toggle_panel(),
            Action::ToggleRenderer => self.toggle_renderer(),
            Action::GrowBook => self.slide_divider(crate::ui::DIVIDER_STEP),
            Action::ShrinkBook => self.slide_divider(-crate::ui::DIVIDER_STEP),
            Action::NewNote => self.new_note(false),
            // `reading_id: None` means the engine's current reading, opening one if the book has none.
            Action::Reflect => {
                let book_id = self.view.as_ref().and_then(|v| v.book.id);
                if let Some(id) = book_id {
                    self.open_anchored_note(id, None, NoteKind::Reflection)
                        .await?;
                }
            }
            Action::Review => {
                let book_id = self.view.as_ref().and_then(|v| v.book.id);
                if let Some(id) = book_id {
                    self.open_anchored_note(id, None, NoteKind::Review).await?;
                }
            }
            Action::Delete => self.ask_delete_selected_note(),
            Action::EditProgress => self.start_input(InputContext::ProgressPage, "page", ""),
            Action::ToggleFinished => self.toggle_finished().await?,
            Action::Export => self.export_cards().await?,

            Action::Back | Action::Left => {
                if self.in_section {
                    self.in_section = false;
                } else {
                    self.back();
                }
            }
            Action::Up => {
                self.ensure_panel();
                if self.in_section {
                    self.step_tab(Move::Row(-1));
                } else {
                    self.cycle_tab(-1);
                }
            }
            Action::Down => {
                self.ensure_panel();
                if self.in_section {
                    self.step_tab(Move::Row(1));
                } else {
                    self.cycle_tab(1);
                }
            }
            Action::PageUp | Action::PageDown => {
                self.ensure_panel();
                if self.in_section {
                    let d = if action == Action::PageUp { -1 } else { 1 };
                    self.step_tab(Move::Page(d));
                }
            }
            Action::PrevTab => {
                self.ensure_panel();
                if !self.in_section {
                    self.cycle_tab(-1);
                }
            }
            Action::Select | Action::Right => {
                self.ensure_panel();
                if self.in_section {
                    self.activate_tab_row().await?;
                } else {
                    self.enter_section();
                }
            }
            _ => self.dirty = false,
        }
        Ok(())
    }

    fn step_reading(&mut self, m: Move) {
        if self.reading.is_empty() {
            return;
        }
        let cur = self.reading_state.selected().unwrap_or(0);
        self.reading_state
            .select(Some(m.land(cur, self.reading.len())));
    }

    /// Both ids: this screen lists readings, so a reflection belongs to the one on screen.
    fn selected_reading(&self) -> Option<(i64, i64)> {
        self.reading_state
            .selected()
            .and_then(|i| self.reading.get(i))
            .and_then(|(b, r)| b.id.map(|id| (id, r.id)))
    }

    async fn open_selected_reading(&mut self) -> Result<()> {
        let book = self
            .reading_state
            .selected()
            .and_then(|i| self.reading.get(i))
            .map(|(b, _)| b.clone());
        if let Some(book) = book {
            self.open_book(book).await?;
        }
        Ok(())
    }

    async fn open_reading_note(&mut self, kind: NoteKind) -> Result<()> {
        let Some((book_id, reading_id)) = self.selected_reading() else {
            self.status = Some("nothing open to write about — / to find a book".into());
            return Ok(());
        };
        self.open_anchored_note(book_id, Some(reading_id), kind)
            .await
    }

    /// There is no `$EDITOR` path in this crate; notes open straight in `TextEditor`.
    async fn open_anchored_note(
        &mut self,
        book_id: i64,
        reading_id: Option<i64>,
        kind: NoteKind,
    ) -> Result<()> {
        let opened = match kind {
            NoteKind::Review => self.engine.open_review_record(book_id, reading_id).await,
            _ => {
                self.engine
                    .open_reflection_record(book_id, reading_id)
                    .await
            }
        };
        let record = match opened {
            Ok(record) => record,
            Err(e) => {
                self.status = Some(format!("could not open it: {e}"));
                return Ok(());
            }
        };
        let body = self.engine.note_body(&record).unwrap_or_default();
        self.note_editor = Some(NoteDraft {
            target: NoteTarget::Edit(record),
            editor: TextEditor::new(&body),
        });
        self.reload_view().await?;
        self.dirty = true;
        Ok(())
    }

    /// Plain case-insensitive substring over title and authors; the engine's fuzzy matcher would
    /// drop short queries.
    async fn find_in_library(&mut self, query: String) -> Result<()> {
        self.library_filter = Some(query.clone());
        // Reset the selection before the reload: the rows are a different set, so row 3 would be unrelated.
        self.library_state.select(None);
        self.refresh_library().await?;
        if self.library.is_empty() {
            self.library_filter = None;
            self.refresh_library().await?;
            self.confirm = Some(Confirm::SearchOnline(query));
            return Ok(());
        }
        self.status = None;
        self.go(Screen::Library);
        Ok(())
    }

    async fn clear_library_filter(&mut self) -> Result<()> {
        self.library_filter = None;
        self.library_state.select(None);
        self.refresh_library().await?;
        self.status = None;
        Ok(())
    }

    fn open_search(&mut self) {
        self.go(Screen::Search);
        self.search_results.clear();
        self.search_state.select(None);
        self.start_input(InputContext::SearchQuery, "search", "");
    }

    fn enter_section(&mut self) {
        self.in_section = true;
        self.clamp_tab_selection();
    }

    async fn activate_menu(&mut self) -> Result<()> {
        match MENU[self.menu_index].0 {
            MenuItem::Home => {
                self.refresh_reading().await?;
                self.go(Screen::Home);
            }
            MenuItem::Library => {
                self.clear_library_filter().await?;
                if self.library.is_empty() {
                    self.status =
                        Some("library is empty — add a book with Search or Add by ISBN".into());
                } else {
                    self.go(Screen::Library);
                }
            }
            MenuItem::Continue => {
                self.clear_library_filter().await?;
                match self.library.first().cloned() {
                    Some(book) => self.open_book(book).await?,
                    None => self.status = Some("nothing to continue — the library is empty".into()),
                }
            }
            MenuItem::Search => self.open_search(),
            MenuItem::AddIsbn => self.start_input(InputContext::IsbnAdd, "isbn", ""),
            MenuItem::ImportKo => self.start_input(InputContext::KoPath, "koreader path", ""),
            MenuItem::Device => self.open_device(),
            MenuItem::Calibre => self.open_calibre(),
            MenuItem::Goodreads => self.open_goodreads(),
            MenuItem::Cards => {
                let n = self.engine.list_flashcards(false).await?.len();
                self.status = Some(format!(
                    "{n} flashcard candidate(s) pending — open a book's Cards tab to export"
                ));
            }
            MenuItem::Settings => self.go(Screen::Settings),
            MenuItem::Quit => self.quit = true,
        }
        Ok(())
    }

    fn open_device(&mut self) {
        self.go(Screen::Device);
        self.link_picker = None;
        let mut mounts = bookmole::candidate_mounts();
        match mounts.len() {
            1 => self.start_scan(mounts.remove(0)),
            0 => {
                self.status =
                    Some("no reader mounted — give me a path (a library directory works)".into());
                self.start_input(InputContext::DevicePath, "device path", "");
            }
            n => {
                self.status = Some(format!(
                    "{n} readers mounted: {}",
                    mounts
                        .iter()
                        .map(|m| m.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
                self.start_input(
                    InputContext::DevicePath,
                    "device path",
                    &mounts[0].display().to_string(),
                );
            }
        }
    }

    /// Scanning, never syncing: a mount only queues the read-only walk, and only while the device
    /// screen is showing; elsewhere it is a status line.
    pub fn on_mount_event(&mut self, event: MountEvent) {
        self.dirty = true;
        match event {
            MountEvent::Arrived(mount) => {
                let asking = self
                    .input
                    .as_ref()
                    .is_some_and(|i| i.context == InputContext::DevicePath);
                if asking {
                    self.input = None;
                }
                // Not while another device is on screen: replacing the list would lose the user's marks and place.
                let looking = self.screen == Screen::Device
                    && (asking
                        || self.device_root.is_none()
                        || self.device_root.as_deref() == Some(mount.as_path()));
                if looking {
                    self.start_scan(mount);
                } else {
                    self.status = Some(format!(
                        "reader mounted: {} — m → device to bring anything across",
                        mount.display()
                    ));
                }
            }
            MountEvent::Departed(mount) => {
                if self.device_root.as_deref() == Some(mount.as_path()) {
                    // Keep the root so `r` after replugging means something.
                    self.device.clear();
                    self.device_marks.clear();
                    self.device_state.select(None);
                    self.link_picker = None;
                }
                self.status = Some(format!("{} was unplugged", mount.display()));
            }
        }
    }

    fn start_scan(&mut self, root: PathBuf) {
        self.status = Some(format!("scanning {}…", root.display()));
        self.pending_scan = Some(root);
        self.dirty = true;
    }

    pub async fn finish_scan(&mut self, root: &Path) -> Result<()> {
        self.dirty = true;
        let scan = match self.engine.scan_device(root).await {
            Ok(scan) => scan,
            Err(e) => {
                self.status = Some(format!("scan failed: {e}"));
                return Ok(());
            }
        };
        self.device_marks.clear();
        self.device = scan.books.into_iter().map(DeviceRow::new).collect();
        self.device_state
            .select((!self.device.is_empty()).then_some(0));
        self.device_root = Some(scan.root);

        let syncable = self
            .device
            .iter()
            .filter(|r| r.book.state.is_syncable())
            .count();
        self.status = Some(match scan.warnings.first() {
            Some(w) => w.detail.clone(),
            None => format!(
                "{} book(s) on the device · {syncable} to bring across ({} read, {} unchanged)",
                self.device.len(),
                scan.parsed,
                scan.cached
            ),
        });
        Ok(())
    }

    fn step_device(&mut self, m: Move) {
        if self.device.is_empty() {
            return;
        }
        let cur = self.device_state.selected().unwrap_or(0);
        self.device_state
            .select(Some(m.land(cur, self.device.len())));
    }

    async fn handle_device(&mut self, action: Action) -> Result<()> {
        if self.link_picker.is_some() {
            return self.handle_link_picker(action).await;
        }
        match action {
            Action::Up => self.step_device(Move::Row(-1)),
            Action::Down => self.step_device(Move::Row(1)),
            Action::PageUp => self.step_device(Move::Page(-1)),
            Action::PageDown => self.step_device(Move::Page(1)),
            Action::Back => self.back(),
            Action::Select => self.pull_selected(),
            Action::Mark => self.toggle_mark(),
            Action::Sync => self.sync_marked(),
            Action::Link => self.open_link_picker(),
            Action::Rescan => match self.device_root.clone() {
                Some(root) => self.start_scan(root),
                None => self.start_input(InputContext::DevicePath, "device path", ""),
            },
            Action::Query => self.start_input(
                InputContext::DevicePath,
                "device path",
                &self
                    .device_root
                    .as_ref()
                    .map(|r| r.display().to_string())
                    .unwrap_or_default(),
            ),
            _ => self.dirty = false,
        }
        Ok(())
    }

    fn selected_row(&self) -> Option<&DeviceRow> {
        self.device_state
            .selected()
            .and_then(|i| self.device.get(i))
    }

    fn toggle_mark(&mut self) {
        let Some(i) = self.device_state.selected() else {
            return;
        };
        if !self.device_marks.remove(&i) {
            self.device_marks.insert(i);
        }
    }

    fn pull_selected(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        if let DeviceState::Unreadable(d) = &row.book.state {
            // Refuse here: a scan degrades on a bad sidecar but `sync_device` errors on one.
            self.status = Some(format!("can't read that one — {}", d.detail));
            return;
        }
        let (path, title) = (row.book.path.clone(), row.book.display_title());
        self.status = Some(format!("bringing {title} across…"));
        self.queue_pull(vec![path]);
    }

    fn sync_marked(&mut self) {
        let chosen: Vec<PathBuf> = if self.device_marks.is_empty() {
            self.device
                .iter()
                .filter(|r| r.book.state.is_syncable())
                .map(|r| r.book.path.clone())
                .collect()
        } else {
            let mut marks: Vec<usize> = self.device_marks.iter().copied().collect();
            marks.sort_unstable();
            marks
                .into_iter()
                .filter_map(|i| self.device.get(i))
                .filter(|r| !matches!(r.book.state, DeviceState::Unreadable(_)))
                .map(|r| r.book.path.clone())
                .collect()
        };
        if chosen.is_empty() {
            self.status = Some("nothing to bring across — mark a row with x".into());
            return;
        }
        self.status = Some(format!("bringing {} book(s) across…", chosen.len()));
        self.queue_pull(chosen);
    }

    fn queue_pull(&mut self, paths: Vec<PathBuf>) {
        // An empty queue must never be `Some`: `has_deferred` would keep the loop's ready arm firing.
        if paths.is_empty() {
            return;
        }
        self.pending_pull
            .get_or_insert_with(VecDeque::new)
            .extend(paths);
        self.dirty = true;
    }

    fn next_pull(&mut self) -> Option<PathBuf> {
        let next = self.pending_pull.as_mut().and_then(|q| q.pop_front());
        if self.pending_pull.as_ref().is_some_and(|q| q.is_empty()) {
            self.pending_pull = None;
        }
        next
    }

    pub async fn finish_pull(&mut self, path: &Path) -> Result<()> {
        self.dirty = true;
        let left = self.pending_pull.as_ref().map_or(0, |q| q.len());
        let reports = match self
            .engine
            .sync_device(std::slice::from_ref(&path.to_path_buf()))
            .await
        {
            Ok(reports) => reports,
            Err(e) => {
                let msg = format!("pull failed: {e}");
                if let Some(row) = self.row_for_mut(path) {
                    row.note = Some(msg.clone());
                }
                self.status = Some(msg);
                return Ok(());
            }
        };

        let mut line = String::new();
        for report in reports {
            let s = &report.stats;
            line = format!(
                "{}: {} new, {} refreshed, {} already here",
                s.book_title, s.inserted, s.updated, s.skipped
            );
            if let Some(row) = self.row_for_mut(path) {
                row.note = Some(line.clone());
                row.notices = report.warnings.clone();
                row.book.book_id = Some(s.book_id);
                row.book.matched_by = Some(s.matched_by);
                row.book.state = DeviceState::Unchanged;
            }
        }
        if let Some(i) = self.index_of(path) {
            self.device_marks.remove(&i);
        }
        self.status = Some(if left > 0 {
            format!("{line}  ·  {left} to go")
        } else {
            line
        });
        if self.pending_pull.is_none() {
            self.refresh_library().await?;
        }
        Ok(())
    }

    fn index_of(&self, path: &Path) -> Option<usize> {
        self.device.iter().position(|r| r.book.path == path)
    }

    fn row_for_mut(&mut self, path: &Path) -> Option<&mut DeviceRow> {
        self.device.iter_mut().find(|r| r.book.path == path)
    }

    fn open_link_picker(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        let candidates = match &row.book.state {
            DeviceState::New { candidates } => candidates.clone(),
            _ => {
                self.status = Some(format!(
                    "{} is already linked — enter brings it across",
                    row.book.display_title()
                ));
                return;
            }
        };
        if candidates.is_empty() {
            self.status = Some(
                "nothing in the library looks like it — enter pulls it in as a new book".into(),
            );
            return;
        }
        let target = LinkTarget::Sidecar(row.book.path.clone());
        let title = row.book.display_title();
        self.open_picker(target, title, candidates);
    }

    fn open_picker(&mut self, target: LinkTarget, title: String, candidates: Vec<MatchCandidate>) {
        let mut state = ListState::default();
        state.select(Some(0));
        self.link_picker = Some(LinkPicker {
            target,
            title,
            candidates,
            state,
        });
        self.status = None;
    }

    async fn handle_link_picker(&mut self, action: Action) -> Result<()> {
        let Some(picker) = self.link_picker.as_mut() else {
            return Ok(());
        };
        let len = picker.candidates.len();
        match action {
            Action::Up | Action::Down | Action::PageUp | Action::PageDown => {
                let m = match action {
                    Action::Up => Move::Row(-1),
                    Action::Down => Move::Row(1),
                    Action::PageUp => Move::Page(-1),
                    _ => Move::Page(1),
                };
                let cur = picker.state.selected().unwrap_or(0);
                picker.state.select(Some(m.land(cur, len)));
            }
            Action::Back | Action::Left => {
                self.link_picker = None;
                self.status = Some("left it unlinked".into());
            }
            Action::Select | Action::Right => self.commit_link().await?,
            _ => self.dirty = false,
        }
        Ok(())
    }

    /// The link calls repoint; importing straight after keeps linking from looking like a no-op key.
    async fn commit_link(&mut self) -> Result<()> {
        let Some(picker) = self.link_picker.take() else {
            return Ok(());
        };
        let Some(candidate) = picker
            .state
            .selected()
            .and_then(|i| picker.candidates.get(i))
        else {
            return Ok(());
        };
        let title = candidate.title.clone();
        let book_id = candidate.book_id;

        let linked = match &picker.target {
            LinkTarget::Sidecar(path) => self.engine.link_sidecar(path, book_id).await.map(|_| ()),
            LinkTarget::Calibre { uuid, .. } => self.engine.link_calibre_book(uuid, book_id).await,
            LinkTarget::Goodreads { external_id } => {
                self.engine.link_goodreads_row(external_id, book_id).await
            }
        };
        if let Err(e) = linked {
            self.status = Some(format!("could not link it: {e}"));
            return Ok(());
        }
        self.status = Some(format!(
            "linked {} → {title} · bringing it across…",
            picker.title
        ));

        match &picker.target {
            LinkTarget::Sidecar(path) => {
                if let Some(row) = self.row_for_mut(path) {
                    row.book.book_id = Some(book_id);
                }
                self.queue_pull(vec![path.clone()]);
            }
            LinkTarget::Calibre { calibre_id, .. } => {
                if let Some(row) = self.calibre_row_mut(*calibre_id) {
                    row.state = CalibreRowState::Linked {
                        matched_by: bookmole::CalibreMatch::Uuid,
                        tags: 0,
                        cover: false,
                        files: 0,
                    };
                }
                self.queue_calibre_import(vec![CalibreImport {
                    calibre_id: *calibre_id,
                    create_ambiguous: false,
                }]);
            }
            LinkTarget::Goodreads { .. } => {
                // A CSV has no single-row import: record the link, then re-run the preview.
                if let Some(path) = self.goodreads.as_ref().map(|p| p.path.clone()) {
                    self.queue_goodreads(GoodreadsJob {
                        path,
                        apply: false,
                        create_ambiguous: false,
                    });
                    self.status = Some(format!(
                        "linked {} → {title} · reading the file again…",
                        picker.title
                    ));
                }
            }
        }
        Ok(())
    }

    /// Opens even when calibre is absent: the empty state is the report, and it prescribes nothing.
    fn open_calibre(&mut self) {
        self.go(Screen::Calibre);
        self.link_picker = None;
        self.status = None;
        if !self.engine.calibre().can_read_library() {
            self.status = Some("calibre's library tools aren't on this machine".into());
            return;
        }
        if !self.calibre_scanned {
            self.start_calibre_scan(self.calibre_library.clone());
        }
    }

    fn start_calibre_scan(&mut self, library: Option<PathBuf>) {
        self.status = Some(match &library {
            Some(p) => format!("reading {}…", p.display()),
            None => "reading your calibre library…".to_string(),
        });
        self.pending_calibre = Some(library);
        self.dirty = true;
    }

    /// Two calls joined on `calibre_id`: only the listing carries authors, uuid and the rows the
    /// importer skips; deriving the shelf from the dry run would drop untitled rows.
    pub async fn finish_calibre_scan(&mut self, library: Option<PathBuf>) -> Result<()> {
        self.dirty = true;
        let books = match self.engine.calibre_library(library.as_deref()).await {
            Ok(books) => books,
            Err(e) => {
                self.status = Some(describe_calibre_failure(&e));
                return Ok(());
            }
        };
        let report = match self
            .engine
            .import_calibre_library(&bookmole::CalibreImportOptions {
                library: library.clone(),
                dry_run: true,
                create_ambiguous: false,
                only: Vec::new(),
            })
            .await
        {
            Ok(report) => report,
            Err(e) => {
                self.status = Some(describe_calibre_failure(&e));
                return Ok(());
            }
        };

        self.calibre_marks.clear();
        self.calibre = books
            .into_iter()
            .map(|cb| {
                let state = report.row_state(cb.calibre_id);
                let title = cb.display_title().to_string();
                CalibreRow {
                    calibre_id: cb.calibre_id,
                    uuid: cb.uuid,
                    title,
                    authors: cb.authors,
                    state,
                    note: None,
                    notices: Vec::new(),
                }
            })
            .collect();
        self.calibre_state
            .select((!self.calibre.is_empty()).then_some(0));
        self.calibre_library = library;
        self.calibre_scanned = true;

        let importable = self.calibre.iter().filter(|r| r.is_importable()).count();
        let undecided = self
            .calibre
            .iter()
            .filter(|r| matches!(r.state, CalibreRowState::Candidates(_)))
            .count();
        self.status = Some(if self.calibre.is_empty() {
            "that library holds no books".to_string()
        } else {
            format!(
                "{} book(s) in calibre · {importable} to bring across · {undecided} to decide about",
                self.calibre.len()
            )
        });
        Ok(())
    }

    fn selected_calibre(&self) -> Option<&CalibreRow> {
        self.calibre_state
            .selected()
            .and_then(|i| self.calibre.get(i))
    }

    fn calibre_row_mut(&mut self, calibre_id: i64) -> Option<&mut CalibreRow> {
        self.calibre.iter_mut().find(|r| r.calibre_id == calibre_id)
    }

    fn toggle_calibre_mark(&mut self) {
        let Some(i) = self.calibre_state.selected() else {
            return;
        };
        if !self.calibre_marks.remove(&i) {
            self.calibre_marks.insert(i);
        }
    }

    fn import_selected_calibre(&mut self, create_ambiguous: bool) {
        let Some(row) = self.selected_calibre() else {
            return;
        };
        if let CalibreRowState::Unreadable(d) = &row.state {
            self.status = Some(format!("calibre gave us nothing to go on — {}", d.detail));
            return;
        }
        if matches!(row.state, CalibreRowState::Candidates(_)) && !create_ambiguous {
            self.status = Some(format!(
                "{} looks like one you have — l to link it, n if it isn't",
                row.title
            ));
            return;
        }
        let (calibre_id, title) = (row.calibre_id, row.title.clone());
        self.status = Some(format!("bringing {title} across…"));
        self.queue_calibre_import(vec![CalibreImport {
            calibre_id,
            create_ambiguous,
        }]);
    }

    /// A `Candidates` row is never swept up: importing it would create the duplicate the band prevents.
    fn sync_calibre_marked(&mut self) {
        let chosen: Vec<CalibreImport> = if self.calibre_marks.is_empty() {
            self.calibre
                .iter()
                .filter(|r| r.is_importable())
                .map(|r| CalibreImport {
                    calibre_id: r.calibre_id,
                    create_ambiguous: false,
                })
                .collect()
        } else {
            let mut marks: Vec<usize> = self.calibre_marks.iter().copied().collect();
            marks.sort_unstable();
            marks
                .into_iter()
                .filter_map(|i| self.calibre.get(i))
                .filter(|r| r.is_importable())
                .map(|r| CalibreImport {
                    calibre_id: r.calibre_id,
                    create_ambiguous: false,
                })
                .collect()
        };
        if chosen.is_empty() {
            self.status = Some("nothing to bring across — mark a row with x".into());
            return;
        }
        self.status = Some(format!("bringing {} book(s) across…", chosen.len()));
        self.queue_calibre_import(chosen);
    }

    fn queue_calibre_import(&mut self, jobs: Vec<CalibreImport>) {
        // An emptied queue left as `Some` spins the loop.
        if jobs.is_empty() {
            return;
        }
        self.pending_calibre_import
            .get_or_insert_with(VecDeque::new)
            .extend(jobs);
        self.dirty = true;
    }

    fn next_calibre_import(&mut self) -> Option<CalibreImport> {
        let next = self
            .pending_calibre_import
            .as_mut()
            .and_then(|q| q.pop_front());
        if self
            .pending_calibre_import
            .as_ref()
            .is_some_and(|q| q.is_empty())
        {
            self.pending_calibre_import = None;
        }
        next
    }

    pub async fn finish_calibre_import(&mut self, job: CalibreImport) -> Result<()> {
        self.dirty = true;
        let left = self.pending_calibre_import.as_ref().map_or(0, |q| q.len());
        let report = match self
            .engine
            .import_calibre_library(&bookmole::CalibreImportOptions {
                library: self.calibre_library.clone(),
                dry_run: false,
                create_ambiguous: job.create_ambiguous,
                only: vec![job.calibre_id],
            })
            .await
        {
            Ok(report) => report,
            Err(e) => {
                let msg = format!("import failed: {e}");
                if let Some(row) = self.calibre_row_mut(job.calibre_id) {
                    row.note = Some(msg.clone());
                }
                self.status = Some(msg);
                return Ok(());
            }
        };

        let warnings = report.warnings.clone();
        let line = match report.books.first() {
            Some(b) => {
                let gained = calibre_gains(b.tags_added, b.cover, b.files_linked);
                let line = format!("{}: {gained} (matched by {})", b.title, b.matched_by);
                if let Some(row) = self.calibre_row_mut(b.calibre_id) {
                    row.note = Some(line.clone());
                    row.notices = warnings;
                    row.state = CalibreRowState::Linked {
                        matched_by: b.matched_by,
                        tags: 0,
                        cover: false,
                        files: 0,
                    };
                }
                line
            }
            None => {
                let line = match report.unmatched.first() {
                    Some(u) => format!("{} is still undecided — l to link, n if it's new", u.title),
                    None => "calibre no longer has that row — r to look again".to_string(),
                };
                if let Some(row) = self.calibre_row_mut(job.calibre_id) {
                    row.note = Some(line.clone());
                    row.notices = warnings;
                }
                line
            }
        };

        if let Some(i) = self
            .calibre
            .iter()
            .position(|r| r.calibre_id == job.calibre_id)
        {
            self.calibre_marks.remove(&i);
        }
        self.status = Some(if left > 0 {
            format!("{line}  ·  {left} to go")
        } else {
            line
        });
        if self.pending_calibre_import.is_none() {
            self.refresh_library().await?;
        }
        Ok(())
    }

    fn open_calibre_link(&mut self) {
        let Some(row) = self.selected_calibre() else {
            return;
        };
        let candidates = match &row.state {
            CalibreRowState::Candidates(c) => c.clone(),
            _ => {
                self.status = Some(format!(
                    "{} isn't waiting on a decision — enter brings it across",
                    row.title
                ));
                return;
            }
        };
        // calibre ids are per-library and reused after a delete, so a durable link needs the uuid.
        let Some(uuid) = row.uuid.clone() else {
            self.status = Some(format!(
                "{} has no calibre uuid, so a link wouldn't survive — n imports it as new",
                row.title
            ));
            return;
        };
        let (calibre_id, title) = (row.calibre_id, row.title.clone());
        self.open_picker(LinkTarget::Calibre { uuid, calibre_id }, title, candidates);
    }

    async fn handle_calibre(&mut self, action: Action) -> Result<()> {
        if self.link_picker.is_some() {
            return self.handle_link_picker(action).await;
        }
        match action {
            Action::Up => self.step_calibre(Move::Row(-1)),
            Action::Down => self.step_calibre(Move::Row(1)),
            Action::PageUp => self.step_calibre(Move::Page(-1)),
            Action::PageDown => self.step_calibre(Move::Page(1)),
            Action::Back => self.back(),
            Action::Select => self.import_selected_calibre(false),
            Action::CreateAnyway => self.import_selected_calibre(true),
            Action::Mark => self.toggle_calibre_mark(),
            Action::Sync => self.sync_calibre_marked(),
            Action::Link => self.open_calibre_link(),
            Action::Rescan => self.start_calibre_scan(self.calibre_library.clone()),
            Action::Convert => {
                self.start_input(InputContext::ConvertInput, "convert — file to read", "")
            }
            Action::Query => {
                let initial = self
                    .calibre_library
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                self.start_input(
                    InputContext::CalibreLibrary,
                    "calibre library (empty = yours)",
                    &initial,
                )
            }
            _ => self.dirty = false,
        }
        Ok(())
    }

    fn step_calibre(&mut self, m: Move) {
        if self.calibre.is_empty() {
            return;
        }
        let cur = self.calibre_state.selected().unwrap_or(0);
        self.calibre_state
            .select(Some(m.land(cur, self.calibre.len())));
    }

    /// Matches the engine's `InvalidInput` string for an existing output, as the CLI does; there is
    /// no typed variant.
    async fn run_convert(&mut self, input: PathBuf, output: PathBuf, overwrite: bool) {
        match self.engine.convert_ebook(&input, &output, overwrite).await {
            Ok(path) => self.status = Some(format!("wrote {}", path.display())),
            Err(EngineError::CalibreMissing { tool }) => {
                self.status = Some(format!(
                    "converting needs calibre's `{tool}`, which isn't here"
                ))
            }
            Err(EngineError::InvalidInput(m)) if m.contains("already exists") => {
                self.confirm = Some(Confirm::OverwriteConversion { input, output });
            }
            Err(e) => self.status = Some(format!("couldn't convert it: {e}")),
        }
    }

    fn open_goodreads(&mut self) {
        self.go(Screen::Goodreads);
        self.link_picker = None;
        self.status = None;
    }

    fn start_goodreads(&mut self, job: GoodreadsJob) {
        self.status = Some(if job.apply {
            format!("bringing {} across…", job.path.display())
        } else {
            format!("reading {}…", job.path.display())
        });
        self.pending_goodreads = Some(job);
        self.dirty = true;
    }

    fn queue_goodreads(&mut self, job: GoodreadsJob) {
        self.pending_goodreads = Some(job);
        self.dirty = true;
    }

    pub async fn finish_goodreads(&mut self, job: GoodreadsJob) -> Result<()> {
        self.dirty = true;
        let report = match self
            .engine
            .import_goodreads(
                &job.path,
                bookmole::GoodreadsImportOptions {
                    dry_run: !job.apply,
                    create_ambiguous: job.create_ambiguous,
                },
            )
            .await
        {
            Ok(report) => report,
            Err(e) => {
                self.status = Some(format!("couldn't read it: {e}"));
                return Ok(());
            }
        };

        let mut rows: Vec<GoodreadsPreviewRow> = report
            .books
            .into_iter()
            .map(|b| GoodreadsPreviewRow::Book {
                title: b.title,
                matched_by: b.matched_by,
                readings: b.readings_added,
                shelves: b.tags_added,
                rating: b.rating,
                review: b.review,
                private_notes: b.private_notes,
            })
            .collect();
        rows.extend(
            report
                .unmatched
                .into_iter()
                .map(|u| GoodreadsPreviewRow::Unmatched {
                    row: u.row,
                    title: u.title,
                    authors: u.authors,
                    external_id: u.external_id,
                    candidates: u.candidates,
                }),
        );

        let mut state = ListState::default();
        state.select((!rows.is_empty()).then_some(0));
        let preview = GoodreadsPreview {
            path: job.path.clone(),
            read: report.rows,
            rows,
            warnings: report.warnings,
            dry_run: report.dry_run,
            state,
        };
        let (created, undecided) = (preview.would_create(), preview.unmatched());
        self.status = Some(if report.dry_run {
            format!(
                "{} row(s) read · {created} would be new · {undecided} to decide about · s brings it across",
                preview.read
            )
        } else {
            format!(
                "{} row(s) read · {created} created · {undecided} to decide about",
                preview.read
            )
        });
        self.goodreads = Some(preview);
        if job.apply {
            self.refresh_library().await?;
        }
        Ok(())
    }

    async fn export_goodreads(&mut self, out: PathBuf) {
        let (csv, warnings) = match self.engine.export_goodreads().await {
            Ok(pair) => pair,
            Err(e) => {
                self.status = Some(format!("couldn't build the file: {e}"));
                return;
            }
        };
        let rows = bookmole::goodreads::row_count(&csv);
        if rows == 0 {
            self.status = Some("nothing to export yet — the library is empty".into());
            return;
        }
        if let Err(e) = std::fs::write(&out, csv) {
            self.status = Some(format!("couldn't write {}: {e}", out.display()));
            return;
        }
        let dropped = match warnings.len() {
            0 => String::new(),
            n => format!(" · {n} row(s) left out"),
        };
        self.status = Some(format!(
            "{rows} book(s) → {}{dropped} · Goodreads: My Books › Import and export",
            out.display()
        ));
        if let Some(preview) = &mut self.goodreads {
            preview.warnings = warnings;
        }
    }

    fn open_goodreads_link(&mut self) {
        let Some(preview) = &self.goodreads else {
            return;
        };
        let Some(GoodreadsPreviewRow::Unmatched {
            title,
            external_id,
            candidates,
            ..
        }) = preview.selected()
        else {
            self.status = Some("stand on a row that says “maybe” — those are the decisions".into());
            return;
        };
        if candidates.is_empty() {
            self.status =
                Some("nothing in the library looks like it — n brings it in as new".into());
            return;
        }
        let Some(external_id) = external_id.clone() else {
            self.status = Some(format!(
                "that row has no Goodreads id, so a link couldn't be recorded — n imports {title} as new"
            ));
            return;
        };
        let (title, candidates) = (title.clone(), candidates.clone());
        self.open_picker(LinkTarget::Goodreads { external_id }, title, candidates);
    }

    async fn handle_goodreads(&mut self, action: Action) -> Result<()> {
        if self.link_picker.is_some() {
            return self.handle_link_picker(action).await;
        }
        match action {
            Action::Up => self.step_goodreads(Move::Row(-1)),
            Action::Down => self.step_goodreads(Move::Row(1)),
            Action::PageUp => self.step_goodreads(Move::Page(-1)),
            Action::PageDown => self.step_goodreads(Move::Page(1)),
            Action::Back => self.back(),
            Action::Query => self.start_input(InputContext::GoodreadsPath, "goodreads csv", ""),
            Action::Export => {
                self.start_input(InputContext::GoodreadsOut, "write csv to", "goodreads.csv")
            }
            Action::Sync | Action::CreateAnyway => match self.goodreads.as_ref() {
                Some(p) => {
                    let job = GoodreadsJob {
                        path: p.path.clone(),
                        apply: true,
                        create_ambiguous: action == Action::CreateAnyway,
                    };
                    self.start_goodreads(job);
                }
                None => self.status = Some("no file yet — / to pick one".into()),
            },
            Action::Link => self.open_goodreads_link(),
            Action::Rescan => match self.goodreads.as_ref().map(|p| p.path.clone()) {
                Some(path) => self.start_goodreads(GoodreadsJob {
                    path,
                    apply: false,
                    create_ambiguous: false,
                }),
                None => self.start_input(InputContext::GoodreadsPath, "goodreads csv", ""),
            },
            _ => self.dirty = false,
        }
        Ok(())
    }

    fn step_goodreads(&mut self, m: Move) {
        let Some(preview) = &mut self.goodreads else {
            return;
        };
        if preview.rows.is_empty() {
            return;
        }
        let cur = preview.state.selected().unwrap_or(0);
        let land = m.land(cur, preview.rows.len());
        preview.state.select(Some(land));
    }

    pub fn has_deferred(&self) -> bool {
        self.pending_verify.is_some()
            || self.pending_scan.is_some()
            || self.pending_pull.is_some()
            || self.pending_calibre.is_some()
            || self.pending_calibre_import.is_some()
            || self.pending_goodreads.is_some()
    }

    /// The error boundary for everything a key or a queued job starts: a failure is shown on the
    /// status line and the app keeps running. `None` when `r` failed.
    fn absorb<T>(&mut self, r: Result<T>) -> Option<T> {
        match r {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!(error = %e, "an action failed");
                self.status = Some(format!("could not finish that: {e:#}"));
                self.dirty = true;
                None
            }
        }
    }

    /// `pump_deferred` behind the error boundary; true when there is something new to draw.
    async fn pump(&mut self) -> bool {
        let pumped = self.pump_deferred().await;
        self.absorb(pumped).unwrap_or(true)
    }

    /// Key handlers queue slow work; the loop draws the announcing frame, then runs one unit per
    /// call so `select!` stays reachable between units.
    pub async fn pump_deferred(&mut self) -> Result<bool> {
        if let Some(key) = self.pending_verify.take() {
            self.finish_verify(key).await;
            return Ok(true);
        }
        if let Some(root) = self.pending_scan.take() {
            self.finish_scan(&root).await?;
            return Ok(true);
        }
        if let Some(path) = self.next_pull() {
            self.finish_pull(&path).await?;
            return Ok(true);
        }
        if let Some(library) = self.pending_calibre.take() {
            self.finish_calibre_scan(library).await?;
            return Ok(true);
        }
        if let Some(job) = self.next_calibre_import() {
            self.finish_calibre_import(job).await?;
            return Ok(true);
        }
        if let Some(job) = self.pending_goodreads.take() {
            self.finish_goodreads(job).await?;
            return Ok(true);
        }
        Ok(false)
    }

    fn cycle_tab(&mut self, delta: isize) {
        let cur = BOOK_TABS
            .iter()
            .position(|(t, _)| *t == self.book_tab)
            .unwrap_or(0);
        let next = (cur as isize + delta).rem_euclid(BOOK_TABS.len() as isize) as usize;
        self.book_tab = BOOK_TABS[next].0;
        self.tab_state.select(None);
        self.clamp_tab_selection();
    }

    fn clamp_tab_selection(&mut self) {
        let len = self.view.as_ref().map_or(0, |v| v.tab_len(self.book_tab));
        match self.tab_state.selected() {
            _ if len == 0 => self.tab_state.select(None),
            None => self.tab_state.select(Some(0)),
            Some(i) if i >= len => self.tab_state.select(Some(len - 1)),
            _ => {}
        }
    }

    fn step_tab(&mut self, m: Move) {
        let len = self.view.as_ref().map_or(0, |v| v.tab_len(self.book_tab));
        if len == 0 {
            return;
        }
        let cur = self.tab_state.selected().unwrap_or(0);
        self.tab_state.select(Some(m.land(cur, len)));
    }

    async fn activate_tab_row(&mut self) -> Result<()> {
        match self.book_tab {
            BookTab::Notes => self.edit_selected_note().await?,
            BookTab::Highlights => self.new_note(true),
            _ => self.dirty = false,
        }
        Ok(())
    }

    fn new_note(&mut self, from_highlight: bool) {
        let Some(view) = &self.view else { return };
        let target = if from_highlight {
            let Some(h) = self
                .tab_state
                .selected()
                .and_then(|i| view.highlights.get(i))
            else {
                self.status = Some("no highlight selected".into());
                return;
            };
            NoteTarget::New {
                book_id: view.book.id,
                page: h.page,
                location: h.chapter.clone(),
                highlight_id: Some(h.id),
            }
        } else {
            NoteTarget::New {
                book_id: view.book.id,
                page: view.book.current_page,
                location: None,
                highlight_id: None,
            }
        };
        self.note_editor = Some(NoteDraft {
            target,
            editor: TextEditor::new(""),
        });
        self.dirty = true;
    }

    async fn open_links(&mut self) -> Result<()> {
        let note = (self.book_tab == BookTab::Notes && self.in_section)
            .then(|| self.selected_note())
            .flatten();
        match note {
            Some(note) => {
                self.ensure_panel();
                self.show_links_for(note).await
            }
            None => {
                self.status = Some("open Notes and pick one — L then shows its links".into());
                Ok(())
            }
        }
    }

    fn selected_note(&self) -> Option<NoteRecord> {
        self.tab_state
            .selected()
            .and_then(|i| self.view.as_ref().and_then(|v| v.notes.get(i)))
            .cloned()
    }

    async fn show_links_for(&mut self, note: NoteRecord) -> Result<()> {
        let outgoing = self.engine.outgoing_links(note.id).await?;
        let inbound = self.engine.backlinks(note.id).await?;
        let mut rows: Vec<LinkRow> = outgoing
            .into_iter()
            .map(|l| LinkRow::Out {
                title: l
                    .to
                    .as_ref()
                    .map_or_else(|| l.target_title.clone(), |n| n.title.clone()),
                to: l.to,
            })
            .collect();
        rows.extend(inbound.into_iter().map(LinkRow::In));

        let mut state = ListState::default();
        if !rows.is_empty() {
            state.select(Some(0));
        }
        self.links = Some(LinksPane { note, rows, state });
        self.dirty = true;
        Ok(())
    }

    async fn refresh_links(&mut self) -> Result<()> {
        let Some(note) = self.links.as_ref().map(|p| p.note.clone()) else {
            return Ok(());
        };
        let cursor = self.links.as_ref().and_then(|p| p.state.selected());
        self.show_links_for(note).await?;
        if let (Some(pane), Some(i)) = (self.links.as_mut(), cursor)
            && i < pane.rows.len()
        {
            pane.state.select(Some(i));
        }
        Ok(())
    }

    fn step_links(&mut self, m: Move) {
        let Some(pane) = self.links.as_mut() else {
            return;
        };
        let len = pane.rows.len();
        if len == 0 {
            return;
        }
        let cur = pane.state.selected().unwrap_or(0);
        pane.state.select(Some(m.land(cur, len)));
    }

    async fn follow_link(&mut self) -> Result<()> {
        let Some(row) = self.links.as_ref().and_then(|p| p.selected()).cloned() else {
            return Ok(());
        };
        match row {
            LinkRow::Out { title, to: None } => {
                self.status = Some(format!(
                    "“{title}” has no note yet — write one and this link resolves itself"
                ));
                self.dirty = true;
                Ok(())
            }
            LinkRow::Out { to: Some(note), .. } | LinkRow::In(note) => self.goto_note(note).await,
        }
    }

    async fn goto_note(&mut self, note: NoteRecord) -> Result<()> {
        let here = self.view.as_ref().and_then(|v| v.book.id);
        if let Some(target_book) = note.book_id
            && here != Some(target_book)
            && let Some(book) = self.engine.get_book(target_book).await?
        {
            self.open_book(book).await?;
        }
        self.book_tab = BookTab::Notes;
        self.in_section = true;
        match self
            .view
            .as_ref()
            .and_then(|v| v.notes.iter().position(|n| n.id == note.id))
        {
            Some(i) => {
                self.tab_state.select(Some(i));
                self.status = None;
            }
            None => {
                self.status = Some(format!("“{}” is not filed under this book", note.title));
            }
        }
        self.show_links_for(note).await
    }

    fn ask_delete_selected_note(&mut self) {
        // With the pane open the Notes list is off screen or stale; `d` would delete a note the user
        // cannot see.
        if self.links.is_some() {
            self.dirty = false;
            return;
        }
        if self.book_tab != BookTab::Notes || !self.in_section {
            self.dirty = false;
            return;
        }
        let note = self
            .tab_state
            .selected()
            .and_then(|i| self.view.as_ref().and_then(|v| v.notes.get(i)))
            .cloned();
        match note {
            Some(note) => self.confirm = Some(Confirm::DeleteNote(note)),
            None => self.dirty = false,
        }
    }

    async fn edit_selected_note(&mut self) -> Result<()> {
        let note = self
            .tab_state
            .selected()
            .and_then(|i| self.view.as_ref().and_then(|v| v.notes.get(i)))
            .cloned();
        let Some(note) = note else {
            return Ok(());
        };
        let body = self.engine.note_body(&note).unwrap_or_default();
        self.note_editor = Some(NoteDraft {
            target: NoteTarget::Edit(note),
            editor: TextEditor::new(&body),
        });
        self.dirty = true;
        Ok(())
    }

    async fn on_editor_key(&mut self, key: KeyEvent) -> Result<()> {
        self.dirty = true;
        let Some(draft) = self.note_editor.as_mut() else {
            return Ok(());
        };
        let newline_chord = key
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => {
                let editing_saved = matches!(draft.target, NoteTarget::Edit(_));
                if editing_saved {
                    self.note_editor = None;
                    self.status = Some("edit cancelled".into());
                } else if draft.editor.is_blank() {
                    self.note_editor = None;
                    self.status = Some("note discarded".into());
                } else {
                    self.pending_discard = self.note_editor.take();
                    self.confirm = Some(Confirm::DiscardDraft);
                    self.status = Some("discard note?  y / n".into());
                }
            }
            KeyCode::Enter if newline_chord => draft.editor.newline(),
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                draft.editor.newline()
            }
            KeyCode::Enter => self.save_note_editor().await?,
            KeyCode::Tab => draft.editor.insert('\t'),
            KeyCode::Backspace => draft.editor.backspace(),
            KeyCode::Left => draft.editor.left(),
            KeyCode::Right => draft.editor.right(),
            KeyCode::Up => draft.editor.up(),
            KeyCode::Down => draft.editor.down(),
            KeyCode::Char(c) => draft.editor.insert(c),
            _ => {}
        }
        Ok(())
    }

    /// The draft stays in `note_editor` until its write lands, so a failed save can be retried.
    async fn save_note_editor(&mut self) -> Result<()> {
        let Some(draft) = self.note_editor.as_ref() else {
            return Ok(());
        };
        if draft.editor.is_blank() {
            self.note_editor = None;
            self.status = Some("empty note, nothing saved".into());
            return Ok(());
        }
        let body = draft.editor.text();
        match &draft.target {
            NoteTarget::New {
                book_id,
                page,
                location,
                highlight_id: Some(highlight_id),
            } => {
                let input =
                    note_input(*book_id, *page, location.clone(), Some(*highlight_id), body);
                let created = self.engine.create_note(input).await?;
                self.note_editor = None;
                self.show_saved_note(&created.title).await?;
            }
            NoteTarget::New {
                book_id, location, ..
            } => {
                self.pending_note = Some(PendingNote {
                    book_id: *book_id,
                    location: location.clone(),
                    body,
                });
                self.note_editor = None;
                self.start_input(InputContext::NotePage, "page (optional, enter to skip)", "");
            }
            NoteTarget::Edit(note) => {
                let note = note.clone();
                self.engine.update_note_body(&note, &body).await?;
                self.note_editor = None;
                self.status = Some(format!("updated “{}”", note.title));
                self.reload_view().await?;
                self.clamp_tab_selection();
                if note.kind == NoteKind::Review.as_str() {
                    self.pending_rating = Some(note.id);
                    self.start_input(InputContext::ReviewRating, RATING_PROMPT, "");
                }
            }
        }
        Ok(())
    }

    async fn show_saved_note(&mut self, title: &str) -> Result<()> {
        self.status = Some(format!("saved note “{title}”"));
        self.book_tab = BookTab::Notes;
        self.in_section = true;
        self.reload_view().await?;
        self.clamp_tab_selection();
        Ok(())
    }

    /// A failed write reopens the editor with the body, so the page prompt never holds the only copy.
    async fn commit_pending_note(&mut self, page_text: String) -> Result<()> {
        let Some(pending) = self.pending_note.take() else {
            return Ok(());
        };
        let page = page_text.trim().parse::<i64>().ok().filter(|p| *p > 0);
        let input = note_input(
            pending.book_id,
            page,
            pending.location.clone(),
            None,
            pending.body.clone(),
        );
        let created = match self.engine.create_note(input).await {
            Ok(created) => created,
            Err(e) => {
                self.note_editor = Some(NoteDraft {
                    target: NoteTarget::New {
                        book_id: pending.book_id,
                        page,
                        location: pending.location,
                        highlight_id: None,
                    },
                    editor: TextEditor::new(&pending.body),
                });
                return Err(e.into());
            }
        };
        self.show_saved_note(&created.title).await
    }

    async fn toggle_finished(&mut self) -> Result<()> {
        let Some(id) = self.view.as_ref().and_then(|v| v.book.id) else {
            return Ok(());
        };
        let finished = self.view.as_ref().is_some_and(|v| v.book.finished);
        self.engine
            .update_progress(id, None, Some(!finished))
            .await?;
        self.reload_view().await?;
        self.refresh_reading().await?;
        self.status = Some(if finished {
            "marked unfinished".into()
        } else {
            "marked finished 🎉".into()
        });
        Ok(())
    }

    async fn export_cards(&mut self) -> Result<()> {
        let book_id = self.view.as_ref().and_then(|v| v.book.id);
        let saved = self.engine.save_anki_export(book_id, false).await?;
        self.status = Some(match (&saved.path, saved.count) {
            (Some(path), 1) => format!("one card went out, to {}", path.display()),
            (Some(path), n) => format!("{n} cards went out, to {}", path.display()),
            (None, _) => "no card made since the last file, so nothing was written".into(),
        });
        self.reload_view().await?;
        Ok(())
    }

    fn toggle_glyphs(&mut self) {
        self.params.glyphs = match self.params.glyphs {
            GlyphSet::Octant => GlyphSet::Quadrant,
            GlyphSet::Quadrant => GlyphSet::Octant,
        };
        self.status = Some(format!("glyph set: {:?}", self.params.glyphs));
    }

    fn cycle_accent(&mut self, dir: i64) {
        let n = theme::PRESETS.len() as i64;
        self.accent_idx = (((self.accent_idx as i64 + dir) % n + n) % n) as usize;
        let (name, rgb) = theme::PRESETS[self.accent_idx];
        theme::set_accent(rgb);
        self.persist_config();
        self.status = Some(format!("accent: {name} {}", theme::to_hex(rgb)));
    }

    fn set_accent_rgb(&mut self, rgb: u32) {
        theme::set_accent(rgb);
        self.accent_idx = theme::PRESETS
            .iter()
            .position(|(_, p)| *p == rgb)
            .unwrap_or(self.accent_idx);
        self.persist_config();
        self.status = Some(format!("accent: {}", theme::to_hex(rgb)));
    }

    pub fn set_library_sort(&mut self, sort: crate::ui::library::Sort) {
        self.library_sort = sort;
        sort.apply(&mut self.library);
        self.dirty = true;
    }

    async fn cycle_library_sort(&mut self) -> Result<()> {
        self.advance_library_sort().await?;
        self.persist_config();
        Ok(())
    }

    /// The order change itself, without the write.
    ///
    /// **The selection follows the book, not the index.** Re-sorting under a
    /// fixed index moves the cursor to whatever row happens to land there, which
    /// looks like the app choosing a different book — and the next keypress
    /// might be `d`. So the id is read before and looked up after.
    async fn advance_library_sort(&mut self) -> Result<()> {
        let standing_on = self
            .library_state
            .selected()
            .and_then(|i| self.library.get(i))
            .and_then(|b| b.id);
        self.library_sort = self.library_sort.next();
        self.refresh_library().await?;
        if let Some(id) = standing_on
            && let Some(i) = self.library.iter().position(|b| b.id == Some(id))
        {
            self.library_state.select(Some(i));
        }
        self.status = Some(format!("by {}", self.library_sort.label()));
        Ok(())
    }

    fn cycle_ambient(&mut self) {
        let motif = self.ambient.motif.next();
        self.ambient = crate::ambient::Ambient::new(motif);
        self.persist_config();
        self.status = Some(format!("ambient: {}", motif.label()));
    }

    /// Writes every field from live state: `config::save` serializes the whole struct over the file,
    /// so a literal of one field would blank the rest.
    fn persist_config(&mut self) {
        let cfg = self.config_snapshot();
        let saved = match &self.config_dir {
            Some(dir) => config::save(dir, &cfg).map(drop),
            None => Err(anyhow::anyhow!("cannot locate home directory")),
        };
        if let Err(e) = saved {
            self.status = Some(format!("could not save settings: {e:#}"));
        }
    }

    /// Every persisted TUI setting, read from live state.
    fn config_snapshot(&self) -> TuiConfig {
        TuiConfig {
            accent: Some(theme::to_hex(theme::accent_rgb())),
            ambient: Some(self.ambient.motif.label().to_string()),
            library_sort: Some(self.library_sort.label().to_string()),
        }
    }

    fn open_api_key(&mut self) {
        self.api_key = Some(ApiKeyModal {
            stage: ApiKeyStage::Entry {
                value: String::new(),
            },
        });
        self.status = None;
        self.dirty = true;
    }

    fn on_api_key_key(&mut self, key: KeyEvent) {
        self.dirty = true;
        let Some(mut modal) = self.api_key.take() else {
            return;
        };
        let mut closed = false;
        let mut next_stage = None;
        match &mut modal.stage {
            ApiKeyStage::Entry { value } => {
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                match key.code {
                    KeyCode::Esc => closed = true,
                    KeyCode::Enter => {
                        let submitted = value.trim().to_string();
                        if submitted.is_empty() {
                            self.status = Some("paste a key first — Ctrl+V".into());
                        } else {
                            self.pending_verify = Some(submitted);
                            next_stage = Some(ApiKeyStage::Verifying);
                        }
                    }
                    KeyCode::Char('v') if ctrl => match crate::clipboard::read() {
                        Ok(text) => value.push_str(&text),
                        Err(e) => self.status = Some(format!("clipboard unavailable: {e:#}")),
                    },
                    KeyCode::Backspace => {
                        value.pop();
                    }
                    KeyCode::Char(c) if !ctrl => value.push(c),
                    _ => {}
                }
            }
            ApiKeyStage::Verifying => {}
            ApiKeyStage::Result { ok, .. } => {
                if *ok {
                    closed = true;
                } else {
                    match key.code {
                        KeyCode::Esc => closed = true,
                        KeyCode::Enter | KeyCode::Char('r') => {
                            next_stage = Some(ApiKeyStage::Entry {
                                value: String::new(),
                            });
                        }
                        _ => {}
                    }
                }
            }
        }
        if let Some(stage) = next_stage {
            modal.stage = stage;
        }
        if !closed {
            self.api_key = Some(modal);
        }
    }

    async fn finish_verify(&mut self, key: String) {
        self.dirty = true;
        let stage = match bookmole::verify_google_key(&key).await {
            Ok(()) => {
                self.engine.set_google_api_key(Some(key.clone()));
                let saved = match &self.config_dir {
                    Some(dir) => config::save_google_key(dir, &key),
                    None => Err(anyhow::anyhow!("cannot locate home directory")),
                };
                match saved {
                    Ok(_) => ApiKeyStage::Result {
                        ok: true,
                        message: "verified and saved.".into(),
                    },
                    Err(e) => ApiKeyStage::Result {
                        ok: true,
                        message: format!("verified, but couldn't save it: {e:#}"),
                    },
                }
            }
            Err(reason) => ApiKeyStage::Result {
                ok: false,
                message: reason,
            },
        };
        if let Some(modal) = self.api_key.as_mut() {
            modal.stage = stage;
        }
    }

    fn ask_remove_selected(&mut self) {
        if let Some(book) = self
            .library_state
            .selected()
            .and_then(|i| self.library.get(i))
            && let Some(id) = book.id
        {
            self.confirm = Some(Confirm::RemoveBook {
                id,
                title: book.display_title().to_string(),
            });
        }
    }

    async fn resolve_confirm(&mut self, yes: bool) -> Result<()> {
        let confirm = self.confirm.take();
        self.dirty = true;
        match confirm {
            Some(Confirm::RemoveBook { id, title }) if yes => {
                self.engine.delete_book(id).await?;
                self.refresh_library().await?;
                self.status = Some(format!("removed {title}"));
            }
            Some(Confirm::DeleteNote(note)) if yes => {
                self.engine.delete_note(&note).await?;
                self.status = Some(format!("deleted “{}”", note.title));
                self.reload_view().await?;
                self.clamp_tab_selection();
            }
            Some(Confirm::DiscardDraft) if yes => {
                self.pending_discard = None;
                self.status = Some("note discarded".into());
            }
            Some(Confirm::DiscardDraft) => {
                self.note_editor = self.pending_discard.take();
                self.status = None;
            }
            // Not `open_search`: it opens an empty box, and the words are already typed.
            Some(Confirm::SearchOnline(query)) if yes => {
                self.go(Screen::Search);
                self.search_results.clear();
                self.search_state.select(None);
                self.run_search(query).await?;
            }
            Some(Confirm::SearchOnline(query)) => {
                self.status = Some(format!("nothing in the library matching “{query}”"));
            }
            // Re-enter through `run_convert` so there is one conversion path.
            Some(Confirm::OverwriteConversion { input, output }) if yes => {
                self.run_convert(input, output, true).await;
            }
            Some(Confirm::OverwriteConversion { output, .. }) => {
                self.status = Some(format!("kept {}", output.display()));
            }
            Some(_) => self.status = Some("kept.".into()),
            None => {}
        }
        Ok(())
    }

    fn start_input(&mut self, context: InputContext, prompt: &'static str, initial: &str) {
        self.input = Some(Input {
            context,
            state: InputState::new(initial),
            prompt,
        });
        self.dirty = true;
    }

    async fn on_input_key(&mut self, key: KeyEvent) -> Result<()> {
        self.dirty = true;
        let Some(input) = self.input.as_mut() else {
            return Ok(());
        };
        match key.code {
            KeyCode::Esc => {
                let context = input.context;
                self.input = None;
                if context == InputContext::NotePage {
                    self.commit_pending_note(String::new()).await?;
                } else if context == InputContext::ReviewRating {
                    self.pending_rating = None;
                } else if self.screen == Screen::Search && self.search_results.is_empty() {
                    self.back();
                }
            }
            KeyCode::Enter => {
                let raw = input.state.take();
                let (context, prompt) = (input.context, input.prompt);
                self.input = None;
                let committed = self.commit_input(context, raw.trim().to_string()).await;
                // The page prompt's text is not the draft: a failed note write reopens the editor instead.
                if committed.is_err() && self.input.is_none() && context != InputContext::NotePage {
                    self.start_input(context, prompt, &raw);
                }
                committed?;
            }
            KeyCode::Backspace => input.state.backspace(),
            KeyCode::Left => input.state.left(),
            KeyCode::Right => input.state.right(),
            KeyCode::Char(c) => input.state.insert(c),
            _ => {}
        }
        Ok(())
    }

    async fn commit_input(&mut self, context: InputContext, text: String) -> Result<()> {
        // The page prompt commits even when empty (empty = no page).
        if context == InputContext::NotePage {
            return self.commit_pending_note(text).await;
        }
        // An empty rating must still reach its handler so the pending review is dropped.
        if context == InputContext::ReviewRating {
            return self.commit_rating(text).await;
        }
        // Empty is an answer here (calibre's own library); the empty check below must not swallow it.
        if context == InputContext::CalibreLibrary {
            self.go(Screen::Calibre);
            let library = (!text.is_empty()).then(|| PathBuf::from(&text));
            self.start_calibre_scan(library);
            return Ok(());
        }
        if text.is_empty() {
            if context == InputContext::SearchQuery && self.search_results.is_empty() {
                self.back();
            }
            if context == InputContext::LibraryFind && self.library_filter.is_some() {
                self.clear_library_filter().await?;
            }
            return Ok(());
        }
        match context {
            InputContext::SearchQuery => self.run_search(text).await?,
            InputContext::LibraryFind => self.find_in_library(text).await?,
            InputContext::IsbnAdd => self.add_isbn(text).await?,
            InputContext::KoPath => self.import_ko(text).await?,
            InputContext::DevicePath => {
                self.go(Screen::Device);
                self.start_scan(PathBuf::from(text));
            }
            InputContext::ProgressPage => self.commit_progress(text).await?,
            InputContext::AccentHex => match theme::parse_hex(&text) {
                Some(rgb) => self.set_accent_rgb(rgb),
                None => self.status = Some(format!("not a #RRGGBB color: {text}")),
            },
            InputContext::GoodreadsPath => {
                self.go(Screen::Goodreads);
                self.start_goodreads(GoodreadsJob {
                    path: PathBuf::from(text),
                    apply: false,
                    create_ambiguous: false,
                });
            }
            InputContext::GoodreadsOut => self.export_goodreads(PathBuf::from(text)).await,
            InputContext::ConvertInput => {
                let input = PathBuf::from(&text);
                // calibre reads the target format off the output extension, so only that is left to type.
                let suggestion = input.with_extension("").display().to_string();
                self.pending_convert = Some(input);
                self.start_input(
                    InputContext::ConvertOutput,
                    "convert — file to write (the extension picks the format)",
                    &suggestion,
                );
            }
            InputContext::ConvertOutput => {
                if let Some(input) = self.pending_convert.take() {
                    self.run_convert(input, PathBuf::from(text), false).await;
                }
            }
            InputContext::NotePage | InputContext::ReviewRating | InputContext::CalibreLibrary => {
                unreachable!("handled above")
            }
        }
        Ok(())
    }

    /// `Storage::set_review_rating` snaps via `RatingScale::canonical`; an off-scale value comes back
    /// as an error and is shown, never rounded.
    async fn commit_rating(&mut self, text: String) -> Result<()> {
        let Some(note_id) = self.pending_rating.take() else {
            return Ok(());
        };
        let text = text.trim();
        if text.is_empty() {
            return Ok(());
        }
        let Ok(value) = text.parse::<f64>() else {
            self.status = Some(format!("'{text}' isn't a number"));
            self.reask_rating(note_id, text);
            return Ok(());
        };
        if let Err(e) = self.engine.set_rating(note_id, value).await {
            self.status = Some(format!("{e}"));
            self.reask_rating(note_id, text);
            return Ok(());
        }
        self.status = Some(match self.engine.goodreads_rating(note_id).await {
            Ok(Some(g)) => format!("rated {value} · goodreads {g}"),
            Ok(None) => format!("rated {value}"),
            Err(EngineError::UnmappedRating { value, scale }) => {
                format!("rated {value} · no goodreads mapping on '{scale}' for it yet")
            }
            Err(e) => format!("rated {value}, but: {e}"),
        });
        Ok(())
    }

    fn reask_rating(&mut self, note_id: i64, text: &str) {
        self.pending_rating = Some(note_id);
        self.start_input(InputContext::ReviewRating, RATING_PROMPT, text);
    }

    async fn commit_progress(&mut self, text: String) -> Result<()> {
        let Some(id) = self.view.as_ref().and_then(|v| v.book.id) else {
            return Ok(());
        };
        match text.parse::<i64>() {
            Ok(page) => {
                self.engine.update_progress(id, Some(page), None).await?;
                self.reload_view().await?;
                self.refresh_reading().await?;
                self.status = Some(format!("progress → page {page}"));
            }
            Err(_) => self.status = Some(format!("'{text}' isn't a page number")),
        }
        Ok(())
    }

    async fn run_search(&mut self, query: String) -> Result<()> {
        self.status = Some(format!("searching “{query}”…"));
        let req = SearchRequest {
            query: Some(query),
            limit: 15,
            ..Default::default()
        };
        match self.engine.search(&req).await {
            Ok(outcome) => {
                let degraded = outcome.failed_providers();
                self.search_results = outcome.results;
                self.search_state
                    .select((!self.search_results.is_empty()).then_some(0));

                let names: Vec<String> = degraded.iter().map(|p| p.to_string()).collect();
                let suffix = if names.is_empty() {
                    String::new()
                } else {
                    format!(" (no answer from {})", names.join(", "))
                };

                self.status = Some(if self.search_results.is_empty() {
                    format!("nothing found{suffix}")
                } else {
                    format!(
                        "{} results — enter to add, / to search again{suffix}",
                        self.search_results.len()
                    )
                });
            }
            Err(e) => self.status = Some(format!("search failed: {e}")),
        }
        Ok(())
    }

    async fn add_search_result(&mut self) -> Result<()> {
        let Some(mut book) = self
            .search_state
            .selected()
            .and_then(|i| self.search_results.get(i))
            .map(|r| r.book.clone())
        else {
            return Ok(());
        };
        if book.cover_url.is_some() {
            self.engine.download_cover(&mut book).await.ok();
        }
        let saved = self.engine.save_book_unrecorded(&book).await?;
        self.status = Some(format!("saved {}", saved.display_title()));
        self.refresh_library().await?;
        Ok(())
    }

    async fn add_isbn(&mut self, isbn: String) -> Result<()> {
        match self.engine.lookup_isbn(&isbn).await {
            Ok(Some(mut book)) => {
                if book.cover_url.is_some() {
                    self.engine.download_cover(&mut book).await.ok();
                }
                let saved = self.engine.save_book_unrecorded(&book).await?;
                self.status = Some(format!("added {}", saved.display_title()));
                self.refresh_library().await?;
            }
            Ok(None) => self.status = Some(format!("no edition found for ISBN {isbn}")),
            Err(e) => self.status = Some(format!("lookup failed: {e}")),
        }
        Ok(())
    }

    async fn import_ko(&mut self, path: String) -> Result<()> {
        let p = std::path::Path::new(&path);
        match self.engine.import_koreader(p, false).await {
            Ok(report) => {
                let inserted: usize = report.imported.iter().map(|s| s.inserted).sum();
                let books = report.imported.len();
                let unmatched = report.unmatched.len();
                self.status = Some(format!(
                    "imported {inserted} highlights across {books} book(s); {unmatched} unmatched"
                ));
                self.refresh_library().await?;
                self.reload_view().await?;
            }
            Err(e) => self.status = Some(format!("import failed: {e}")),
        }
        Ok(())
    }

    fn step_library(&mut self, m: Move) {
        if self.library.is_empty() {
            return;
        }
        let cur = self.library_state.selected().unwrap_or(0);
        self.library_state
            .select(Some(m.land(cur, self.library.len())));
    }

    fn step_search(&mut self, m: Move) {
        if self.search_results.is_empty() {
            return;
        }
        let cur = self.search_state.selected().unwrap_or(0);
        self.search_state
            .select(Some(m.land(cur, self.search_results.len())));
    }
}

fn note_input(
    book_id: Option<i64>,
    page: Option<i64>,
    location: Option<String>,
    highlight_id: Option<i64>,
    body: String,
) -> NewNoteInput {
    NewNoteInput {
        book_id,
        reading_id: None,
        highlight_id,
        page,
        location,
        kind: NoteKind::Note,
        title: None,
        body,
    }
}

/// Title and authors only; `to_lowercase`, not ASCII folding (`Ä` must find `ä`). ISBNs are not
/// searched.
fn matches_book(b: &Book, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    let hay = format!("{} {}", b.display_title(), b.display_authors()).to_lowercase();
    hay.contains(&q)
}

fn wrap_angle(a: f32) -> f32 {
    let tau = std::f32::consts::TAU;
    let mut a = a % tau;
    if a > std::f32::consts::PI {
        a -= tau;
    } else if a < -std::f32::consts::PI {
        a += tau;
    }
    a
}

/// Never resolves rather than `None`: a `select!` arm that resolves immediately forever is a spin.
async fn next_mount(watcher: &mut Option<MountWatcher>) -> MountEvent {
    match watcher {
        Some(w) => match w.next().await {
            Some(event) => event,
            None => std::future::pending().await,
        },
        None => std::future::pending().await,
    }
}

/// Same `pending()` reasoning as `next_mount`; and polling this is the work, since the re-index
/// runs inside `next()`.
async fn next_vault(watcher: &mut Option<VaultWatcher>) -> VaultEvent {
    match watcher {
        Some(w) => match w.next().await {
            Some(event) => event,
            None => std::future::pending().await,
        },
        None => std::future::pending().await,
    }
}

pub async fn run<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    mut mounts: Option<MountWatcher>,
    mut vault: Option<VaultWatcher>,
) -> Result<()> {
    let mut events = EventStream::new();
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    // Sweeps before the first frame to catch notes edited while the TUI was closed; failure is logged.
    if let Err(e) = app.engine.reconcile_vault().await {
        tracing::warn!(error = %e, "could not reconcile the vault");
    }

    redraw(terminal, app)?;

    loop {
        // `biased`: keypresses win during a sync; the last arm stops the loop waiting when work is queued.
        // Every arm's future must be cancel-safe; work that is not (scan, sync) goes through
        // `pump_deferred`.
        tokio::select! {
            biased;
            maybe_event = events.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) => on_key(app, key).await,
                    Some(Ok(Event::Resize(_, _))) => app.dirty = true,
                    Some(Err(e)) => return Err(e.into()),
                    None => break,
                    _ => {}
                }
            }
            _ = ticker.tick() => {
                if app.tick() {
                    app.dirty = true;
                }
            }
            // Above the ready arm so a mid-sync plug-in is noticed, below keypress; the handler only queues
            // work.
            event = next_mount(&mut mounts) => app.on_mount_event(event),
            // No handler on purpose: the re-index happens in the poll itself, and there is nothing to announce.
            event = next_vault(&mut vault) => {
                tracing::trace!(note = event.note_id(), "vault event");
            }
            _ = std::future::ready(()), if app.has_deferred() => {}
        }

        if app.quit {
            break;
        }
        if app.dirty {
            redraw(terminal, app)?;
        }
        if app.pump().await {
            redraw(terminal, app)?;
        }
    }
    Ok(())
}

/// Every draw goes through here so `params.moving` is authoritative; the renderer cannot infer it
/// from pose history.
pub fn redraw<B: Backend>(terminal: &mut Terminal<B>, app: &mut App) -> Result<()> {
    app.params.moving = Some(app.animating());
    let rtt = app.rtt_sample.take();
    let term = terminal.size()?;
    // A resize invalidates the image the terminal holds (`RichState::resized`); this is the only place
    // that sees every draw and the size.
    if app.term_size != Some((term.width, term.height)) {
        app.term_size = Some((term.width, term.height));
        app.rich.resized();
    }
    app.perf.frame_begin();
    let started = Instant::now();
    terminal.draw(|f| ui::draw(f, app))?;
    let draw = started.elapsed();
    park_cursor(terminal)?;
    app.perf.frame_end(crate::perf::FrameCtx {
        mode: app.render_mode.label(),
        moving: app.animating(),
        term_cols: term.width,
        term_rows: term.height,
        draw,
        rtt_kitty: rtt.and_then(|r| r.kitty),
        rtt_tmux: rtt.and_then(|r| r.tmux),
    });
    app.dirty = false;
    Ok(())
}

/// Pins the hidden cursor to the bottom-right: ratatui leaves it on the last diff cell, and some
/// terminals streak a trail after it.
fn park_cursor<B: Backend>(terminal: &mut Terminal<B>) -> Result<()> {
    let area = terminal.get_frame().area();
    if area.width == 0 || area.height == 0 {
        return Ok(());
    }
    let backend = terminal.backend_mut();
    backend.set_cursor_position(Position::new(area.width - 1, area.height - 1))?;
    backend.flush()?;
    Ok(())
}

/// Only reading or drawing the terminal ends the app; a key's failure goes to the status line.
async fn on_key(app: &mut App, key: KeyEvent) {
    let handled = dispatch_key(app, key).await;
    app.absorb(handled);
}

/// Routes a key to the open text input, a pending confirmation, or the action map, in that
/// priority.
async fn dispatch_key(app: &mut App, key: KeyEvent) -> Result<()> {
    if key.kind == KeyEventKind::Release {
        return Ok(());
    }
    // Help comes first and any key closes it: its listed keys belong to the screen underneath and must
    // not fire against it.
    if app.help {
        app.help = false;
        app.dirty = true;
        // Except ctrl-c, which no layer may swallow.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            app.quit = true;
        }
        return Ok(());
    }
    if app.note_editor.is_some() {
        app.on_editor_key(key).await?;
    } else if app.api_key.is_some() {
        app.on_api_key_key(key);
    } else if app.input.is_some() {
        app.on_input_key(key).await?;
    } else if app.confirm.is_some() {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => app.resolve_confirm(true).await?,
            _ => app.resolve_confirm(false).await?,
        }
    } else if let Some(action) = crate::event::map_key_on(app.screen, key) {
        app.handle(action).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bookmole::{EngineConfig, NewNoteInput};
    use ratatui::backend::TestBackend;

    async fn test_app() -> App {
        // `None`: detection falls through to `PATH`, so results differ between a dev machine and CI.
        test_app_with_calibre(None).await
    }

    /// `find_tool` searches `calibre_bin_dir` before `PATH`, so a fake binary gives the same answer
    /// everywhere; editing `PATH` is `unsafe` in edition 2024 and races other tests.
    #[expect(clippy::disallowed_methods)]
    async fn test_app_with_calibre(calibre_bin_dir: Option<PathBuf>) -> App {
        // A unique dir per call: tests share a process, so a per-pid vault would let them wipe each other.
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp =
            std::env::temp_dir().join(format!("bookmole-tui-tests-{}-{n}", std::process::id()));
        std::fs::remove_dir_all(&tmp).ok();
        let config = EngineConfig {
            db_url: "sqlite::memory:".into(),
            images_dir: tmp.join("images"),
            files_dir: tmp.join("files"),
            vault_dir: tmp.join("vault"),
            log_dir: tmp.join("logs"),
            exports_dir: tmp.join("exports"),
            google_api_key: None,
            calibre_bin_dir,
        };
        let engine = Engine::open(config).await.expect("engine");
        let book = engine
            .save_book(&Book {
                title: Some("Station Eleven".into()),
                authors: vec!["Emily St. John Mandel".into()],
                page_count: Some(333),
                isbn_13: Some("9781447268963".into()),
                ..Book::default()
            })
            .await
            .expect("save");
        let id = book.id.expect("id");
        // Progress is a reading, not a book column: setting `current_page` on the `Book` would be ignored.
        engine
            .update_progress(id, Some(120), None)
            .await
            .expect("seed progress");
        engine
            .storage()
            .insert_highlight(
                id,
                &bookmole::storage::NewHighlight {
                    text: "survival is insufficient".into(),
                    chapter: Some("1".into()),
                    page: Some(58),
                    pos0: Some("/body/p[1]".into()),
                    pos1: None,
                    ko_datetime: Some("2026-01-01 10:00:00".into()),
                    ko_datetime_updated: None,
                    color: None,
                    note: None,
                    source: "koreader".into(),
                },
            )
            .await
            .expect("highlight");
        engine
            .storage()
            .insert_flashcard(id, None, "insufficient", Some("ch1"))
            .await
            .expect("card");
        engine
            .create_note(NewNoteInput {
                book_id: Some(id),
                page: Some(120),
                body: "A note about the [[Symphony]].".into(),
                ..NewNoteInput::default()
            })
            .await
            .expect("note");
        let mut app = App::new(engine, Some(tmp.join("config")))
            .await
            .expect("app");
        // Never calibre's default library: that is the developer's own.
        let calibre_library = tmp.join("calibre-library");
        std::fs::create_dir_all(&calibre_library).expect("calibre library dir");
        app.calibre_library = Some(calibre_library);
        let root = write_device_tree(&tmp.join("device"));
        app.finish_scan(&root).await.expect("scan");
        app
    }

    /// Reached through `activate_menu`, not by assigning `screen`, so the back trail is the real one.
    async fn home_app() -> App {
        let mut app = test_app().await;
        app.menu_index = menu_row(MenuItem::Home);
        app.handle(Action::Select).await.expect("home");
        assert_eq!(app.screen, Screen::Home);
        app
    }

    fn sample_reading(ko_percent: Option<f64>) -> Reading {
        Reading {
            id: 1,
            book_id: 1,
            started_at: None,
            finished_at: None,
            status: "reading".into(),
            source: "manual".into(),
            current_page: None,
            ko_status: None,
            ko_percent,
            ko_rating: None,
            created_at: 0,
            last_modified: 0,
        }
    }

    /// By identity, not index: a hard-coded index goes stale when the menu gains a row.
    fn menu_row(item: MenuItem) -> usize {
        MENU.iter()
            .position(|(it, ..)| *it == item)
            .unwrap_or_else(|| panic!("no {item:?} menu row"))
    }

    /// A miniature device: one Updated book, one New, and one sidecar that is not Lua (Unreadable).
    fn write_device_tree(root: &std::path::Path) -> PathBuf {
        let sidecar = |dir: &str, title: &str, authors: &str, md5: &str, text: &str| {
            let d = root.join(dir);
            std::fs::create_dir_all(&d).expect("sdr dir");
            std::fs::write(
                d.join("metadata.epub.lua"),
                format!(
                    r#"return {{
    ["annotations"] = {{
        [1] = {{
            ["chapter"] = "One",
            ["datetime"] = "2026-02-02 09:00:00",
            ["pageno"] = 12,
            ["pos0"] = "/body/DocFragment[1]/body/p[3]/text().0",
            ["pos1"] = "/body/DocFragment[1]/body/p[3]/text().20",
            ["text"] = "{text}",
        }},
    }},
    ["doc_props"] = {{
        ["authors"] = "{authors}",
        ["title"] = "{title}",
    }},
    ["percent_finished"] = 0.42,
    ["partial_md5_checksum"] = "{md5}",
}}
"#
                ),
            )
            .expect("sidecar");
        };
        sidecar(
            "Station Eleven.sdr",
            "Station Eleven",
            "Emily St. John Mandel",
            "1111111111111111111111111111aaaa",
            "the Museum of Civilization",
        );
        sidecar(
            "Piranesi.sdr",
            "Piranesi",
            "Susanna Clarke",
            "2222222222222222222222222222bbbb",
            "the Beauty of the House is immeasurable",
        );
        let broken = root.join("Broken.sdr");
        std::fs::create_dir_all(&broken).expect("sdr dir");
        std::fs::write(broken.join("metadata.epub.lua"), "not lua {{{ &&&").expect("broken");
        root.to_path_buf()
    }

    /// A `CrosstermBackend` whose `size` and `window_size` come from the test: the real `size()` is an
    /// ioctl that fails without a tty, and `redraw` calls it every frame.
    struct FixedSize<B> {
        inner: B,
        size: ratatui::layout::Size,
    }

    impl<B: Backend> Backend for FixedSize<B> {
        fn draw<'a, I>(&mut self, content: I) -> std::io::Result<()>
        where
            I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>,
        {
            self.inner.draw(content)
        }
        fn hide_cursor(&mut self) -> std::io::Result<()> {
            self.inner.hide_cursor()
        }
        fn show_cursor(&mut self) -> std::io::Result<()> {
            self.inner.show_cursor()
        }
        fn get_cursor_position(&mut self) -> std::io::Result<Position> {
            self.inner.get_cursor_position()
        }
        fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> std::io::Result<()> {
            self.inner.set_cursor_position(position)
        }
        fn clear(&mut self) -> std::io::Result<()> {
            self.inner.clear()
        }
        fn size(&self) -> std::io::Result<ratatui::layout::Size> {
            Ok(self.size)
        }
        fn window_size(&mut self) -> std::io::Result<ratatui::backend::WindowSize> {
            Ok(ratatui::backend::WindowSize {
                columns_rows: self.size,
                // Zero means "unknown" to every caller; an invented pixel geometry would mislead the rich path.
                pixels: ratatui::layout::Size {
                    width: 0,
                    height: 0,
                },
            })
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.inner.flush()
        }
    }

    type CountedTestTerminal = ratatui::Terminal<
        FixedSize<ratatui::backend::CrosstermBackend<crate::perf::CountingWriter<Vec<u8>>>>,
    >;

    fn counted_terminal(w: u16, h: u16) -> (crate::perf::Meter, CountedTestTerminal) {
        use crate::perf::{ByteClass, CountingWriter, Meter};
        use ratatui::backend::CrosstermBackend;
        use ratatui::{TerminalOptions, Viewport};

        let meter = Meter::new();
        let backend = FixedSize {
            inner: CrosstermBackend::new(CountingWriter::new(
                Vec::new(),
                meter.clone(),
                ByteClass::Text,
            )),
            size: ratatui::layout::Size {
                width: w,
                height: h,
            },
        };
        let terminal = ratatui::Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Fixed(ratatui::layout::Rect::new(0, 0, w, h)),
            },
        )
        .expect("terminal");
        (meter, terminal)
    }

    async fn glyph_spin_bytes(w: u16, h: u16, ticks: u32) -> u64 {
        let mut app = test_app().await;
        let book = app.library.first().cloned().expect("seeded book");
        app.open_book(book).await.expect("open");
        app.render_mode = RenderMode::Glyph;
        app.spinning = true;

        let (meter, mut terminal) = counted_terminal(w, h);

        redraw(&mut terminal, &mut app).expect("first draw");
        let start = meter.snapshot();
        for _ in 0..ticks {
            app.tick();
            redraw(&mut terminal, &mut app).expect("draw");
        }
        meter.snapshot().since(start).text
    }

    /// Same harness as `glyph_spin_bytes`. `Motif::Off` reads 0, making the layer's cost a subtraction.
    async fn ambient_bytes(motif: crate::ambient::Motif, w: u16, h: u16, ticks: u32) -> u64 {
        let mut app = test_app().await;
        app.screen = Screen::Menu;
        app.ambient = crate::ambient::Ambient::new(motif);

        let (meter, mut terminal) = counted_terminal(w, h);

        redraw(&mut terminal, &mut app).expect("first draw");
        let start = meter.snapshot();
        for _ in 0..ticks {
            // The live loop's rule: only a tick that reports a change redraws.
            if app.tick() {
                redraw(&mut terminal, &mut app).expect("draw");
            }
        }
        meter.snapshot().since(start).text
    }

    /// `cargo test --release -p bookmole-tui -- --ignored --nocapture ambient_wire_rate`
    #[tokio::test]
    #[ignore = "timing, not correctness; run on a release build"]
    async fn ambient_wire_rate() {
        use crate::ambient::Motif;
        let ticks = 60;
        println!(
            "{:>10} {:>10} {:>10} {:>10}",
            "motif", "rect", "B/tick", "MB/s"
        );
        for motif in Motif::ALL {
            for (w, h) in [(80u16, 24u16), (120, 40), (200, 50)] {
                let bytes = ambient_bytes(motif, w, h, ticks).await;
                println!(
                    "{:>10} {:>10} {:>10} {:>10.4}",
                    motif.label(),
                    format!("{w}x{h}"),
                    bytes / ticks as u64,
                    bytes as f64 / (ticks as f64 * TICK.as_secs_f64()) / 1.0e6
                );
            }
        }
        println!("(off never redraws, so it is the baseline the others subtract from)");
    }

    #[tokio::test]
    async fn the_ambient_layer_stays_inside_its_byte_budget() {
        use crate::ambient::Motif;
        let ticks = 60;

        assert_eq!(
            ambient_bytes(Motif::Off, 120, 40, ticks).await,
            0,
            "an idle menu drew something"
        );

        // The ceiling is ~3x the contours rate: it catches a categorically expensive layer, not drift.
        // Contour bands must stay flat ink; a brightness ramp repaints the whole terminal every frame.
        for motif in [Motif::Motes, Motif::Contours] {
            let bytes = ambient_bytes(motif, 120, 40, ticks).await;
            let per_second = bytes as f64 / (ticks as f64 * TICK.as_secs_f64());
            assert!(
                per_second < 0.05e6,
                "{} now costs {:.3} MB/s ({bytes} bytes over {ticks} ticks)",
                motif.label(),
                per_second / 1.0e6
            );
            assert!(bytes > 0, "{} drew nothing at all", motif.label());
        }
    }

    /// `cargo test --release -p bookmole-tui -- --ignored --nocapture glyph_wire_rate`
    #[tokio::test]
    #[ignore = "timing, not correctness; run on a release build"]
    async fn glyph_wire_rate() {
        let ticks = 60;
        println!("{:>10} {:>10} {:>10}", "rect", "B/frame", "MB/s");
        for (w, h) in [(30u16, 16u16), (50, 26), (80, 30), (120, 40)] {
            let bytes = glyph_spin_bytes(w, h, ticks).await;
            println!(
                "{:>10} {:>10} {:>10.3}",
                format!("{w}x{h}"),
                bytes / ticks as u64,
                bytes as f64 / (ticks as f64 * TICK.as_secs_f64()) / 1.0e6
            );
        }
        println!("(the whole terminal, not the object rect — the object is capped inside it)");
    }

    #[tokio::test]
    async fn a_glyph_spin_stays_inside_its_byte_budget() {
        // The ceiling keeps ~5x headroom: it catches a categorically more expensive spin, not drift.
        let ticks = 60;
        let bytes = glyph_spin_bytes(50, 26, ticks).await;
        let per_second = bytes as f64 / (ticks as f64 * TICK.as_secs_f64());
        assert!(
            per_second < 0.5e6,
            "the glyph spin now costs {:.2} MB/s ({bytes} bytes over {ticks} ticks)",
            per_second / 1.0e6
        );
        assert!(bytes > 0, "the spin drew nothing at all");
    }

    #[tokio::test]
    async fn the_hybrid_costs_no_more_than_glyphs_while_moving() {
        // A moving book in rich mode must cost the same as in glyph mode; `caps` at its floor also proves
        // the fallback never leaks escapes.
        let ticks = 60;
        let glyph = glyph_spin_bytes(50, 26, ticks).await;

        let mut app = test_app().await;
        let book = app.library.first().cloned().expect("seeded book");
        app.open_book(book).await.expect("open");
        app.render_mode = RenderMode::Rich;
        app.spinning = true;

        let (meter, mut terminal) = counted_terminal(50, 26);
        redraw(&mut terminal, &mut app).expect("first draw");
        let start = meter.snapshot();
        for _ in 0..ticks {
            app.tick();
            redraw(&mut terminal, &mut app).expect("draw");
        }
        let hybrid = meter.snapshot().since(start);

        assert_eq!(hybrid.image, 0, "the hybrid sent image bytes while moving");
        assert_eq!(
            hybrid.text, glyph,
            "a moving book in rich mode drew something other than the glyph frame"
        );
    }

    /// Folding the ambient layer into `animating()` would leave a parked book reporting `moving`
    /// forever and kill the kitty hybrid, with nothing else looking broken.
    #[tokio::test]
    async fn ambient_dirties_without_claiming_the_book_is_moving() {
        let mut app = test_app().await;
        app.screen = Screen::Menu;
        app.ambient = crate::ambient::Ambient::new(crate::ambient::Motif::Motes);

        let asked = (0..20).filter(|_| app.tick()).count();
        assert!(asked > 0, "the ambient layer never asked for a redraw");
        assert!(
            !app.animating(),
            "ambient motion must never register as the book moving"
        );

        let mut terminal = ratatui::Terminal::new(TestBackend::new(80, 24)).expect("terminal");
        redraw(&mut terminal, &mut app).expect("draw");
        assert_eq!(
            app.params.moving,
            Some(false),
            "the renderer was told the book is moving while it sat parked"
        );
    }

    #[tokio::test]
    async fn the_book_screen_has_no_ambient() {
        let mut app = test_app().await;
        let book = app.library.first().cloned().expect("seeded book");
        app.open_book(book).await.expect("open");
        app.screen = Screen::Book;
        app.spinning = false;
        app.ambient = crate::ambient::Ambient::new(crate::ambient::Motif::Contours);

        assert!(!app.ambient_visible());
        assert!(
            (0..40).all(|_| !app.tick()),
            "the book screen redrew for the ambient layer"
        );

        let draw_with = |app: &mut App| {
            let mut t = ratatui::Terminal::new(TestBackend::new(120, 40)).expect("terminal");
            t.draw(|f| ui::draw(f, app)).expect("draw");
            t.backend().buffer().clone()
        };
        let with = draw_with(&mut app);
        app.ambient = crate::ambient::Ambient::new(crate::ambient::Motif::Off);
        let without = draw_with(&mut app);
        assert_eq!(with, without, "ambient leaked onto the book screen");
    }

    #[tokio::test]
    async fn an_open_modal_freezes_the_ambient() {
        let mut app = test_app().await;
        app.screen = Screen::Search;
        app.ambient = crate::ambient::Ambient::new(crate::ambient::Motif::Motes);
        app.start_input(InputContext::SearchQuery, "search", "");

        assert!(app.ambient_visible(), "the layer should still be drawn");
        assert!(
            (0..40).all(|_| !app.tick()),
            "the ambient layer kept drifting under an open input box"
        );
    }

    #[tokio::test]
    async fn a_motif_setting_does_not_disturb_the_book_spin() {
        let mut app = test_app().await;
        let book = app.library.first().cloned().expect("seeded book");
        app.open_book(book).await.expect("open");
        app.spinning = true;
        app.screen = Screen::Book;
        app.ambient = crate::ambient::Ambient::new(crate::ambient::Motif::Motes);

        let before = app.params.pose.yaw;
        assert!(app.tick(), "a spinning book must still request its redraw");
        assert_ne!(app.params.pose.yaw, before, "the book stopped turning");
    }

    /// `config::save` serializes the whole struct over the file, so persisting
    /// one setting from a struct literal silently blanks every other one —
    /// change the accent and the ambient motif is gone. Asserted on
    /// `config_snapshot`, which is what every persist path writes.
    #[tokio::test]
    async fn persisting_carries_every_setting_not_just_the_changed_one() {
        let mut app = test_app().await;
        app.ambient = crate::ambient::Ambient::new(crate::ambient::Motif::Contours);
        theme::set_accent(0x12_34_56);

        let cfg = app.config_snapshot();
        assert_eq!(cfg.ambient.as_deref(), Some("contours"));
        assert_eq!(cfg.accent.as_deref(), Some("#123456"));
    }

    #[tokio::test]
    async fn every_screen_draws_at_every_size() {
        let mut app = test_app().await;
        let book = app.library.first().cloned().expect("seeded book");
        app.open_book(book).await.expect("open");

        for motif in crate::ambient::Motif::ALL {
            app.ambient = crate::ambient::Ambient::new(motif);
            for (w, h) in [
                (120, 40),
                (80, 24),
                (40, 30),
                (30, 30),
                (20, 8),
                (4, 2),
                (1, 1),
            ] {
                draw_every_screen_once(&mut app, w, h).await;
            }
        }
    }

    #[expect(clippy::too_many_lines)]
    async fn draw_every_screen_once(app: &mut App, w: u16, h: u16) {
        {
            let app = &mut *app;
            let mut terminal = ratatui::Terminal::new(TestBackend::new(w, h)).expect("terminal");
            for screen in ALL_SCREENS {
                app.screen = screen;
                for tab in [
                    BookTab::Info,
                    BookTab::Notes,
                    BookTab::Highlights,
                    BookTab::Cards,
                ] {
                    app.book_tab = tab;
                    for in_section in [false, true] {
                        app.in_section = in_section;
                        app.clamp_tab_selection();
                        for options in [false, true] {
                            app.show_options = options;
                            app.layout.panel = options;
                            terminal.draw(|f| ui::draw(f, app)).expect("draw");
                        }
                    }
                }
                app.help = true;
                terminal.draw(|f| ui::draw(f, app)).expect("draw help");
                app.help = false;
            }
            app.screen = Screen::Search;
            app.start_input(InputContext::SearchQuery, "search", "middlemarch");
            terminal.draw(|f| ui::draw(f, app)).expect("draw input");
            app.input = None;
            app.screen = Screen::Library;
            app.confirm = Some(Confirm::RemoveBook {
                id: 1,
                title: "X".into(),
            });
            terminal.draw(|f| ui::draw(f, app)).expect("draw confirm");
            app.confirm = None;
            app.screen = Screen::Home;
            let held = std::mem::take(&mut app.reading);
            terminal
                .draw(|f| ui::draw(f, app))
                .expect("draw empty home");
            app.reading = held;
            app.screen = Screen::Device;
            app.device_marks.insert(0);
            app.link_picker = Some(LinkPicker {
                target: LinkTarget::Sidecar(PathBuf::from("/mnt/Piranesi.sdr/metadata.epub.lua")),
                title: "Piranesi".into(),
                candidates: vec![MatchCandidate {
                    book_id: 1,
                    title: "Piranesi's House".into(),
                    authors_display: vec!["Susanna Clarke".into()],
                    publish_year: Some(2020),
                    score: 0.71,
                }],
                state: ListState::default(),
            });
            terminal.draw(|f| ui::draw(f, app)).expect("draw link");
            app.link_picker = None;
            app.device_marks.clear();
            app.screen = Screen::Book;
            app.note_editor = Some(NoteDraft {
                target: NoteTarget::New {
                    book_id: Some(1),
                    page: Some(1),
                    location: None,
                    highlight_id: None,
                },
                editor: TextEditor::new("a line\nanother line"),
            });
            terminal.draw(|f| ui::draw(f, app)).expect("draw editor");
            app.note_editor = None;
            app.book_tab = BookTab::Notes;
            app.in_section = true;
            for rows in [
                vec![
                    LinkRow::Out {
                        title: "Symphony".into(),
                        to: Some(sample_note(2, "Symphony")),
                    },
                    LinkRow::Out {
                        title: "Nowhere".into(),
                        to: None,
                    },
                    LinkRow::In(sample_note(3, "Doctor Eleven")),
                ],
                Vec::new(),
            ] {
                let mut state = ListState::default();
                if !rows.is_empty() {
                    state.select(Some(0));
                }
                app.links = Some(LinksPane {
                    note: sample_note(1, "Hub"),
                    rows,
                    state,
                });
                terminal.draw(|f| ui::draw(f, app)).expect("draw links");
            }
            app.links = None;
            app.screen = Screen::Settings;
            for stage in [
                ApiKeyStage::Entry {
                    value: "AIzaSyDummyKey".into(),
                },
                ApiKeyStage::Verifying,
                ApiKeyStage::Result {
                    ok: true,
                    message: "verified and saved.".into(),
                },
                ApiKeyStage::Result {
                    ok: false,
                    message: "API key not valid".into(),
                },
            ] {
                app.api_key = Some(ApiKeyModal { stage });
                terminal.draw(|f| ui::draw(f, app)).expect("draw api key");
            }
            app.api_key = None;

            app.screen = Screen::Calibre;
            app.calibre = sample_calibre_rows();
            app.calibre_state.select(Some(0));
            app.calibre_marks.insert(0);
            terminal.draw(|f| ui::draw(f, app)).expect("draw calibre");
            app.link_picker = Some(LinkPicker {
                target: LinkTarget::Calibre {
                    uuid: "c47437a8".into(),
                    calibre_id: 3,
                },
                title: "The Dispossessed".into(),
                candidates: vec![MatchCandidate {
                    book_id: 1,
                    title: "The Dispossessed: An Ambiguous Utopia".into(),
                    authors_display: vec!["Ursula K. Le Guin".into()],
                    publish_year: Some(1974),
                    score: 0.78,
                }],
                state: ListState::default(),
            });
            terminal
                .draw(|f| ui::draw(f, app))
                .expect("draw calibre link");
            app.link_picker = None;
            app.calibre_marks.clear();
            app.calibre.clear();
            app.calibre_state.select(None);
            terminal
                .draw(|f| ui::draw(f, app))
                .expect("draw empty calibre");

            app.screen = Screen::Goodreads;
            app.goodreads = Some(sample_goodreads_preview());
            terminal.draw(|f| ui::draw(f, app)).expect("draw goodreads");
            app.goodreads = None;
            terminal
                .draw(|f| ui::draw(f, app))
                .expect("draw empty goodreads");

            app.confirm = Some(Confirm::OverwriteConversion {
                input: PathBuf::from("/books/in.epub"),
                output: PathBuf::from("/books/out.azw3"),
            });
            terminal
                .draw(|f| ui::draw(f, app))
                .expect("draw overwrite confirm");
            app.confirm = None;
        }
    }

    fn sample_calibre_rows() -> Vec<CalibreRow> {
        let row = |calibre_id: i64, title: &str, state: CalibreRowState| CalibreRow {
            calibre_id,
            uuid: Some(format!("uuid-{calibre_id}")),
            title: title.into(),
            authors: vec!["Ursula K. Le Guin".into()],
            state,
            note: None,
            notices: Vec::new(),
        };
        vec![
            row(1, "Piranesi", CalibreRowState::New),
            row(
                2,
                "Gödel, Escher, Bach",
                CalibreRowState::Linked {
                    matched_by: bookmole::CalibreMatch::Uuid,
                    tags: 2,
                    cover: true,
                    files: 1,
                },
            ),
            row(
                3,
                "The Dispossessed",
                CalibreRowState::Candidates(vec![MatchCandidate {
                    book_id: 1,
                    title: "The Dispossessed: An Ambiguous Utopia".into(),
                    authors_display: vec!["Ursula K. Le Guin".into()],
                    publish_year: Some(1974),
                    score: 0.78,
                }]),
            ),
            row(
                4,
                "(untitled)",
                CalibreRowState::Unreadable(Diagnostic {
                    kind: bookmole::DiagnosticKind::CalibreRowSkipped { calibre_id: 4 },
                    severity: bookmole::Severity::Warning,
                    detail: "no title, so there is nothing to match on".into(),
                }),
            ),
        ]
    }

    fn sample_goodreads_preview() -> GoodreadsPreview {
        let mut state = ListState::default();
        state.select(Some(0));
        GoodreadsPreview {
            path: PathBuf::from("/downloads/goodreads_library_export.csv"),
            read: 3,
            rows: vec![
                GoodreadsPreviewRow::Book {
                    title: "Middlemarch".into(),
                    matched_by: bookmole::GoodreadsMatch::Isbn,
                    readings: 1,
                    shelves: 2,
                    rating: Some(5.0),
                    review: bookmole::TextOutcome::Written,
                    private_notes: bookmole::TextOutcome::Absent,
                },
                GoodreadsPreviewRow::Book {
                    title: "Crime and Punishment".into(),
                    matched_by: bookmole::GoodreadsMatch::New,
                    readings: 3,
                    shelves: 1,
                    rating: None,
                    review: bookmole::TextOutcome::KeptOurs,
                    private_notes: bookmole::TextOutcome::Written,
                },
                GoodreadsPreviewRow::Unmatched {
                    row: 4,
                    title: "Kokoro".into(),
                    authors: vec!["Natsume Sōseki".into()],
                    external_id: None,
                    candidates: vec![MatchCandidate {
                        book_id: 2,
                        title: "Kokoro: A Novel".into(),
                        authors_display: vec!["Natsume Sōseki".into()],
                        publish_year: Some(1914),
                        score: 0.71,
                    }],
                },
            ],
            warnings: Vec::new(),
            dry_run: true,
            state,
        }
    }

    #[expect(clippy::disallowed_methods)]
    fn scratch(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("bookmole-tui-{tag}-{}-{n}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// The stub fails: tests using it expect a refusal before calibre is spawned, so reaching the
    /// binary must be loud.
    #[cfg(unix)]
    fn fake_calibre(tag: &str, name: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch(tag);
        let bin = dir.join(name);
        std::fs::write(
            &bin,
            "#!/bin/sh\necho 'the fake calibre was run' >&2\nexit 1\n",
        )
        .expect("write fake");
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        dir
    }

    fn sample_note(id: i64, title: &str) -> NoteRecord {
        NoteRecord {
            id,
            book_id: Some(1),
            reading_id: None,
            highlight_id: None,
            page: None,
            location: None,
            file_path: format!("book/{id}.md"),
            title: title.into(),
            kind: "note".into(),
            created_at: None,
            last_modified: None,
        }
    }

    /// A resize must retransmit even when the object rect is unchanged: the transmit cache is keyed on
    /// the rect, but the terminal drops images across a resize.
    #[tokio::test]
    async fn a_resize_retransmits_the_parked_image() {
        use crate::render3d::caps::{Caps, Passthrough};
        use crate::render3d::present::RichState;
        use std::sync::{Arc, Mutex};

        #[derive(Clone, Default)]
        struct Sink(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Sink {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl Sink {
            fn take(&self) -> String {
                let mut g = self.0.lock().unwrap();
                let s = String::from_utf8_lossy(&g).to_string();
                g.clear();
                s
            }
        }

        let caps = Caps {
            kitty_graphics: true,
            image_wire: crate::render3d::ImageWire::Zlib,
            cell_px: (9, 19),
            cell_px_measured: true,
            in_tmux: true,
            passthrough: Passthrough::Already,
        };
        let sink = Sink::default();

        let mut app = test_app().await;
        let book = app.library.first().cloned().expect("seeded book");
        app.open_book(book).await.expect("open");
        app.set_render_mode(RenderMode::Rich);
        app.caps = caps;
        app.rich = RichState::with_writer(caps, Box::new(sink.clone()));
        app.screen = Screen::Book;
        app.spinning = false;

        // Portrait: the object rect is 84 x 26 (both caps binding) at 120 and at 130 columns.
        let mut terminal = ratatui::Terminal::new(TestBackend::new(120, 70)).expect("terminal");
        redraw(&mut terminal, &mut app).expect("first draw");
        assert!(
            sink.take().contains("a=T"),
            "the parked book transmitted no image to begin with"
        );

        terminal.backend_mut().resize(130, 70);
        terminal.autoresize().expect("autoresize");
        redraw(&mut terminal, &mut app).expect("redraw after resize");
        assert!(
            sink.take().contains("a=T"),
            "a resize left the old image on screen and sent nothing"
        );
    }

    #[tokio::test]
    async fn rich_mode_draws_the_book_view_without_panicking() {
        let mut app = test_app().await;
        let book = app.library.first().cloned().expect("seeded book");
        app.open_book(book).await.expect("open");
        app.set_render_mode(RenderMode::Rich);
        app.screen = Screen::Book;
        for (w, h) in [(40, 60), (120, 40), (20, 8)] {
            let mut terminal = ratatui::Terminal::new(TestBackend::new(w, h)).expect("terminal");
            for in_section in [false, true] {
                app.in_section = in_section;
                terminal.draw(|f| ui::draw(f, &mut app)).expect("draw rich");
            }
        }
    }

    #[tokio::test]
    async fn rotating_toggling_and_resizing_adjust_layout_and_redraw() {
        let mut app = test_app().await;
        let book = app.library.first().cloned().expect("seeded book");
        app.open_book(book).await.expect("open");
        assert!(
            app.layout.panel,
            "the view opens with its sections in reach"
        );
        assert_eq!(app.layout.rotation, 0);
        assert_eq!(app.layout.divider_bias, 0.0);

        for expected in [1, 2, 3, 0] {
            app.handle(Action::RotateLayout).await.expect("rotate");
            assert_eq!(app.layout.rotation, expected);
        }

        app.handle(Action::TogglePanel).await.expect("toggle");
        assert!(!app.layout.panel);
        app.handle(Action::TogglePanel).await.expect("toggle");
        assert!(app.layout.panel);

        for action in [Action::Down, Action::Up, Action::Select, Action::Right] {
            app.layout.panel = false;
            app.in_section = false;
            app.handle(action).await.expect("menu key");
            assert!(app.layout.panel, "{action:?} left the pane hidden");
        }
        app.in_section = false;

        app.handle(Action::GrowBook).await.expect("grow");
        assert!(app.layout.divider_bias > 0.0);
        app.handle(Action::ShrinkBook).await.expect("shrink");
        app.handle(Action::ShrinkBook).await.expect("shrink");
        assert!(app.layout.divider_bias < 0.0);
        for _ in 0..100 {
            app.handle(Action::GrowBook).await.expect("grow");
        }
        assert!(app.layout.divider_bias <= 1.0, "bias stays clamped");

        app.screen = Screen::Book;
        for rotation in 0..4 {
            app.layout.rotation = rotation;
            app.layout.panel = rotation % 2 == 0;
            for bias in [-1.0, 0.0, 1.0] {
                app.layout.divider_bias = bias;
                for (w, h) in [(120, 40), (40, 60), (26, 8)] {
                    let mut t = ratatui::Terminal::new(TestBackend::new(w, h)).expect("term");
                    t.draw(|f| ui::draw(f, &mut app)).expect("draw");
                }
            }
        }
    }

    #[tokio::test]
    async fn menu_selects_a_section_then_the_section_navigates() {
        let mut app = test_app().await;
        let book = app.library.first().cloned().expect("seeded book");
        app.open_book(book).await.expect("open");
        assert_eq!(app.book_tab, BookTab::Info);
        assert!(!app.in_section, "opens onto the section menu");

        app.handle(Action::Down).await.expect("menu down");
        assert_eq!(app.book_tab, BookTab::Notes);
        assert!(!app.in_section);

        app.handle(Action::Select).await.expect("enter section");
        assert!(app.in_section);
        assert_eq!(app.tab_state.selected(), Some(0));

        app.handle(Action::Down).await.expect("list down");
        assert_eq!(app.tab_state.selected(), Some(0));

        app.handle(Action::Back).await.expect("back to menu");
        assert!(!app.in_section);
        assert_eq!(app.screen, Screen::Book);

        app.handle(Action::Back).await.expect("leave the book");
        assert_eq!(app.screen, Screen::Menu);
    }

    #[tokio::test]
    async fn progress_input_updates_the_book() {
        let mut app = test_app().await;
        let book = app.library.first().cloned().expect("seeded book");
        app.open_book(book).await.expect("open");

        app.handle(Action::EditProgress).await.expect("open input");
        assert!(app.input.is_some());
        for c in "200".chars() {
            app.on_input_key(KeyEvent::from(KeyCode::Char(c)))
                .await
                .expect("type");
        }
        app.on_input_key(KeyEvent::from(KeyCode::Enter))
            .await
            .expect("commit");
        assert!(app.input.is_none());
        assert_eq!(app.view.as_ref().unwrap().book.current_page, Some(200));
    }

    #[tokio::test]
    async fn new_note_asks_for_a_page_then_saves() {
        let mut app = test_app().await;
        let book = app.library.first().cloned().expect("seeded book");
        app.open_book(book).await.expect("open");
        let before = app.view.as_ref().unwrap().notes.len();

        app.handle(Action::NewNote).await.expect("open editor");
        assert!(app.note_editor.is_some());
        for c in "fresh thought".chars() {
            app.on_editor_key(KeyEvent::from(KeyCode::Char(c)))
                .await
                .expect("type");
        }
        app.on_editor_key(KeyEvent::from(KeyCode::Enter))
            .await
            .expect("finish body");
        assert!(app.note_editor.is_none());
        assert!(app.input.is_some(), "asks for a page");

        for c in "58".chars() {
            app.on_input_key(KeyEvent::from(KeyCode::Char(c)))
                .await
                .expect("type page");
        }
        app.on_input_key(KeyEvent::from(KeyCode::Enter))
            .await
            .expect("commit page");

        let notes = &app.view.as_ref().unwrap().notes;
        assert_eq!(notes.len(), before + 1);
        assert!(
            notes.iter().any(|n| n.page == Some(58)),
            "the new note carries the entered page"
        );
        assert!(app.in_section && app.book_tab == BookTab::Notes);
    }

    #[tokio::test]
    async fn editor_inserts_tabs_and_shift_enter_newlines() {
        let mut app = test_app().await;
        let book = app.library.first().cloned().expect("seeded book");
        app.open_book(book).await.expect("open");

        app.handle(Action::NewNote).await.expect("open editor");
        app.on_editor_key(KeyEvent::from(KeyCode::Char('a')))
            .await
            .expect("type");
        app.on_editor_key(KeyEvent::from(KeyCode::Tab))
            .await
            .expect("tab");
        app.on_editor_key(KeyEvent::from(KeyCode::Char('b')))
            .await
            .expect("type");
        app.on_editor_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT))
            .await
            .expect("shift-enter");
        app.on_editor_key(KeyEvent::from(KeyCode::Char('c')))
            .await
            .expect("type");

        let draft = app.note_editor.as_ref().expect("editor still open");
        assert_eq!(draft.editor.text(), "a\tb\nc");
    }

    #[tokio::test]
    async fn empty_page_prompt_saves_without_an_anchor() {
        let mut app = test_app().await;
        let book = app.library.first().cloned().expect("seeded book");
        app.open_book(book).await.expect("open");

        app.handle(Action::NewNote).await.expect("open editor");
        app.on_editor_key(KeyEvent::from(KeyCode::Char('x')))
            .await
            .expect("type");
        app.on_editor_key(KeyEvent::from(KeyCode::Enter))
            .await
            .expect("finish body");
        app.on_input_key(KeyEvent::from(KeyCode::Enter))
            .await
            .expect("skip page");

        assert!(app.input.is_none());
        assert!(
            app.view
                .as_ref()
                .unwrap()
                .notes
                .iter()
                .any(|n| n.page.is_none()),
            "the new note saved without a page"
        );
    }

    #[tokio::test]
    async fn editor_updates_a_body_preserving_frontmatter() {
        let mut app = test_app().await;
        let book = app.library.first().cloned().expect("seeded book");
        app.open_book(book).await.expect("open");
        app.book_tab = BookTab::Notes;
        app.in_section = true;
        app.clamp_tab_selection();

        let note = app.view.as_ref().unwrap().notes[0].clone();
        let path = app.engine.vault_dir().join(&note.file_path);
        let original = std::fs::read_to_string(&path).unwrap();
        assert!(original.contains("page: 120"));

        app.handle(Action::Select).await.expect("open editor");
        assert!(app.note_editor.is_some());
        if let Some(draft) = app.note_editor.as_mut() {
            for _ in 0..200 {
                draft.editor.backspace();
            }
        }
        for c in "rewritten body".chars() {
            app.on_editor_key(KeyEvent::from(KeyCode::Char(c)))
                .await
                .expect("type");
        }
        app.on_editor_key(KeyEvent::from(KeyCode::Enter))
            .await
            .expect("save");

        let updated = std::fs::read_to_string(&path).unwrap();
        assert!(updated.contains("page: 120"), "frontmatter kept");
        assert!(updated.contains("rewritten body"));
        assert!(!updated.contains("Symphony"), "old body gone");
    }

    #[tokio::test]
    async fn esc_on_an_existing_note_cancels_without_prompting() {
        let mut app = test_app().await;
        let book = app.library.first().cloned().expect("seeded book");
        app.open_book(book).await.expect("open");
        app.book_tab = BookTab::Notes;
        app.in_section = true;
        app.clamp_tab_selection();

        let note = app.view.as_ref().unwrap().notes[0].clone();
        let path = app.engine.vault_dir().join(&note.file_path);
        let original = std::fs::read_to_string(&path).unwrap();

        app.handle(Action::Select).await.expect("open editor");
        assert!(app.note_editor.is_some());
        app.on_editor_key(KeyEvent::from(KeyCode::Esc))
            .await
            .expect("esc");

        assert!(app.note_editor.is_none(), "editor closed");
        assert!(app.confirm.is_none(), "no discard prompt for a saved note");
        assert!(app.pending_discard.is_none());
        assert_eq!(app.view.as_ref().unwrap().notes.len(), 1, "note kept");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original,
            "saved note file untouched"
        );
    }

    #[tokio::test]
    async fn deleting_a_note_needs_confirmation() {
        let mut app = test_app().await;
        let book = app.library.first().cloned().expect("seeded book");
        app.open_book(book).await.expect("open");
        app.book_tab = BookTab::Notes;
        app.in_section = true;
        app.clamp_tab_selection();
        let before = app.view.as_ref().unwrap().notes.len();
        assert_eq!(before, 1);
        let path = app.engine.note_path(&app.view.as_ref().unwrap().notes[0]);
        assert!(path.exists());

        app.handle(Action::Delete).await.expect("ask");
        assert!(matches!(app.confirm, Some(Confirm::DeleteNote(_))));
        app.resolve_confirm(false).await.expect("keep");
        assert_eq!(app.view.as_ref().unwrap().notes.len(), before);
        assert!(path.exists());

        app.handle(Action::Delete).await.expect("ask again");
        app.resolve_confirm(true).await.expect("delete");
        assert!(app.view.as_ref().unwrap().notes.is_empty());
        assert!(!path.exists(), "the note file was left behind");
    }

    #[tokio::test]
    async fn deleting_a_note_only_works_in_the_open_notes_section() {
        let mut app = test_app().await;
        let book = app.library.first().cloned().expect("seeded book");
        app.open_book(book).await.expect("open");
        app.book_tab = BookTab::Notes;
        app.in_section = false;
        app.handle(Action::Delete).await.expect("menu d");
        assert!(app.confirm.is_none());

        app.book_tab = BookTab::Highlights;
        app.in_section = true;
        app.clamp_tab_selection();
        app.handle(Action::Delete).await.expect("highlights d");
        assert!(app.confirm.is_none());
    }

    /// Order is load-bearing: `Symphony` exists before the hub links to it (resolves at insert),
    /// `Nowhere` never exists (stays text), `Doctor Eleven` is written last and links back by title.
    /// Returns `(hub, symphony, inbound)` note ids.
    async fn seed_graph(app: &mut App) -> (i64, i64, i64) {
        let book_id = app.view.as_ref().and_then(|v| v.book.id);
        let make = async |title: &str, body: &str| {
            app.engine
                .create_note(NewNoteInput {
                    book_id,
                    title: Some(title.into()),
                    body: body.into(),
                    ..NewNoteInput::default()
                })
                .await
                .expect("note")
                .id
        };
        let symphony = make("Symphony", "the travelling orchestra").await;
        let hub = make("Hub", "See [[Symphony]] and [[Nowhere]].").await;
        let inbound = make("Doctor Eleven", "Back to [[Hub]].").await;
        app.reload_view().await.expect("reload");
        (hub, symphony, inbound)
    }

    async fn open_graph_pane(app: &mut App) -> (i64, i64, i64) {
        let ids = seed_graph(app).await;
        app.book_tab = BookTab::Notes;
        app.in_section = true;
        let i = app
            .view
            .as_ref()
            .unwrap()
            .notes
            .iter()
            .position(|n| n.id == ids.0)
            .expect("hub is in the book's notes");
        app.tab_state.select(Some(i));
        app.handle(Action::Links).await.expect("open links");
        ids
    }

    #[tokio::test]
    async fn the_pane_reads_both_directions_and_keeps_a_dangling_target() {
        let mut app = test_app().await;
        let book = app.library.first().cloned().expect("seeded book");
        app.open_book(book).await.expect("open");
        let (hub, symphony, inbound) = open_graph_pane(&mut app).await;

        let pane = app.links.as_ref().expect("pane open");
        assert_eq!(pane.note.id, hub);
        assert_eq!((pane.out_count(), pane.in_count()), (2, 1));

        match &pane.rows[0] {
            LinkRow::Out { title, to: Some(n) } => {
                assert_eq!(title, "Symphony");
                assert_eq!(n.id, symphony);
            }
            other => panic!("first row is the resolved outbound link, got {other:?}"),
        }
        match &pane.rows[1] {
            LinkRow::Out { title, to: None } => assert_eq!(title, "Nowhere"),
            other => panic!("the dangling target is kept as text, got {other:?}"),
        }
        match &pane.rows[2] {
            LinkRow::In(n) => assert_eq!(n.id, inbound),
            other => panic!("inbound comes after outbound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_dangling_target_names_itself_rather_than_doing_nothing() {
        let mut app = test_app().await;
        let book = app.library.first().cloned().expect("seeded book");
        app.open_book(book).await.expect("open");
        let (hub, _, _) = open_graph_pane(&mut app).await;

        app.handle(Action::Down).await.expect("down");
        app.handle(Action::Select).await.expect("follow");

        let status = app.status.clone().expect("said something");
        assert!(status.contains("Nowhere"), "{status}");
        assert_eq!(
            app.links.as_ref().expect("pane still open").note.id,
            hub,
            "a dangling target must not move the pane"
        );
    }

    #[tokio::test]
    async fn following_a_resolved_link_moves_to_that_note() {
        let mut app = test_app().await;
        let book = app.library.first().cloned().expect("seeded book");
        app.open_book(book).await.expect("open");
        let (hub, symphony, inbound) = open_graph_pane(&mut app).await;

        app.handle(Action::Select).await.expect("follow out");
        let pane = app.links.as_ref().expect("pane open");
        assert_eq!(pane.note.id, symphony);
        let selected = app
            .tab_state
            .selected()
            .and_then(|i| app.view.as_ref().unwrap().notes.get(i))
            .map(|n| n.id);
        assert_eq!(selected, Some(symphony), "the notes list follows too");

        let pane = app.links.as_ref().unwrap();
        assert_eq!(pane.out_count(), 0);
        assert!(pane.in_count() >= 1);
        let back: Vec<i64> = pane
            .rows
            .iter()
            .filter_map(|r| match r {
                LinkRow::In(n) => Some(n.id),
                _ => None,
            })
            .collect();
        assert!(back.contains(&hub), "the hub links here: {back:?}");
        assert!(!back.contains(&inbound));
    }

    #[tokio::test]
    async fn esc_closes_the_pane_back_onto_the_note_list() {
        let mut app = test_app().await;
        let book = app.library.first().cloned().expect("seeded book");
        app.open_book(book).await.expect("open");
        open_graph_pane(&mut app).await;

        app.handle(Action::Back).await.expect("esc");
        assert!(app.links.is_none());
        assert!(app.in_section && app.book_tab == BookTab::Notes);
        assert_eq!(app.screen, Screen::Book);

        app.handle(Action::Links).await.expect("reopen");
        assert!(app.links.is_some());
        app.handle(Action::Links).await.expect("close");
        assert!(app.links.is_none());

        app.layout.panel = false;
        app.handle(Action::Links).await.expect("open with no panel");
        assert!(app.links.is_some() && app.layout.panel);
    }

    #[tokio::test]
    async fn the_pane_refuses_with_a_next_move_when_no_note_is_selected() {
        let mut app = test_app().await;
        let book = app.library.first().cloned().expect("seeded book");
        app.open_book(book).await.expect("open");

        app.book_tab = BookTab::Info;
        app.in_section = true;
        app.handle(Action::Links).await.expect("L on Info");
        assert!(app.links.is_none());
        assert!(app.status.as_deref().is_some_and(|s| s.contains("Notes")));

        app.status = None;
        app.book_tab = BookTab::Notes;
        app.in_section = false;
        app.handle(Action::Links).await.expect("L on the menu");
        assert!(app.links.is_none());
        assert!(app.status.is_some());
    }

    #[tokio::test]
    async fn a_note_with_no_links_renders_a_reason_rather_than_a_blank_box() {
        let mut app = test_app().await;
        let book = app.library.first().cloned().expect("seeded book");
        app.open_book(book).await.expect("open");
        let lonely = app
            .engine
            .create_note(NewNoteInput {
                book_id: app.view.as_ref().and_then(|v| v.book.id),
                title: Some("Alone".into()),
                body: "no links in this one".into(),
                ..NewNoteInput::default()
            })
            .await
            .expect("note");
        app.reload_view().await.expect("reload");
        let i = app
            .view
            .as_ref()
            .unwrap()
            .notes
            .iter()
            .position(|n| n.id == lonely.id)
            .expect("in the list");
        app.book_tab = BookTab::Notes;
        app.in_section = true;
        app.tab_state.select(Some(i));
        app.handle(Action::Links).await.expect("open links");

        assert!(app.links.as_ref().expect("pane").rows.is_empty());
        assert!(
            screen_text(&mut app, 110, 32).contains("wikilink"),
            "the empty pane must explain what makes an edge"
        );
    }

    #[tokio::test]
    async fn the_rendered_pane_distinguishes_the_directions_and_the_dangling_row() {
        let mut app = test_app().await;
        let book = app.library.first().cloned().expect("seeded book");
        app.open_book(book).await.expect("open");
        open_graph_pane(&mut app).await;

        let text = screen_text(&mut app, 110, 32);
        assert!(text.contains("→ Symphony"), "{text}");
        assert!(text.contains("→ Nowhere"), "{text}");
        assert!(text.contains("(no note yet)"), "{text}");
        assert!(text.contains("← Doctor Eleven"), "{text}");
        assert!(text.contains("2 out · 1 in"), "{text}");
        assert!(text.contains("Links"), "{text}");
    }

    fn screen_text(app: &mut App, w: u16, h: u16) -> String {
        let mut t = ratatui::Terminal::new(TestBackend::new(w, h)).expect("terminal");
        t.draw(|f| ui::draw(f, app)).expect("draw");
        let buf = t.backend().buffer();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    async fn highlights_text(app: &mut App, book: Book) -> String {
        app.open_book(book).await.expect("open");
        app.book_tab = BookTab::Highlights;
        app.in_section = true;
        app.clamp_tab_selection();
        screen_text(app, 100, 24)
    }

    /// The drawn row carrying `needle`, list chrome stripped; `contains` cannot tell numbered rows.
    fn row_body<'a>(text: &'a str, needle: &str) -> &'a str {
        text.lines()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("no row for {needle}:\n{text}"))
            .split('│')
            .find(|seg| seg.contains(needle))
            .expect("the pane the needle was found in")
            .trim_start_matches([' ', '›', '‹'])
            .trim_end()
    }

    async fn mark(app: &App, book_id: i64, text: &str, when: &str) {
        app.engine
            .storage()
            .insert_highlight(
                book_id,
                &bookmole::storage::NewHighlight {
                    text: text.into(),
                    chapter: None,
                    page: None,
                    pos0: Some(format!("/body/{text}")),
                    pos1: None,
                    ko_datetime: Some(when.into()),
                    ko_datetime_updated: None,
                    color: None,
                    note: None,
                    source: "koreader".into(),
                },
            )
            .await
            .expect("highlight");
    }

    #[tokio::test]
    async fn one_reading_puts_no_read_number_on_a_highlight() {
        let mut app = test_app().await;
        let book = app.library.first().cloned().expect("seeded book");
        let text = highlights_text(&mut app, book).await;
        assert!(
            !app.view.as_ref().unwrap().shows_read_gutter(),
            "one reading has nothing to tell apart"
        );
        assert!(
            row_body(&text, "p.58").starts_with("p.58"),
            "the row is the highlight and nothing in front of it:\n{text}"
        );
    }

    #[tokio::test]
    async fn a_reread_marks_each_highlight_with_the_read_it_came_from() {
        let mut app = test_app().await;
        // Own book: the fixture's reading is opened at the machine's "now", so its dates are not fixed.
        let book = app
            .engine
            .save_book(&Book {
                title: Some("Middlemarch".into()),
                page_count: Some(490),
                ..Book::default()
            })
            .await
            .expect("save");
        let id = book.id.expect("id");

        let s = app.engine.storage();
        s.record_reading(
            id,
            Some(1_577_836_800),
            Some(1_580_515_200),
            "finished",
            "manual",
        )
        .await
        .expect("first reading");
        s.record_reading(id, Some(1_798_761_600), None, "reading", "manual")
            .await
            .expect("second reading");
        // Short texts: the section pane shares width with the book object and clips long ones.
        mark(&app, id, "alpha", "2020-01-15 10:00:00").await;
        mark(&app, id, "gamma", "2027-02-01 10:00:00").await;
        mark(&app, id, "beta", "2023-06-01 10:00:00").await;
        s.attribute_highlights(id).await.expect("attribute");

        app.refresh_library().await.expect("refresh");
        let text = highlights_text(&mut app, book).await;
        assert!(
            app.view.as_ref().unwrap().shows_read_gutter(),
            "two readings is exactly when the column earns its cell"
        );
        assert_eq!(
            row_body(&text, "alpha"),
            "1 alpha",
            "the first read's highlight is numbered 1:\n{text}"
        );
        assert_eq!(
            row_body(&text, "gamma"),
            "2 gamma",
            "the second read's highlight is numbered 2:\n{text}"
        );
        assert_eq!(
            row_body(&text, "beta"),
            "· beta",
            "an unplaceable highlight keeps a gutter cell rather than going \
             blank — the dot is the answer, not a missing one:\n{text}"
        );
    }

    #[tokio::test]
    async fn discarding_a_written_draft_asks_first() {
        let mut app = test_app().await;
        let book = app.library.first().cloned().expect("seeded book");
        app.open_book(book).await.expect("open");
        let before = app.view.as_ref().unwrap().notes.len();

        app.handle(Action::NewNote).await.expect("open editor");
        for c in "half a thought".chars() {
            app.on_editor_key(KeyEvent::from(KeyCode::Char(c)))
                .await
                .expect("type");
        }
        app.on_editor_key(KeyEvent::from(KeyCode::Esc))
            .await
            .expect("esc");
        assert!(app.note_editor.is_none());
        assert!(matches!(app.confirm, Some(Confirm::DiscardDraft)));

        app.resolve_confirm(false).await.expect("keep editing");
        assert!(app.confirm.is_none());
        let draft = app.note_editor.as_ref().expect("editor restored");
        assert_eq!(draft.editor.text(), "half a thought");

        app.on_editor_key(KeyEvent::from(KeyCode::Esc))
            .await
            .expect("esc again");
        app.resolve_confirm(true).await.expect("discard");
        assert!(app.note_editor.is_none());
        assert!(app.pending_discard.is_none());
        assert_eq!(app.view.as_ref().unwrap().notes.len(), before);
    }

    #[tokio::test]
    async fn discarding_a_blank_draft_needs_no_confirmation() {
        let mut app = test_app().await;
        let book = app.library.first().cloned().expect("seeded book");
        app.open_book(book).await.expect("open");

        app.handle(Action::NewNote).await.expect("open editor");
        app.on_editor_key(KeyEvent::from(KeyCode::Esc))
            .await
            .expect("esc");
        assert!(app.note_editor.is_none());
        assert!(app.confirm.is_none());
        assert!(app.pending_discard.is_none());
    }

    #[tokio::test]
    async fn removing_a_book_needs_confirmation() {
        let mut app = test_app().await;
        app.screen = Screen::Library;
        app.library_state.select(Some(0));
        assert_eq!(app.library.len(), 1);

        app.handle(Action::Delete).await.expect("ask");
        assert!(app.confirm.is_some());
        app.resolve_confirm(false).await.expect("keep");
        assert_eq!(app.library.len(), 1);

        app.handle(Action::Delete).await.expect("ask again");
        app.resolve_confirm(true).await.expect("remove");
        assert!(app.library.is_empty());
    }

    #[tokio::test]
    async fn spin_can_be_paused_and_resumed() {
        let mut app = test_app().await;
        let book = app.library.first().cloned().expect("seeded book");
        app.open_book(book).await.expect("open");
        assert!(app.spinning);

        app.handle(Action::ToggleSpin).await.expect("pause");
        assert!(!app.spinning);
        let held = app.params.pose.yaw;
        assert!(!app.tick(), "a paused book doesn't move");
        assert_eq!(app.params.pose.yaw, held);

        app.handle(Action::ToggleSpin).await.expect("resume");
        assert!(app.spinning);
        assert!(app.tick(), "a resumed book moves");
        assert!(
            (app.params.pose.yaw - held).abs() < 0.05,
            "resumes smoothly"
        );
    }

    #[tokio::test]
    async fn the_spin_goes_all_the_way_round() {
        let mut app = test_app().await;
        let book = app.library.first().cloned().expect("seeded book");
        app.open_book(book).await.expect("open");

        let mut seen_back = false;
        let mut seen_front = false;
        for _ in 0..2000 {
            app.tick();
            let yaw = app.params.pose.yaw.abs();
            seen_back |= yaw > 2.4;
            seen_front |= yaw < 0.2;
            assert!(yaw <= std::f32::consts::PI + 1e-3, "yaw escaped: {yaw}");
        }
        assert!(seen_back && seen_front, "never completed a turn");
    }

    #[tokio::test]
    async fn the_home_shelf_is_one_row_off_the_menu() {
        let mut app = home_app().await;
        assert_eq!(app.reading.len(), 1, "the seeded book has an open reading");
        assert_eq!(app.reading_state.selected(), Some(0));

        let text = screen_text(&mut app, 80, 20);
        assert!(text.contains("Station Eleven"), "{text}");
        assert!(text.contains("36%"), "{text}");

        app.handle(Action::Back).await.expect("back");
        assert_eq!(app.screen, Screen::Menu);
        assert!(!app.quit, "esc off the shelf killed the app");

        app.menu_index = menu_row(MenuItem::Home);
        app.handle(Action::Select).await.expect("home");
        app.handle(Action::Menu).await.expect("menu");
        assert_eq!(app.screen, Screen::Menu);
        app.handle(Action::Back).await.expect("back");
        assert_eq!(app.screen, Screen::Menu);
        assert!(!app.quit, "esc on the menu killed the app");
    }

    #[tokio::test]
    async fn the_home_screen_greets_you_with_no_numbers() {
        let mut app = home_app().await;
        app.status = None;
        let text = screen_text(&mut app, 80, 20);
        for line in text.lines() {
            let digits: String = line.chars().filter(|c| c.is_ascii_digit()).collect();
            assert!(
                digits.is_empty() || line.contains("Station Eleven"),
                "a number that is not a book's own progress: {line}"
            );
        }
    }

    #[tokio::test]
    async fn the_empty_shelf_offers_a_way_onward() {
        let mut app = home_app().await;
        let id = app.library[0].id.expect("id");
        app.engine
            .update_progress(id, None, Some(true))
            .await
            .expect("finish");
        app.refresh_reading().await.expect("refresh");
        assert!(app.reading.is_empty());

        let text = screen_text(&mut app, 80, 20);
        assert!(text.contains("nothing open"), "{text}");
        assert!(text.contains("find"), "no way onward: {text}");
        assert!(text.contains("menu"), "no way onward: {text}");

        app.handle(Action::Query).await.expect("find");
        assert_eq!(app.screen, Screen::Home);
        assert_eq!(
            app.input.as_ref().map(|i| i.context),
            Some(InputContext::LibraryFind),
            "/ opened no find box"
        );
    }

    async fn seed_second_book(app: &App) {
        app.engine
            .save_book(&Book {
                title: Some("Middlemarch".into()),
                authors: vec!["George Eliot".into()],
                isbn_13: Some("9781900000178".into()),
                ..Book::default()
            })
            .await
            .expect("save");
    }

    #[tokio::test]
    async fn a_find_on_the_home_shelf_looks_in_the_library_first() {
        let mut app = test_app().await;
        seed_second_book(&app).await;
        app.refresh_library().await.expect("refresh");
        assert_eq!(app.library.len(), 2);

        app.handle(Action::Query).await.expect("find");
        app.commit_input(InputContext::LibraryFind, "middlemarch".into())
            .await
            .expect("commit");

        assert_eq!(app.screen, Screen::Library);
        assert_eq!(app.library_filter.as_deref(), Some("middlemarch"));
        assert_eq!(app.library.len(), 1);
        assert_eq!(app.library[0].display_title(), "Middlemarch");
        assert_eq!(app.library_state.selected(), Some(0));
        assert!(app.search_results.is_empty(), "a provider was asked");
    }

    #[tokio::test]
    async fn a_find_that_matches_nothing_asks_before_going_online() {
        let mut app = home_app().await;
        app.refresh_library().await.expect("refresh");
        let all = app.library.len();

        app.commit_input(InputContext::LibraryFind, "gravity's rainbow".into())
            .await
            .expect("commit");

        assert_eq!(app.screen, Screen::Home);
        assert!(
            app.library_filter.is_none(),
            "a filter matching nothing stuck"
        );
        assert_eq!(app.library.len(), all);
        match &app.confirm {
            Some(Confirm::SearchOnline(q)) => assert_eq!(q, "gravity's rainbow"),
            other => panic!("expected the online-search question, got {other:?}"),
        }

        app.resolve_confirm(false).await.expect("decline");
        assert_eq!(app.screen, Screen::Home);
        assert!(app.search_results.is_empty());
        assert!(
            app.status.as_deref().is_some_and(|s| s.contains("library")),
            "the answer should say where it looked: {:?}",
            app.status
        );
    }

    #[tokio::test]
    async fn esc_widens_a_narrowed_library_before_it_leaves() {
        let mut app = test_app().await;
        seed_second_book(&app).await;
        app.refresh_library().await.expect("refresh");
        let all = app.library.len();

        app.commit_input(InputContext::LibraryFind, "station".into())
            .await
            .expect("commit");
        assert_eq!(app.library.len(), 1);

        app.handle(Action::Back).await.expect("widen");
        assert_eq!(app.screen, Screen::Library, "esc left before it widened");
        assert!(app.library_filter.is_none());
        assert_eq!(app.library.len(), all);

        app.handle(Action::Back).await.expect("leave");
        assert_eq!(app.screen, Screen::Menu);
    }

    #[tokio::test]
    async fn a_refresh_does_not_widen_a_narrowed_library() {
        let mut app = test_app().await;
        seed_second_book(&app).await;
        app.commit_input(InputContext::LibraryFind, "station".into())
            .await
            .expect("commit");
        assert_eq!(app.library.len(), 1);

        app.handle(Action::Refresh).await.expect("refresh");
        assert_eq!(app.library.len(), 1);

        app.screen = Screen::Menu;
        app.menu_index = MENU
            .iter()
            .position(|m| m.0 == MenuItem::Library)
            .expect("item");
        app.handle(Action::Select).await.expect("open library");
        assert!(app.library_filter.is_none());
        assert_eq!(app.library.len(), 2);
    }

    #[test]
    fn a_find_matches_title_and_author_in_any_case() {
        let b = Book {
            title: Some("Kafka on the Shore".into()),
            authors: vec!["Haruki Murakami".into()],
            ..Book::default()
        };
        assert!(matches_book(&b, "kafka"));
        assert!(matches_book(&b, "MURAKAMI"));
        assert!(matches_book(&b, "  shore "));
        assert!(!matches_book(&b, "middlemarch"));
        assert!(matches_book(&b, "   "));
    }

    #[test]
    fn a_find_folds_case_outside_ascii() {
        let b = Book {
            title: Some("Ödipus".into()),
            ..Book::default()
        };
        assert!(matches_book(&b, "ödipus"));
    }

    #[tokio::test]
    async fn home_opens_a_book_and_its_reflection() {
        let mut app = home_app().await;
        app.handle(Action::Select).await.expect("open the book");
        assert_eq!(app.screen, Screen::Book);
        assert_eq!(
            app.view.as_ref().unwrap().book.display_title(),
            "Station Eleven"
        );

        app.handle(Action::Reflect).await.expect("reflect");
        let draft = app.note_editor.as_ref().expect("the editor opened");
        assert_eq!(draft.title(), "reflection");

        for c in "the museum of civilization".chars() {
            app.on_editor_key(KeyEvent::from(KeyCode::Char(c)))
                .await
                .expect("type");
        }
        app.on_editor_key(KeyEvent::from(KeyCode::Enter))
            .await
            .expect("save");
        assert!(app.note_editor.is_none());

        let notes = &app.view.as_ref().unwrap().notes;
        let reflection = notes
            .iter()
            .find(|n| n.kind == "reflection")
            .expect("the reflection is in the Notes list");
        assert!(reflection.reading_id.is_some(), "anchored to the reading");
        assert_eq!(
            app.engine.note_body(reflection).unwrap().trim(),
            "the museum of civilization"
        );

        app.book_tab = BookTab::Notes;
        app.in_section = true;
        app.clamp_tab_selection();
        assert!(screen_text(&mut app, 120, 40).contains('◆'));
    }

    #[tokio::test]
    async fn opening_a_reflection_twice_is_the_same_note() {
        let mut app = home_app().await;
        app.handle(Action::Reflect).await.expect("first");
        let first = match &app.note_editor.as_ref().unwrap().target {
            NoteTarget::Edit(n) => n.id,
            _ => panic!("a reflection opens as an edit of the note that exists"),
        };
        app.note_editor = None;

        app.handle(Action::Reflect).await.expect("second");
        let second = match &app.note_editor.as_ref().unwrap().target {
            NoteTarget::Edit(n) => n.id,
            _ => panic!("edit"),
        };
        assert_eq!(first, second, "a second press started a second reflection");

        let id = app.library[0].id.expect("id");
        let notes = app
            .engine
            .list_notes(NoteScope::Book(id), None)
            .await
            .expect("notes");
        assert_eq!(notes.iter().filter(|n| n.kind == "reflection").count(), 1);
    }

    #[tokio::test]
    async fn a_review_asks_for_its_rating_and_never_rounds_one() {
        let mut app = home_app().await;
        app.handle(Action::Review).await.expect("review");
        assert_eq!(app.note_editor.as_ref().unwrap().title(), "review");
        for c in "worth your time".chars() {
            app.on_editor_key(KeyEvent::from(KeyCode::Char(c)))
                .await
                .expect("type");
        }
        app.on_editor_key(KeyEvent::from(KeyCode::Enter))
            .await
            .expect("save");
        assert_eq!(
            app.input.as_ref().map(|i| i.context),
            Some(InputContext::ReviewRating),
            "saving a review did not ask for its rating"
        );

        for c in "4.3".chars() {
            app.on_input_key(KeyEvent::from(KeyCode::Char(c)))
                .await
                .expect("type");
        }
        app.on_input_key(KeyEvent::from(KeyCode::Enter))
            .await
            .expect("commit");
        let status = app.status.clone().unwrap_or_default();
        assert!(
            status.contains("4.3") && status.contains("scale"),
            "{status}"
        );

        let note_id = app
            .engine
            .list_notes(NoteScope::Book(app.library[0].id.unwrap()), None)
            .await
            .expect("notes")
            .into_iter()
            .find(|n| n.kind == "review")
            .expect("the review")
            .id;
        app.pending_rating = Some(note_id);
        app.commit_rating("4.25".into()).await.expect("rate");
        assert_eq!(app.status.as_deref(), Some("rated 4.25 · goodreads 4.25"));

        app.pending_rating = Some(note_id);
        app.commit_rating("4".into()).await.expect("rate");
        assert_eq!(app.status.as_deref(), Some("rated 4 · goodreads 4"));

        app.engine
            .put_rating_scale("tens", 0.0, 10.0, 1.0)
            .await
            .expect("scale");
        app.pending_rating = Some(note_id);
        app.commit_rating("7".into()).await.expect("rate");
        let status = app.status.clone().unwrap_or_default();
        assert!(status.contains("rated 7"), "{status}");
        assert!(status.contains("no goodreads mapping"), "{status}");
    }

    #[tokio::test]
    async fn finishing_a_book_takes_it_off_the_shelf() {
        let mut app = home_app().await;
        app.handle(Action::Select).await.expect("open");
        app.handle(Action::ToggleFinished).await.expect("finish");
        assert!(app.reading.is_empty(), "a finished book is still 'reading'");
        app.handle(Action::ToggleFinished).await.expect("unfinish");
        assert_eq!(app.reading.len(), 1, "unfinishing reopened nothing");
    }

    #[tokio::test]
    async fn the_home_screen_navigates_and_reaches_the_menu() {
        let mut app = home_app().await;
        app.handle(Action::Up).await.expect("up");
        assert_eq!(app.reading_state.selected(), Some(0), "one row wraps to it");
        app.handle(Action::Menu).await.expect("menu");
        assert_eq!(app.screen, Screen::Menu);
    }

    #[tokio::test]
    async fn a_scan_seeds_one_row_per_book_in_the_state_it_deserves() {
        let app = test_app().await;
        assert_eq!(app.device.len(), 3, "one row per sidecar");
        assert!(app.device_root.is_some());
        assert_eq!(app.device_state.selected(), Some(0));

        let by_title = |t: &str| {
            app.device
                .iter()
                .find(|r| r.book.display_title() == t)
                .unwrap_or_else(|| panic!("no row for {t}"))
        };
        assert!(matches!(
            by_title("Station Eleven").book.state,
            DeviceState::Updated {
                new_highlights: 1,
                ..
            }
        ));
        assert!(matches!(
            by_title("Piranesi").book.state,
            DeviceState::New { .. }
        ));
        assert!(matches!(
            by_title("Broken").book.state,
            DeviceState::Unreadable(_)
        ));
    }

    #[tokio::test]
    async fn a_sync_drains_one_book_per_iteration() {
        let mut app = test_app().await;
        app.screen = Screen::Device;

        let syncable: Vec<usize> = app
            .device
            .iter()
            .enumerate()
            .filter(|(_, r)| r.book.state.is_syncable())
            .map(|(i, _)| i)
            .collect();
        for i in syncable {
            app.device_state.select(Some(i));
            app.toggle_mark();
        }
        assert_eq!(app.device_marks.len(), 2);

        app.handle(Action::Sync).await.expect("sync");
        assert_eq!(
            app.pending_pull.as_ref().map(|q| q.len()),
            Some(2),
            "the whole selection is queued, not run"
        );
        assert!(app.has_deferred(), "the loop must not block on select!");

        assert!(app.pump_deferred().await.expect("pump"));
        assert_eq!(
            app.pending_pull.as_ref().map(|q| q.len()),
            Some(1),
            "a sync-all ran more than one book before redrawing"
        );
        assert!(app.pump_deferred().await.expect("pump"));
        assert!(
            app.pending_pull.is_none(),
            "the queue drops when it empties"
        );
        assert!(!app.pump_deferred().await.expect("pump"), "nothing left");

        for row in &app.device {
            if row.book.state == DeviceState::Unchanged && row.note.is_some() {
                assert!(row.note.as_deref().unwrap().contains("new"));
            }
        }
        assert!(app.device_marks.is_empty());
        assert_eq!(app.library.len(), 2);
    }

    #[tokio::test]
    async fn an_unmarked_sync_takes_every_syncable_row() {
        let mut app = test_app().await;
        app.screen = Screen::Device;
        app.handle(Action::Sync).await.expect("sync");
        let queued = app.pending_pull.as_ref().expect("queued");
        assert_eq!(queued.len(), 2);
        assert!(
            !queued
                .iter()
                .any(|p| p.to_string_lossy().contains("Broken")),
            "an unreadable sidecar was queued for a sync that would error on it"
        );
    }

    #[tokio::test]
    async fn enter_pulls_one_row_and_refuses_an_unreadable_one() {
        let mut app = test_app().await;
        app.screen = Screen::Device;
        let broken = app
            .device
            .iter()
            .position(|r| matches!(r.book.state, DeviceState::Unreadable(_)))
            .expect("a broken row");
        app.device_state.select(Some(broken));
        app.handle(Action::Select).await.expect("enter");
        assert!(
            app.pending_pull.is_none(),
            "queued a sidecar it cannot read"
        );
        assert!(app.status.as_deref().unwrap().contains("can't read"));

        let piranesi = app
            .device
            .iter()
            .position(|r| r.book.display_title() == "Piranesi")
            .expect("piranesi");
        app.device_state.select(Some(piranesi));
        app.handle(Action::Select).await.expect("enter");
        assert_eq!(app.pending_pull.as_ref().map(|q| q.len()), Some(1));

        app.pump_deferred().await.expect("pull");
        assert!(app.library.iter().any(|b| b.display_title() == "Piranesi"));
        assert!(
            app.device[piranesi].note.is_some(),
            "the row says what it did"
        );
    }

    #[tokio::test]
    async fn linking_offers_candidates_and_then_imports() {
        let mut app = test_app().await;
        app.screen = Screen::Device;
        let piranesi = app
            .device
            .iter()
            .position(|r| r.book.display_title() == "Piranesi")
            .expect("piranesi");
        app.device_state.select(Some(piranesi));

        app.handle(Action::Link).await.expect("link");
        assert!(app.link_picker.is_none());
        assert!(
            app.status
                .as_deref()
                .unwrap()
                .contains("nothing in the library")
        );

        let book_id = app.library[0].id.expect("seeded book id");
        app.device[piranesi].book.state = DeviceState::New {
            candidates: vec![MatchCandidate {
                book_id,
                title: "Station Eleven".into(),
                authors_display: vec!["Emily St. John Mandel".into()],
                publish_year: Some(2014),
                score: 0.72,
            }],
        };
        app.handle(Action::Link).await.expect("link");
        assert!(app.link_picker.is_some(), "the chooser opened");

        app.handle(Action::Select).await.expect("commit link");
        assert!(app.link_picker.is_none());
        assert_eq!(app.device[piranesi].book.book_id, Some(book_id));
        assert_eq!(
            app.pending_pull.as_ref().map(|q| q.len()),
            Some(1),
            "linking queued no import, so it would look like a key that did nothing"
        );

        app.pump_deferred().await.expect("import");
        assert_eq!(app.library.len(), 1, "linking created a second book");
    }

    #[tokio::test]
    async fn the_candidate_chooser_can_be_left_alone() {
        let mut app = test_app().await;
        app.screen = Screen::Device;
        app.link_picker = Some(LinkPicker {
            target: LinkTarget::Sidecar(app.device[0].book.path.clone()),
            title: "Piranesi".into(),
            candidates: vec![MatchCandidate {
                book_id: 1,
                title: "Something".into(),
                authors_display: Vec::new(),
                publish_year: None,
                score: 0.7,
            }],
            state: ListState::default(),
        });
        app.ambient = crate::ambient::Ambient::new(crate::ambient::Motif::Motes);
        assert!(app.ambient_visible(), "the layer should still be drawn");
        assert!((0..40).all(|_| !app.tick()));

        app.handle(Action::Back).await.expect("esc");
        assert!(app.link_picker.is_none());
        assert!(app.pending_pull.is_none());
        assert_eq!(app.screen, Screen::Device, "esc left the screen entirely");
    }

    #[tokio::test]
    async fn rescanning_defers_the_walk_and_drops_the_marks() {
        let mut app = test_app().await;
        app.screen = Screen::Device;
        app.device_marks.insert(1);

        app.handle(Action::Rescan).await.expect("rescan");
        assert!(
            app.pending_scan.is_some(),
            "the walk ran in the key handler"
        );
        assert!(app.status.as_deref().unwrap().contains("scanning"));

        app.pump_deferred().await.expect("scan");
        assert!(app.pending_scan.is_none());
        assert_eq!(app.device.len(), 3);
        assert!(app.device_marks.is_empty(), "stale marks survived a rescan");
    }

    #[tokio::test]
    async fn a_reader_arriving_refreshes_the_device_screen_it_is_looking_at() {
        let mut app = test_app().await;
        let root = app.device_root.clone().expect("a root");
        app.screen = Screen::Device;
        app.pending_scan = None;

        app.on_mount_event(MountEvent::Arrived(root.clone()));
        assert_eq!(app.pending_scan.as_deref(), Some(root.as_path()));
        assert!(app.dirty);
    }

    #[tokio::test]
    async fn a_reader_arriving_never_brings_anything_across_by_itself() {
        let mut app = test_app().await;
        let root = app.device_root.clone().expect("a root");
        let before = app.library.len();
        app.screen = Screen::Device;

        app.on_mount_event(MountEvent::Arrived(root));
        assert!(app.pending_pull.is_none(), "a mount queued a sync");
        while app.pump_deferred().await.expect("pump") {}
        assert!(app.pending_pull.is_none(), "the scan went on to sync");
        assert_eq!(app.library.len(), before);
    }

    #[tokio::test]
    async fn a_reader_arriving_elsewhere_only_says_so() {
        let mut app = test_app().await;
        let root = app.device_root.clone().expect("a root");
        app.screen = Screen::Home;
        app.pending_scan = None;

        app.on_mount_event(MountEvent::Arrived(root.clone()));
        assert_eq!(app.screen, Screen::Home, "the screen was taken over");
        assert!(app.pending_scan.is_none(), "a scan ran unasked");
        let status = app.status.clone().expect("a status line");
        assert!(status.contains(&root.display().to_string()));
        assert!(
            status.contains('m'),
            "{status:?} names no way to the device"
        );
    }

    #[tokio::test]
    async fn an_arriving_reader_answers_the_path_prompt() {
        let mut app = test_app().await;
        let root = app.device_root.clone().expect("a root");
        app.screen = Screen::Device;
        app.device_root = None;
        app.pending_scan = None;
        app.start_input(InputContext::DevicePath, "device path", "");

        app.on_mount_event(MountEvent::Arrived(root.clone()));
        assert!(app.input.is_none(), "the prompt outlived its own answer");
        assert_eq!(app.pending_scan.as_deref(), Some(root.as_path()));
    }

    #[tokio::test]
    async fn a_second_reader_does_not_take_the_screen_from_the_first() {
        let mut app = test_app().await;
        app.screen = Screen::Device;
        app.pending_scan = None;
        app.device_marks.insert(1);

        app.on_mount_event(MountEvent::Arrived(PathBuf::from("/Volumes/OtherReader")));
        assert!(app.pending_scan.is_none(), "the other reader took over");
        assert_eq!(app.device.len(), 3, "the shown list was replaced");
        assert!(app.device_marks.contains(&1), "the marks were dropped");
        assert!(app.status.as_deref().unwrap().contains("OtherReader"));
    }

    #[tokio::test]
    async fn unplugging_the_shown_reader_empties_the_shelf_but_keeps_the_root() {
        let mut app = test_app().await;
        let root = app.device_root.clone().expect("a root");
        app.screen = Screen::Device;
        app.device_marks.insert(0);

        app.on_mount_event(MountEvent::Departed(root.clone()));
        assert!(app.device.is_empty());
        assert!(app.device_marks.is_empty());
        assert_eq!(app.device_state.selected(), None);
        assert_eq!(app.device_root.as_deref(), Some(root.as_path()));
        assert!(app.status.as_deref().unwrap().contains("unplugged"));
    }

    #[tokio::test]
    async fn unplugging_a_different_reader_leaves_the_shelf_alone() {
        let mut app = test_app().await;
        app.screen = Screen::Device;

        app.on_mount_event(MountEvent::Departed(PathBuf::from("/Volumes/OtherReader")));
        assert_eq!(app.device.len(), 3);
    }

    #[tokio::test]
    async fn marking_toggles_the_selected_row() {
        let mut app = test_app().await;
        app.screen = Screen::Device;
        app.device_state.select(Some(2));
        app.handle(Action::Mark).await.expect("mark");
        assert_eq!(
            app.device_marks.iter().copied().collect::<Vec<_>>(),
            vec![2]
        );
        app.handle(Action::Mark).await.expect("unmark");
        assert!(app.device_marks.is_empty());
    }

    #[tokio::test]
    async fn the_device_screen_navigates_and_returns_to_the_menu() {
        let mut app = test_app().await;
        app.screen = Screen::Device;
        app.handle(Action::Up).await.expect("up");
        assert_eq!(app.device_state.selected(), Some(2), "wraps backwards");
        app.handle(Action::Down).await.expect("down");
        assert_eq!(app.device_state.selected(), Some(0));
        app.handle(Action::Menu).await.expect("menu");
        assert_eq!(app.screen, Screen::Menu);
    }

    #[tokio::test]
    async fn the_menu_opens_the_device_screen() {
        let mut app = test_app().await;
        app.screen = Screen::Menu;
        app.menu_index = menu_row(MenuItem::Device);
        app.handle(Action::Select).await.expect("open device");
        assert_eq!(app.screen, Screen::Device);
        assert!(app.pending_scan.is_some() || app.input.is_some());
    }

    #[tokio::test]
    async fn a_typed_path_is_scanned() {
        let mut app = test_app().await;
        let root = app.device_root.clone().expect("a root");
        app.device.clear();
        app.screen = Screen::Menu;

        app.start_input(InputContext::DevicePath, "device path", "");
        for c in root.to_string_lossy().chars() {
            app.on_input_key(KeyEvent::from(KeyCode::Char(c)))
                .await
                .expect("type");
        }
        app.on_input_key(KeyEvent::from(KeyCode::Enter))
            .await
            .expect("commit");
        assert_eq!(app.screen, Screen::Device);
        app.pump_deferred().await.expect("scan");
        assert_eq!(app.device.len(), 3);
    }

    #[tokio::test]
    async fn quitting_and_navigation_move_between_screens() {
        let mut app = test_app().await;
        assert_eq!(app.screen, Screen::Menu);
        app.menu_index = menu_row(MenuItem::Library);
        app.handle(Action::Select).await.expect("open library");
        assert_eq!(app.screen, Screen::Library);
        app.handle(Action::Select).await.expect("open book");
        assert_eq!(app.screen, Screen::Book);
        app.handle(Action::Back).await.expect("back");
        assert_eq!(app.screen, Screen::Library);
        app.handle(Action::Menu).await.expect("menu");
        assert_eq!(app.screen, Screen::Menu);
        app.handle(Action::Quit).await.expect("quit");
        assert!(app.quit);
    }

    /// Every screen, for the sweeps that must cover all of them; the length is written out on purpose.
    const ALL_SCREENS: [Screen; 9] = [
        Screen::Home,
        Screen::Menu,
        Screen::Library,
        Screen::Book,
        Screen::Search,
        Screen::Settings,
        Screen::Device,
        Screen::Calibre,
        Screen::Goodreads,
    ];

    #[tokio::test]
    async fn the_app_opens_on_the_menu_with_nothing_behind_it() {
        let app = test_app().await;
        assert_eq!(app.screen, Screen::Menu);
        assert!(
            app.trail.is_empty(),
            "the front door has a trail: {:?}",
            app.trail
        );
    }

    #[tokio::test]
    async fn back_walks_the_trail_one_screen_at_a_time_and_stops_at_the_menu() {
        let mut app = test_app().await;
        app.menu_index = menu_row(MenuItem::Library);
        app.handle(Action::Select).await.expect("library");
        app.handle(Action::Select).await.expect("book");
        assert_eq!(app.screen, Screen::Book);
        assert_eq!(app.trail, vec![Screen::Menu, Screen::Library]);

        app.handle(Action::Back).await.expect("back");
        assert_eq!(app.screen, Screen::Library);
        app.handle(Action::Back).await.expect("back");
        assert_eq!(app.screen, Screen::Menu);
        assert!(app.trail.is_empty());

        app.handle(Action::Back).await.expect("back");
        assert_eq!(app.screen, Screen::Menu, "back walked past the menu");
        assert!(!app.quit, "back off the bottom of the trail quit the app");
    }

    #[tokio::test]
    async fn back_never_quits_from_any_screen() {
        for screen in ALL_SCREENS {
            let mut app = test_app().await;
            app.screen = screen;
            app.handle(Action::Back).await.expect("back");
            assert!(!app.quit, "back quit the app from {screen:?}");
            app.handle(Action::Back).await.expect("back again");
            assert!(!app.quit, "a second back quit the app from {screen:?}");
        }
    }

    #[tokio::test]
    async fn the_menu_is_always_the_bottom_of_the_trail() {
        let mut app = test_app().await;
        app.menu_index = menu_row(MenuItem::Library);
        app.handle(Action::Select).await.expect("library");
        app.handle(Action::Select).await.expect("book");
        app.open_search();
        app.input = None;
        assert_eq!(
            app.trail,
            vec![Screen::Menu, Screen::Library, Screen::Book],
            "three screens deep"
        );

        app.handle(Action::Menu).await.expect("menu");
        assert_eq!(app.screen, Screen::Menu);
        assert!(
            app.trail.is_empty(),
            "the menu left a trail: {:?}",
            app.trail
        );
    }

    #[tokio::test]
    async fn the_trail_is_capped_and_keeps_the_menu_underneath() {
        let mut app = test_app().await;
        for _ in 0..TRAIL_MAX + 8 {
            app.go(Screen::Library);
            app.go(Screen::Search);
        }
        assert!(
            app.trail.len() <= TRAIL_MAX,
            "the trail grew past its cap: {}",
            app.trail.len()
        );
        assert_eq!(app.trail[0], Screen::Menu, "the cap ate the bottom");

        let depth = app.trail.len();
        for _ in 0..depth {
            app.handle(Action::Back).await.expect("back");
        }
        assert_eq!(app.screen, Screen::Menu);
        assert!(!app.quit);
    }

    #[tokio::test]
    async fn a_book_opened_from_the_home_shelf_goes_back_to_it() {
        let mut app = home_app().await;
        app.handle(Action::Select).await.expect("open the book");
        assert_eq!(app.screen, Screen::Book);

        app.handle(Action::Back).await.expect("leave the book");
        assert_eq!(
            app.screen,
            Screen::Home,
            "leaving the book landed somewhere it was not opened from"
        );
    }

    /// `cargo test -p bookmole-tui -- --ignored --nocapture print_help`
    #[test]
    #[ignore = "development aid: prints the help pages"]
    fn print_help() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut app = rt.block_on(test_app());
        app.status = None;
        app.help = true;

        let (w, h) = (88u16, 34u16);
        for screen in [
            Screen::Menu,
            Screen::Home,
            Screen::Library,
            Screen::Book,
            Screen::Search,
            Screen::Settings,
            Screen::Device,
            Screen::Calibre,
            Screen::Goodreads,
        ] {
            app.screen = screen;
            let mut t = ratatui::Terminal::new(TestBackend::new(w, h)).unwrap();
            t.draw(|f| ui::draw(f, &mut app)).unwrap();
            println!("=== {screen:?} {w}x{h} ===");
            let buf = t.backend().buffer();
            for y in 0..h {
                let text: String = (0..w)
                    .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                    .collect();
                println!("{}", text.trim_end());
            }
            println!();
        }
    }

    /// `cargo test -p bookmole-tui -- --ignored --nocapture print_lists`
    #[test]
    #[ignore = "development aid: prints the list screens"]
    fn print_lists() {
        use ratatui::style::Modifier;

        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut app = rt.block_on(test_app());

        for extra in ["Piranesi", "The Overstory", "Klara and the Sun"] {
            app.library.push(Book {
                title: Some(extra.into()),
                authors: vec!["Someone".into()],
                publish_year: Some(2020),
                ..Book::default()
            });
        }
        app.library_state.select(Some(0));
        for (book, percent) in [("Piranesi", Some(0.42)), ("The Overstory", None)] {
            app.reading.push((
                Book {
                    title: Some(book.into()),
                    authors: vec!["Someone".into()],
                    page_count: Some(300),
                    ..Book::default()
                },
                sample_reading(percent),
            ));
        }
        app.reading_state.select(Some(0));
        app.search_results = app
            .library
            .iter()
            .map(|b| bookmole::RankedResult {
                book: b.clone(),
                sources: Vec::new(),
                score: 1.0,
                shelf_id: b.id,
            })
            .collect();
        app.search_state.select(Some(0));

        app.device_marks.insert(1);
        app.calibre = sample_calibre_rows();
        app.calibre_state.select(Some(0));
        app.calibre_marks.insert(1);
        app.goodreads = Some(sample_goodreads_preview());

        let (w, h) = (96u16, 16u16);
        for (screen, linking, empty) in [
            (Screen::Home, false, false),
            (Screen::Home, false, true),
            (Screen::Library, false, false),
            (Screen::Search, false, false),
            (Screen::Device, false, false),
            (Screen::Device, true, false),
            (Screen::Calibre, false, false),
            (Screen::Goodreads, false, false),
        ] {
            app.screen = screen;
            let held = empty.then(|| std::mem::take(&mut app.reading));
            if linking {
                let mut state = ListState::default();
                state.select(Some(1));
                app.link_picker = Some(LinkPicker {
                    target: LinkTarget::Sidecar(app.device[1].book.path.clone()),
                    title: app.device[1].book.display_title(),
                    candidates: vec![
                        MatchCandidate {
                            book_id: 1,
                            title: "Piranesi's House".into(),
                            authors_display: vec!["Susanna Clarke".into()],
                            publish_year: Some(2020),
                            score: 0.78,
                        },
                        MatchCandidate {
                            book_id: 2,
                            title: "Piranesi (illustrated)".into(),
                            authors_display: vec!["Susanna Clarke".into()],
                            publish_year: Some(2021),
                            score: 0.64,
                        },
                    ],
                    state,
                });
            }
            let mut t = ratatui::Terminal::new(TestBackend::new(w, h)).unwrap();
            t.draw(|f| ui::draw(f, &mut app)).unwrap();
            println!(
                "=== {screen:?}{} {w}x{h} ===",
                if empty { " (empty)" } else { "" }
            );
            let buf = t.backend().buffer();
            for y in 0..h {
                let text: String = (0..w)
                    .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                    .collect();
                let mask: String = (0..w)
                    .map(|x| {
                        if buf[(x, y)].modifier.contains(Modifier::REVERSED) {
                            '#'
                        } else {
                            ' '
                        }
                    })
                    .collect();
                if mask.trim().is_empty() {
                    println!("|{text}|");
                } else {
                    println!("|{text}|  reversed: |{}|", mask.trim_end());
                }
            }
            if let Some(rows) = held {
                app.reading = rows;
            }
            // Per-iteration: a chooser left standing draws over every later screen.
            app.link_picker = None;
        }
    }

    /// `cargo test -p bookmole-tui -- --ignored --nocapture print_ambient`
    #[test]
    #[ignore = "development aid: prints the ambient layer"]
    fn print_ambient() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut app = rt.block_on(test_app());
        app.screen = Screen::Menu;
        let (w, h) = (110u16, 32u16);

        for motif in crate::ambient::Motif::ALL {
            if motif == crate::ambient::Motif::Off {
                continue; // Without a clock the frame never advances, and waiting for one hangs.
            }
            app.ambient = crate::ambient::Ambient::new(motif);
            for frame in [0u32, 12, 40] {
                while app.ambient.frame_index() < frame {
                    app.tick();
                }
                let mut t = ratatui::Terminal::new(TestBackend::new(w, h)).unwrap();
                t.draw(|f| ui::draw(f, &mut app)).unwrap();
                println!("=== {} · frame {frame} ===", motif.label());
                let buf = t.backend().buffer();
                for y in 0..h {
                    let row: String = (0..w)
                        .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                        .collect();
                    println!("|{row}|");
                }
            }
        }
    }

    #[test]
    #[ignore = "development aid: prints the composed layout"]
    fn print_layout() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut app = rt.block_on(test_app());
        let book = app.library.first().cloned().unwrap();
        rt.block_on(app.open_book(book)).unwrap();
        app.show_options = true;
        rt.block_on(app.handle(Action::Reflect)).unwrap();
        app.note_editor = None;
        for (w, h, panel, notes) in [
            (110, 32, true, false),
            (44, 26, true, false),
            (86, 30, true, false),
            (110, 32, true, true),
            (180, 44, true, false),
            (180, 44, false, false),
        ] {
            app.layout.panel = panel;
            app.book_tab = if notes { BookTab::Notes } else { BookTab::Info };
            app.in_section = notes;
            app.clamp_tab_selection();
            print_frame(
                &mut app,
                w,
                h,
                &format!(
                    "{w}x{h}{}{}",
                    if notes { " · notes" } else { "" },
                    if panel { "" } else { " · no panel" }
                ),
            );
        }

        rt.block_on(open_graph_pane(&mut app));
        app.layout.panel = true;
        for (w, h) in [(110, 32), (44, 26)] {
            print_frame(&mut app, w, h, &format!("links pane {w}x{h}"));
        }
    }

    fn print_frame(app: &mut App, w: u16, h: u16, label: &str) {
        let mut t = ratatui::Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| ui::draw(f, app)).unwrap();
        println!("=== {label} ===");
        let buf = t.backend().buffer();
        for y in 0..h {
            let row: String = (0..w)
                .map(|x| {
                    let s = buf[(x, y)].symbol();
                    match s {
                        "▀" | "▄" => '#',
                        " " => ' ',
                        _ => s.chars().next().unwrap_or(' '),
                    }
                })
                .collect();
            println!("|{row}|");
        }
    }

    #[tokio::test]
    async fn api_key_box_collects_a_key_and_defers_verification() {
        let mut app = test_app().await;
        app.screen = Screen::Settings;

        app.handle(Action::EditApiKey).await.expect("open");
        assert!(matches!(
            app.api_key.as_ref().map(|m| &m.stage),
            Some(ApiKeyStage::Entry { .. })
        ));

        for c in "AIzaKEY".chars() {
            app.on_api_key_key(KeyEvent::from(KeyCode::Char(c)));
        }
        app.on_api_key_key(KeyEvent::from(KeyCode::Backspace));
        match app.api_key.as_ref().map(|m| &m.stage) {
            Some(ApiKeyStage::Entry { value }) => assert_eq!(value, "AIzaKE"),
            _ => panic!("still in entry"),
        }

        app.on_api_key_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.pending_verify.as_deref(), Some("AIzaKE"));
        assert!(matches!(
            app.api_key.as_ref().map(|m| &m.stage),
            Some(ApiKeyStage::Verifying)
        ));
    }

    #[tokio::test]
    async fn api_key_box_cancels_and_empty_submit_is_inert() {
        let mut app = test_app().await;
        app.open_api_key();

        app.on_api_key_key(KeyEvent::from(KeyCode::Enter));
        assert!(app.pending_verify.is_none());
        assert!(app.api_key.is_some(), "box stays open");

        app.on_api_key_key(KeyEvent::from(KeyCode::Esc));
        assert!(app.api_key.is_none());
        assert!(app.pending_verify.is_none());
    }

    #[tokio::test]
    async fn api_key_failure_page_retries_or_cancels() {
        let mut app = test_app().await;

        app.api_key = Some(ApiKeyModal {
            stage: ApiKeyStage::Result {
                ok: false,
                message: "bad key".into(),
            },
        });
        app.on_api_key_key(KeyEvent::from(KeyCode::Enter));
        match app.api_key.as_ref().map(|m| &m.stage) {
            Some(ApiKeyStage::Entry { value }) => assert!(value.is_empty()),
            _ => panic!("retry reopens the entry field"),
        }

        app.api_key = Some(ApiKeyModal {
            stage: ApiKeyStage::Result {
                ok: false,
                message: "bad key".into(),
            },
        });
        app.on_api_key_key(KeyEvent::from(KeyCode::Esc));
        assert!(app.api_key.is_none());

        app.api_key = Some(ApiKeyModal {
            stage: ApiKeyStage::Result {
                ok: true,
                message: "ok".into(),
            },
        });
        app.on_api_key_key(KeyEvent::from(KeyCode::Char(' ')));
        assert!(app.api_key.is_none());
    }

    #[test]
    fn angles_wrap_into_a_single_turn() {
        let a = wrap_angle(std::f32::consts::TAU + 0.5);
        assert!((a - 0.5).abs() < 1e-4, "{a}");
        let b = wrap_angle(-std::f32::consts::TAU - 0.5);
        assert!((b + 0.5).abs() < 1e-4, "{b}");
    }

    #[tokio::test]
    async fn the_menu_opens_the_two_import_screens() {
        for (item, screen) in [
            (MenuItem::Calibre, Screen::Calibre),
            (MenuItem::Goodreads, Screen::Goodreads),
        ] {
            let mut app = test_app().await;
            app.screen = Screen::Menu;
            app.menu_index = menu_row(item);
            app.handle(Action::Select).await.expect("open");
            assert_eq!(app.screen, screen);
        }
    }

    #[tokio::test]
    async fn opening_the_calibre_screen_reads_the_library_or_says_why_not() {
        let mut app = test_app().await;
        app.handle(Action::Menu).await.expect("menu");
        app.menu_index = menu_row(MenuItem::Calibre);
        app.handle(Action::Select).await.expect("open calibre");

        assert_eq!(app.screen, Screen::Calibre);
        if app.engine.calibre().can_read_library() {
            assert!(
                app.pending_calibre.is_some(),
                "with calibre here, opening the screen queues the read"
            );
        } else {
            assert!(
                app.pending_calibre.is_none(),
                "nothing to read, so nothing was queued"
            );
            let said = app.status.clone().unwrap_or_default();
            assert!(said.contains("calibre"), "{said}");
            for forbidden in ["install", "download", "brew", "http"] {
                assert!(!said.to_lowercase().contains(forbidden), "{said}");
            }
        }
    }

    #[test]
    fn an_absent_calibre_is_not_reported_as_a_failure() {
        let absent = describe_calibre_failure(&EngineError::CalibreMissing {
            tool: "calibredb".into(),
        });
        assert!(!absent.contains("couldn't"), "{absent}");
        for forbidden in ["install", "download", "brew", "http", ".com", "you need"] {
            assert!(!absent.to_lowercase().contains(forbidden), "{absent}");
        }

        let real = describe_calibre_failure(&EngineError::InvalidInput(
            "/typo has no metadata.db, so it is not a calibre library".into(),
        ));
        assert!(real.contains("couldn't read the library"), "{real}");
        assert!(real.contains("metadata.db"), "{real}");
    }

    #[tokio::test]
    async fn a_calibre_sync_queues_the_selection_and_drains_one_row_at_a_time() {
        let mut app = test_app().await;
        app.screen = Screen::Calibre;
        app.calibre = sample_calibre_rows();
        app.calibre_state.select(Some(0));
        // A directory with no `metadata.db`: the engine refuses before calibredb runs, so this needs no
        // calibre.
        app.calibre_library = Some(scratch("nolib"));

        app.handle(Action::Sync).await.expect("sync");
        assert_eq!(
            app.pending_calibre_import.as_ref().map(|q| q.len()),
            Some(2)
        );
        assert!(app.has_deferred());

        app.pump_deferred().await.expect("pump");
        assert_eq!(
            app.pending_calibre_import.as_ref().map(|q| q.len()),
            Some(1)
        );
        app.pump_deferred().await.expect("pump");
        assert!(
            app.pending_calibre_import.is_none(),
            "an emptied queue must not be left as Some — the loop would spin"
        );
    }

    #[tokio::test]
    async fn a_calibre_sync_leaves_the_undecided_rows_alone() {
        let mut app = test_app().await;
        app.screen = Screen::Calibre;
        app.calibre = sample_calibre_rows();
        app.calibre_state.select(Some(0));
        app.handle(Action::Sync).await.expect("sync");

        let queued: Vec<i64> = app
            .pending_calibre_import
            .as_ref()
            .expect("a queue")
            .iter()
            .map(|j| j.calibre_id)
            .collect();
        assert!(
            !queued.contains(&3),
            "the candidate row was swept up: {queued:?}"
        );
        assert!(
            !queued.contains(&4),
            "the unreadable row was swept up: {queued:?}"
        );
        assert!(queued.iter().all(|j| !matches!(j, 3 | 4)));
    }

    #[tokio::test]
    async fn enter_on_an_undecided_calibre_row_refuses_and_names_both_moves() {
        let mut app = test_app().await;
        app.screen = Screen::Calibre;
        app.calibre = sample_calibre_rows();
        app.calibre_state.select(Some(2));

        app.handle(Action::Select).await.expect("enter");
        assert!(
            app.pending_calibre_import.is_none(),
            "nothing may be created behind the user's back"
        );
        let said = app.status.clone().unwrap_or_default();
        assert!(said.contains("l to link"), "{said}");
        assert!(said.contains('n'), "{said}");

        app.handle(Action::CreateAnyway).await.expect("n");
        let job = app
            .pending_calibre_import
            .as_ref()
            .and_then(|q| q.front())
            .expect("queued");
        assert_eq!(job.calibre_id, 3);
        assert!(job.create_ambiguous, "n is the --new escape hatch");
    }

    #[tokio::test]
    async fn a_calibre_row_without_a_uuid_is_not_offered_a_link() {
        let mut app = test_app().await;
        app.screen = Screen::Calibre;
        app.calibre = sample_calibre_rows();
        app.calibre[2].uuid = None;
        app.calibre_state.select(Some(2));

        app.handle(Action::Link).await.expect("l");
        assert!(app.link_picker.is_none(), "no picker without an identity");
        let said = app.status.clone().unwrap_or_default();
        assert!(said.contains("uuid"), "{said}");
        assert!(said.contains('n'), "the next move is still named: {said}");
    }

    #[tokio::test]
    async fn linking_a_calibre_row_records_it_and_then_imports() {
        let mut app = test_app().await;
        app.screen = Screen::Calibre;
        app.calibre = sample_calibre_rows();
        app.calibre_state.select(Some(2));
        let book_id = app.library[0].id.expect("a book");
        app.calibre[2].state = CalibreRowState::Candidates(vec![MatchCandidate {
            book_id,
            title: app.library[0].title.clone().unwrap_or_default(),
            authors_display: app.library[0].authors_in_display_order(),
            publish_year: app.library[0].publish_year,
            score: 0.78,
        }]);

        app.handle(Action::Link).await.expect("l");
        assert!(app.link_picker.is_some(), "the chooser opened");
        app.handle(Action::Select).await.expect("choose");
        assert!(app.link_picker.is_none(), "and closed");

        assert_eq!(
            app.engine
                .storage()
                .book_for_external_id("calibre", "uuid-3")
                .await
                .expect("query"),
            Some(book_id)
        );
        assert!(matches!(
            app.calibre[2].state,
            CalibreRowState::Linked { .. }
        ));
        assert!(
            app.pending_calibre_import.is_some(),
            "the import was queued"
        );
    }

    #[tokio::test]
    async fn an_empty_library_path_means_calibres_own() {
        let mut app = test_app().await;
        app.screen = Screen::Calibre;
        app.calibre_library = Some(PathBuf::from("/somewhere/else"));

        app.handle(Action::Query).await.expect("/");
        assert!(app.input.is_some(), "the box opened");
        app.commit_input(InputContext::CalibreLibrary, String::new())
            .await
            .expect("empty enter");

        assert_eq!(app.screen, Screen::Calibre);
        assert_eq!(
            app.pending_calibre,
            Some(None),
            "an empty path queues a scan of calibre's default library"
        );
    }

    /// Uses a fake `ebook-convert`: the overwrite guard sits behind `Calibre::require`, so without one
    /// the question is never asked. The stub is never spawned.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_conversion_over_an_existing_file_asks_first() {
        let mut app =
            test_app_with_calibre(Some(fake_calibre("convert-bin", "ebook-convert"))).await;
        let dir = scratch("convert");
        let input = dir.join("in.epub");
        let output = dir.join("out.azw3");
        std::fs::write(&input, b"epub").unwrap();
        std::fs::write(&output, b"already here").unwrap();

        app.run_convert(input.clone(), output.clone(), false).await;
        match &app.confirm {
            Some(Confirm::OverwriteConversion { output: o, .. }) => assert_eq!(o, &output),
            other => panic!("expected the overwrite question, got {other:?}"),
        }
        app.resolve_confirm(false).await.expect("decline");
        assert_eq!(
            std::fs::read(&output).unwrap(),
            b"already here",
            "declining must not touch the file"
        );
        let said = app.status.clone().unwrap_or_default();
        assert!(said.contains("out.azw3"), "{said}");
    }

    #[tokio::test]
    async fn converting_asks_for_both_paths_in_turn() {
        let mut app = test_app().await;
        app.screen = Screen::Calibre;
        app.handle(Action::Convert).await.expect("c");
        assert_eq!(
            app.input.as_ref().map(|i| i.context),
            Some(InputContext::ConvertInput)
        );

        app.commit_input(InputContext::ConvertInput, "/books/in.epub".into())
            .await
            .expect("input path");
        assert_eq!(
            app.input.as_ref().map(|i| i.context),
            Some(InputContext::ConvertOutput)
        );
        assert_eq!(app.pending_convert, Some(PathBuf::from("/books/in.epub")));
    }

    #[tokio::test]
    async fn the_goodreads_screen_opens_empty_and_writes_nothing_by_itself() {
        let mut app = test_app().await;
        app.screen = Screen::Menu;
        app.menu_index = menu_row(MenuItem::Goodreads);
        app.handle(Action::Select).await.expect("open");

        assert_eq!(app.screen, Screen::Goodreads);
        assert!(app.goodreads.is_none());
        assert!(!app.has_deferred(), "opening the screen reads no file");

        app.handle(Action::Sync).await.expect("s");
        let said = app.status.clone().unwrap_or_default();
        assert!(said.contains('/'), "{said}");
    }

    #[tokio::test]
    async fn a_typed_csv_path_previews_before_it_writes() {
        let mut app = test_app().await;
        let csv = concat!(
            "Title,Author,ISBN,My Rating,Date Read,Date Added,Bookshelves,Review\n",
            "Middlemarch,George Eliot,,4,2019/03/12,2018/12/01,,\n"
        );
        let path = scratch("csv").join("export.csv");
        std::fs::write(&path, csv).unwrap();

        app.screen = Screen::Goodreads;
        app.handle(Action::Query).await.expect("/");
        app.commit_input(InputContext::GoodreadsPath, path.display().to_string())
            .await
            .expect("path");

        let job = app.pending_goodreads.clone().expect("queued");
        assert!(!job.apply, "a typed path must preview, never write");
        let before = app.library.len();
        while app.pump_deferred().await.expect("pump") {}

        let preview = app.goodreads.as_ref().expect("a preview");
        assert!(preview.dry_run);
        assert_eq!(preview.read, 1);
        assert_eq!(
            app.library.len(),
            before,
            "a preview must not change the library"
        );

        app.handle(Action::Sync).await.expect("s");
        assert_eq!(app.pending_goodreads.as_ref().map(|j| j.apply), Some(true));
        while app.pump_deferred().await.expect("pump") {}
        assert!(!app.goodreads.as_ref().expect("preview").dry_run);
        assert_eq!(app.library.len(), before + 1, "now it landed");
    }

    #[tokio::test]
    async fn a_goodreads_row_without_an_id_is_not_offered_a_link() {
        let mut app = test_app().await;
        app.screen = Screen::Goodreads;
        app.goodreads = Some(sample_goodreads_preview());
        app.goodreads.as_mut().unwrap().state.select(Some(2));

        app.handle(Action::Link).await.expect("l");
        assert!(app.link_picker.is_none());
        let said = app.status.clone().unwrap_or_default();
        assert!(said.contains("Goodreads id"), "{said}");
        assert!(said.contains('n'), "the next move is still named: {said}");
    }

    #[tokio::test]
    async fn linking_a_matched_goodreads_row_explains_itself() {
        let mut app = test_app().await;
        app.screen = Screen::Goodreads;
        app.goodreads = Some(sample_goodreads_preview());
        app.goodreads.as_mut().unwrap().state.select(Some(0));

        app.handle(Action::Link).await.expect("l");
        assert!(app.link_picker.is_none());
        assert!(app.status.is_some(), "a key that does nothing must say so");
    }

    #[tokio::test]
    async fn exporting_cards_writes_the_anki_file_and_names_where_it_went() {
        let mut app = test_app().await;
        let book = app.library.first().cloned().expect("seeded book");
        app.open_book(book).await.expect("open");

        app.handle(Action::Export).await.expect("x");
        let said = app.status.clone().unwrap_or_default();
        let dir = app.engine.exports_dir();
        assert!(said.starts_with("one card went out, to "), "{said}");
        assert!(said.contains(&dir.display().to_string()), "{said}");
        let path = said.trim_start_matches("one card went out, to ");
        let written = std::fs::read_to_string(path).expect("the file exists");
        assert!(written.contains("#guid column"), "{written}");
        assert!(written.contains("insufficient"), "{written}");

        app.handle(Action::Export).await.expect("x again");
        let said = app.status.clone().unwrap_or_default();
        assert!(said.contains("nothing was written"), "{said}");
    }

    #[tokio::test]
    async fn exporting_writes_the_csv_and_names_where_it_went() {
        let mut app = test_app().await;
        let out = scratch("export").join("goodreads.csv");

        app.screen = Screen::Goodreads;
        app.handle(Action::Export).await.expect("x");
        assert_eq!(
            app.input.as_ref().map(|i| i.context),
            Some(InputContext::GoodreadsOut)
        );
        app.commit_input(InputContext::GoodreadsOut, out.display().to_string())
            .await
            .expect("write");

        let written = std::fs::read_to_string(&out).expect("the file exists");
        assert!(written.starts_with("Title,Author,ISBN"), "{written}");
        let said = app.status.clone().unwrap_or_default();
        assert!(said.contains("goodreads.csv"), "{said}");
    }

    #[tokio::test]
    async fn the_import_screens_show_no_badge_of_what_is_undone() {
        let mut app = test_app().await;
        app.status = None;

        for screen in [Screen::Calibre, Screen::Goodreads] {
            app.screen = screen;
            let text = screen_text(&mut app, 90, 20);
            for line in text.lines() {
                assert!(
                    !line.chars().any(|c| c.is_ascii_digit()),
                    "{screen:?} put a number on an empty shelf: {line:?}"
                );
            }
        }
    }

    #[tokio::test]
    async fn the_library_key_cycles_the_order_and_the_border_says_so() {
        use crate::ui::library::Sort;

        let mut app = test_app().await;
        app.screen = Screen::Library;
        app.status = None;
        for (title, author, year) in [
            ("Middlemarch", "George Eliot", 1871),
            ("A Wizard of Earthsea", "Ursula K. Le Guin", 1968),
        ] {
            app.engine
                .save_book(&Book {
                    title: Some(title.into()),
                    authors: vec![author.into()],
                    publish_year: Some(year),
                    ..Book::default()
                })
                .await
                .expect("save");
        }
        app.refresh_library().await.expect("refresh");
        assert_eq!(app.library_sort, Sort::Recent);

        let saved = app.config_dir.clone().expect("test config dir");
        for want in [Sort::Title, Sort::Author, Sort::Year, Sort::Recent] {
            app.handle(Action::CycleSort).await.expect("sort");
            assert_eq!(app.library_sort, want);
            let text = screen_text(&mut app, 90, 20);
            assert!(
                text.contains(&format!("by {}", want.label())),
                "the border does not name {want:?}:\n{text}"
            );
            assert_eq!(
                config::load(&saved).expect("saved").library_sort.as_deref(),
                Some(want.label()),
                "the order was not saved to the app's own config dir"
            );
        }

        app.library_sort = Sort::Author;
        app.refresh_library().await.expect("refresh");
        assert_eq!(
            app.library.first().map(|b| b.display_title()),
            Some("Middlemarch")
        );
    }

    #[tokio::test]
    async fn the_cursor_follows_the_book_through_a_re_sort() {
        use crate::ui::library::Sort;

        let mut app = test_app().await;
        app.screen = Screen::Library;
        for (title, author) in [
            ("A Wizard of Earthsea", "Ursula K. Le Guin"),
            ("Middlemarch", "George Eliot"),
        ] {
            app.engine
                .save_book(&Book {
                    title: Some(title.into()),
                    authors: vec![author.into()],
                    ..Book::default()
                })
                .await
                .expect("save");
        }
        app.refresh_library().await.expect("refresh");

        app.library_state.select(Some(app.library.len() - 1));
        let standing_on = app.library[app.library.len() - 1].id;
        assert!(standing_on.is_some());

        while app.library_sort != Sort::Author {
            app.advance_library_sort().await.expect("sort");
        }
        let now = app
            .library_state
            .selected()
            .and_then(|i| app.library.get(i))
            .and_then(|b| b.id);
        assert_eq!(now, standing_on, "the cursor moved to a different book");
    }

    #[test]
    fn the_snapshot_carries_the_order_beside_every_other_setting() {
        use crate::ui::library::Sort;

        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut app = rt.block_on(test_app());
        app.set_library_sort(Sort::Author);
        let cfg = app.config_snapshot();
        assert_eq!(cfg.library_sort.as_deref(), Some("author"));
        assert!(cfg.accent.is_some());
        assert!(cfg.ambient.is_some());
    }

    #[test]
    fn a_row_wraps_and_a_page_stops_at_the_end() {
        assert_eq!(Move::Row(-1).land(0, 40), 39);
        assert_eq!(Move::Row(1).land(39, 40), 0);

        assert_eq!(Move::Page(1).land(0, 40), PAGE as usize);
        assert_eq!(Move::Page(1).land(35, 40), 39);
        assert_eq!(Move::Page(-1).land(4, 40), 0);
        assert_eq!(Move::Page(-1).land(0, 40), 0);

        assert_eq!(Move::Page(1).land(0, 5), 4);
        assert_eq!(Move::Page(-1).land(4, 5), 0);

        assert_eq!(Move::Page(1).land(0, 0), 0);
        assert_eq!(Move::Row(1).land(0, 0), 0);
    }

    #[tokio::test]
    async fn the_page_keys_move_every_list() {
        let mut app = test_app().await;
        for i in 0..40 {
            app.library.push(Book {
                title: Some(format!("Book {i}")),
                ..Book::default()
            });
        }
        app.search_results = app
            .library
            .iter()
            .map(|b| bookmole::RankedResult {
                book: b.clone(),
                sources: Vec::new(),
                score: 1.0,
                shelf_id: b.id,
            })
            .collect();
        app.search_state.select(Some(0));
        app.library_state.select(Some(0));

        for (screen, at) in [(Screen::Library, 0usize), (Screen::Search, 0usize)] {
            app.screen = screen;
            app.handle(Action::PageDown).await.expect("page down");
            let landed = match screen {
                Screen::Library => app.library_state.selected(),
                _ => app.search_state.selected(),
            };
            assert_eq!(
                landed,
                Some(at + PAGE as usize),
                "{screen:?} did not jump a page"
            );
            app.handle(Action::PageUp).await.expect("page up");
            let landed = match screen {
                Screen::Library => app.library_state.selected(),
                _ => app.search_state.selected(),
            };
            assert_eq!(landed, Some(at), "{screen:?} did not come back");
        }

        let book = app.library[0].clone();
        app.open_book(book).await.expect("open");
        app.book_tab = BookTab::Highlights;
        app.in_section = true;
        app.clamp_tab_selection();
        let len = app
            .view
            .as_ref()
            .map_or(0, |v| v.tab_len(BookTab::Highlights));
        assert!(len > 0, "the fixture seeds a highlight");
        app.handle(Action::PageDown).await.expect("page down");
        assert_eq!(app.tab_state.selected(), Some(len - 1));
        app.handle(Action::PageUp).await.expect("page up");
        assert_eq!(app.tab_state.selected(), Some(0));
    }

    #[tokio::test]
    async fn the_help_key_opens_the_page_for_the_screen_underneath() {
        let mut app = test_app().await;
        app.status = None;
        let book = app.library[0].clone();
        app.open_book(book).await.expect("open");

        for (screen, want) in [
            (Screen::Menu, "bookmole"),
            (Screen::Home, "reading"),
            (Screen::Library, "library"),
            (Screen::Book, "a book"),
            (Screen::Search, "search"),
            (Screen::Settings, "settings"),
            (Screen::Device, "device"),
            (Screen::Calibre, "calibre"),
            (Screen::Goodreads, "goodreads"),
        ] {
            app.screen = screen;
            dispatch_key(&mut app, KeyEvent::from(KeyCode::Char('?')))
                .await
                .expect("help");
            assert!(app.help, "? did not open the page on {screen:?}");
            let text = screen_text(&mut app, 90, 40);
            assert!(
                text.contains(want),
                "{screen:?}'s page is not the one that was drawn:\n{text}"
            );
            dispatch_key(&mut app, KeyEvent::from(KeyCode::Esc))
                .await
                .expect("close");
            assert!(!app.help);
        }
    }

    #[tokio::test]
    async fn any_key_closes_the_help_page_and_none_reaches_the_screen_behind() {
        let mut app = test_app().await;
        // Reached through the menu: with an empty trail a leaked `Back` is a no-op and the test asserts
        // nothing.
        app.menu_index = menu_row(MenuItem::Library);
        app.handle(Action::Select).await.expect("library");
        assert_eq!(app.trail, vec![Screen::Menu]);
        let before = app.library.len();

        for code in [
            KeyCode::Esc,
            KeyCode::Char('?'),
            KeyCode::Char('j'),
            KeyCode::Enter,
            KeyCode::Char('d'),
            KeyCode::Char('q'),
        ] {
            app.help = true;
            dispatch_key(&mut app, KeyEvent::from(code))
                .await
                .expect("key");
            assert!(!app.help, "{code:?} left the page up");
            assert!(!app.quit, "{code:?} quit the app from the help page");
            assert!(app.confirm.is_none(), "{code:?} reached the screen behind");
            assert_eq!(app.library.len(), before);
            assert_eq!(app.screen, Screen::Library, "{code:?} navigated away");
        }

        app.help = true;
        dispatch_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        )
        .await
        .expect("ctrl-c");
        assert!(!app.help);
        assert!(app.quit);
    }

    #[tokio::test]
    async fn the_help_page_calms_what_is_behind_it() {
        let mut app = test_app().await;
        let book = app.library[0].clone();
        app.open_book(book).await.expect("open");
        assert!(app.animating());
        app.help = true;
        assert!(!app.animating());

        app.help = false;
        app.screen = Screen::Menu;
        app.ambient.motif = crate::ambient::Motif::Motes;
        assert!(app.ambient_animating());
        app.help = true;
        assert!(!app.ambient_animating());
        assert!(app.ambient_visible());
    }

    #[tokio::test]
    async fn the_cloud_folder_line_rides_the_status_line_at_every_size() {
        let mut app = test_app().await;
        assert_eq!(app.engine.library_location().cloud_folder, None);

        app.status = Some(crate::location::cloud_folder_line(&bookmole::CloudFolder {
            service: bookmole::CloudService::ICloudDrive,
            root: PathBuf::from("/Users/u/Library/Mobile Documents"),
        }));
        let text = screen_text(&mut app, 200, 40);
        assert!(text.contains("The library is in iCloud Drive."), "{text}");
        for screen in ALL_SCREENS {
            app.screen = screen;
            for (w, h) in [(120, 40), (80, 24), (20, 8), (4, 2), (1, 1)] {
                screen_text(&mut app, w, h);
            }
        }
    }

    async fn press(app: &mut App, code: KeyCode) {
        on_key(app, KeyEvent::from(code)).await;
    }

    async fn type_keys(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c)).await;
        }
    }

    fn draft_text(app: &App) -> Option<String> {
        app.note_editor.as_ref().map(|d| d.editor.text())
    }

    /// A file where the folder was: creating a note there fails for every user, root included.
    fn block_dir(dir: &Path) -> PathBuf {
        let aside = dir.with_extension("aside");
        std::fs::rename(dir, &aside).expect("move the folder aside");
        std::fs::write(dir, "").expect("put a file in its place");
        aside
    }

    fn unblock_dir(dir: &Path, aside: &Path) {
        std::fs::remove_file(dir).expect("remove the blocker");
        std::fs::rename(aside, dir).expect("put the folder back");
    }

    /// The seeded note, selected in its book's Notes tab, and the vault folder it lives in.
    async fn on_seeded_note() -> (App, PathBuf, PathBuf) {
        let mut app = test_app().await;
        let book = app.library.first().cloned().expect("seeded book");
        app.open_book(book).await.expect("open");
        app.book_tab = BookTab::Notes;
        app.in_section = true;
        app.clamp_tab_selection();
        let note = app.view.as_ref().unwrap().notes[0].clone();
        let file = app.engine.vault_dir().join(&note.file_path);
        let dir = file.parent().expect("book folder").to_path_buf();
        (app, file, dir)
    }

    fn assert_failed(app: &App) {
        assert!(!app.quit, "a failed action ended the app");
        let status = app.status.clone().unwrap_or_default();
        assert!(status.starts_with("could not finish that"), "{status}");
    }

    #[tokio::test]
    async fn a_failing_key_is_reported_and_the_app_keeps_running() {
        let (mut app, file, _) = on_seeded_note().await;
        // A non-empty directory where the note file was: removing it fails, root included.
        std::fs::remove_file(&file).expect("remove note file");
        std::fs::create_dir_all(file.join("kept")).expect("directory in its place");

        app.handle(Action::Delete).await.expect("ask");
        assert!(matches!(app.confirm, Some(Confirm::DeleteNote(_))));
        press(&mut app, KeyCode::Char('y')).await;

        assert_failed(&app);
        assert_eq!(
            app.view.as_ref().unwrap().notes.len(),
            1,
            "the note row went"
        );
        let text = screen_text(&mut app, 120, 40);
        assert!(text.contains("could not finish that"), "{text}");

        press(&mut app, KeyCode::Down).await;
        assert!(!app.quit);
    }

    #[tokio::test]
    async fn a_failed_save_of_an_edited_note_keeps_the_draft() {
        let (mut app, file, _) = on_seeded_note().await;
        let original = std::fs::read_to_string(&file).unwrap();
        app.handle(Action::Select).await.expect("open editor");
        type_keys(&mut app, " and more").await;
        let typed = draft_text(&app).expect("editor open");
        assert!(typed.ends_with(" and more"), "{typed}");

        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir(&file).unwrap();
        press(&mut app, KeyCode::Enter).await;
        assert_failed(&app);
        assert_eq!(
            draft_text(&app).as_deref(),
            Some(typed.as_str()),
            "the draft is gone"
        );

        std::fs::remove_dir(&file).unwrap();
        std::fs::write(&file, original).unwrap();
        press(&mut app, KeyCode::Enter).await;
        assert!(app.note_editor.is_none(), "the retry did not save");
        assert!(
            std::fs::read_to_string(&file)
                .unwrap()
                .contains(" and more")
        );
    }

    #[tokio::test]
    async fn a_failed_save_of_a_note_on_a_highlight_keeps_the_draft() {
        let (mut app, _, dir) = on_seeded_note().await;
        app.book_tab = BookTab::Highlights;
        app.clamp_tab_selection();
        app.handle(Action::Select)
            .await
            .expect("new note on the highlight");
        type_keys(&mut app, "on survival").await;

        let aside = block_dir(&dir);
        press(&mut app, KeyCode::Enter).await;
        assert_failed(&app);
        assert_eq!(
            draft_text(&app).as_deref(),
            Some("on survival"),
            "the draft is gone"
        );

        unblock_dir(&dir, &aside);
        press(&mut app, KeyCode::Enter).await;
        assert!(app.note_editor.is_none(), "the retry did not save");
        assert_eq!(app.view.as_ref().unwrap().notes.len(), 2);
    }

    #[tokio::test]
    async fn a_failed_save_after_the_page_prompt_returns_the_draft_to_the_editor() {
        let (mut app, _, dir) = on_seeded_note().await;
        app.handle(Action::NewNote).await.expect("new note");
        type_keys(&mut app, "a loose thought").await;
        press(&mut app, KeyCode::Enter).await;
        assert_eq!(
            app.input.as_ref().map(|i| i.context),
            Some(InputContext::NotePage)
        );

        let aside = block_dir(&dir);
        type_keys(&mut app, "12").await;
        press(&mut app, KeyCode::Enter).await;
        assert_failed(&app);
        assert!(app.input.is_none());
        assert_eq!(
            draft_text(&app).as_deref(),
            Some("a loose thought"),
            "the draft is gone"
        );

        unblock_dir(&dir, &aside);
        press(&mut app, KeyCode::Enter).await;
        press(&mut app, KeyCode::Enter).await;
        assert!(app.note_editor.is_none() && app.input.is_none());
        assert_eq!(app.view.as_ref().unwrap().notes.len(), 2);
    }

    #[tokio::test]
    async fn a_failed_progress_write_keeps_the_typed_page() {
        let (mut app, _, _) = on_seeded_note().await;
        let id = app.view.as_ref().unwrap().book.id.expect("id");
        app.handle(Action::EditProgress)
            .await
            .expect("ask for a page");
        type_keys(&mut app, "42").await;
        app.engine.delete_book(id).await.expect("delete elsewhere");
        press(&mut app, KeyCode::Enter).await;

        assert_failed(&app);
        let input = app.input.as_mut().expect("the page prompt closed");
        assert_eq!(input.context, InputContext::ProgressPage);
        assert_eq!(input.state.take(), "42");
    }

    #[tokio::test]
    async fn a_refused_rating_keeps_the_typed_value() {
        let mut app = home_app().await;
        app.handle(Action::Review).await.expect("review");
        type_keys(&mut app, "worth it").await;
        press(&mut app, KeyCode::Enter).await;
        assert!(app.pending_rating.is_some());

        type_keys(&mut app, "4.3").await;
        press(&mut app, KeyCode::Enter).await;
        assert!(!app.quit);
        assert!(
            app.pending_rating.is_some(),
            "the review lost its rating prompt"
        );
        let input = app.input.as_mut().expect("the rating prompt closed");
        assert_eq!(input.context, InputContext::ReviewRating);
        assert_eq!(input.state.take(), "4.3");
    }
}
