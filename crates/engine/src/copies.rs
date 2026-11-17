//! Copies: which of a book you have, in what form, on what terms, and where it
//! sits (migration `0027`).
//!
//! **A book is in your library when a copy is recorded for it.** A book can
//! have any number of copies, of different kinds (a paperback and an epub, a
//! loan and a later purchase), and borrowed, rented and subscription copies
//! count while you have them. A book that is not in the library still exists:
//! it keeps its lists, history and notes and search still finds it; it is just
//! not on the shelf. Nothing about copies ever deletes a book row.
//!
//! Pure data and vocabulary. The SQL is `storage/copies.rs`; the derived facts
//! (`in_library`, `touched`, `read_without_copy`) are answered there too and
//! reach a frontend through [`BookStanding`].
//!
//! **Only the user changes [`Copy::have_now`].** The engine never flips it: not
//! when a due date passes, not when a file disappears from calibre or a
//! reader. Those are recorded as facts (`due_at` in the past,
//! [`CopyPlace::missing_since`]) and a frontend offers *I don't have it any
//! more*. `Some(false)` is the only value that takes a copy out of the
//! reckoning; `None` (not said) keeps the book in the library.
//!
//! Free text (`from_whom`, `app`, `location`, `lent_to`, `note`) is the user's
//! private writing: never logged above `trace!`, and an empty string is stored
//! as `NULL`.

use std::fmt;
use std::str::FromStr;

use crate::error::EngineError;

macro_rules! string_enum {
    ($(#[$m:meta])* $name:ident { $($(#[$vm:meta])* $var:ident => $s:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name { $($(#[$vm])* $var),+ }

        impl $name {
            /// Every value, in declaration order.
            pub const ALL: &'static [$name] = &[$($name::$var),+];

            /// The stored word.
            pub fn as_str(self) -> &'static str {
                match self { $($name::$var => $s),+ }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = EngineError;
            fn from_str(s: &str) -> Result<Self, EngineError> {
                match s {
                    $($s => Ok($name::$var),)+
                    other => Err(EngineError::InvalidInput(format!(
                        "unknown {}: {other:?}", stringify!($name)
                    ))),
                }
            }
        }
    };
}

pub(crate) use string_enum;

string_enum! {
    /// What kind of thing a copy is. A stored `NULL` is "not said" (a book
    /// migrated in with no evidence), which is `Option::None` on [`Copy`].
    CopyForm {
        /// A file you can open: epub, pdf, azw3.
        File => "file",
        /// Paper.
        Print => "print",
        /// Read in an app (Kindle, Apple Books, Libby, Audible) with no usable file.
        App => "app",
    }
}

string_enum! {
    /// On what terms you have it.
    Tenure {
        Own => "own",
        Borrowed => "borrowed",
        Rented => "rented",
        Subscription => "subscription",
        /// In the library before copies existed (or read on something the CSV
        /// did not say); how it was had is not known.
        Unrecorded => "unrecorded",
    }
}

string_enum! {
    /// What a place points to. **One file is one copy, wherever it sits**: the
    /// same epub in calibre, on a reader and in our own store is one copy with
    /// three places. The `ref` stored for each kind is given on its variant;
    /// `detail` carries the secondary key where there is one (calibre's library
    /// root and format, a reader's paired device id).
    PlaceKind {
        /// calibre book uuid.
        Calibre => "calibre",
        /// KOReader `partial_md5`.
        Reader => "reader",
        /// Absolute path on this computer.
        Path => "path",
        /// sha256 of a file in bookmole's own store.
        Store => "store",
        /// Apple Books asset id.
        AppleBooks => "apple_books",
    }
}

string_enum! {
    /// How we know about a copy.
    LearnedFrom {
        Calibre => "calibre",
        Koreader => "koreader",
        FileImport => "file_import",
        AppleBooks => "apple_books",
        Kindle => "kindle",
        User => "user",
        Migration => "migration",
    }
}

/// One place a copy sits. A place belongs to exactly one copy: `(kind,
/// reference)` is its identity, **not** its bytes (Preview rewrites a PDF when
/// it annotates it, so `last_hash` is only what we saw last time).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyPlace {
    pub copy_id: i64,
    pub kind: PlaceKind,
    /// The identity within `kind` (the column is `ref`).
    pub reference: String,
    /// The second half of what identifies it where one is needed: a calibre
    /// library root and format, a paired device id.
    pub detail: Option<String>,
    pub last_hash: Option<String>,
    pub last_seen_at: Option<i64>,
    /// File mtime at the last look; a `path` place is rescanned when it moves.
    pub last_mtime: Option<i64>,
    /// A fact about the place, never a verdict on the copy: missing is not gone.
    pub missing_since: Option<i64>,
}

/// A place about to be attached to a copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPlace {
    pub kind: PlaceKind,
    pub reference: String,
    pub detail: Option<String>,
    pub last_hash: Option<String>,
    pub last_mtime: Option<i64>,
}

impl NewPlace {
    pub fn new(kind: PlaceKind, reference: impl Into<String>) -> NewPlace {
        NewPlace {
            kind,
            reference: reference.into(),
            detail: None,
            last_hash: None,
            last_mtime: None,
        }
    }
}

/// One copy of a book.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Copy {
    pub id: i64,
    pub book_id: i64,
    /// `None` is "not said" (a migrated book with no evidence).
    pub form: Option<CopyForm>,
    pub tenure: Tenure,
    /// Three-valued: `Some(true)`, `Some(false)` (you no longer have it), or
    /// `None` (not said). **Only the user writes this.** Lent out is still
    /// `Some(true)`; an expired loan is still `Some(true)` until you say so.
    pub have_now: Option<bool>,
    pub from_whom: Option<String>,
    pub app: Option<String>,
    pub location: Option<String>,
    pub started_at: Option<i64>,
    pub due_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub lent_to: Option<String>,
    /// Copy-protected: we cannot open it, you have it. Never retry parsing.
    pub unreadable: bool,
    pub note: Option<String>,
    pub learned_from: LearnedFrom,
    pub created_at: i64,
    pub updated_at: i64,
    pub places: Vec<CopyPlace>,
}

/// A copy about to be recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCopy {
    pub book_id: i64,
    pub form: Option<CopyForm>,
    pub tenure: Tenure,
    pub have_now: Option<bool>,
    pub from_whom: Option<String>,
    pub app: Option<String>,
    pub location: Option<String>,
    pub started_at: Option<i64>,
    pub due_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub lent_to: Option<String>,
    pub unreadable: bool,
    pub note: Option<String>,
    pub learned_from: LearnedFrom,
}

impl NewCopy {
    /// A copy with only what every copy has; everything else unsaid.
    pub fn new(
        book_id: i64,
        form: Option<CopyForm>,
        tenure: Tenure,
        learned_from: LearnedFrom,
    ) -> NewCopy {
        NewCopy {
            book_id,
            form,
            tenure,
            have_now: None,
            from_whom: None,
            app: None,
            location: None,
            started_at: None,
            due_at: None,
            ended_at: None,
            lent_to: None,
            unreadable: false,
            note: None,
            learned_from,
        }
    }
}

/// Where a book stands, derived here so no frontend computes any of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BookStanding {
    /// A copy not explicitly ended (`have_now` true or not said), or any copy
    /// at all and the book touched.
    pub in_library: bool,
    /// Has a reading, reading event, highlight, note or flashcard. Being on a
    /// list or having metadata edited does not count. See
    /// `storage::copies::touched_sql` for the exact list.
    pub touched: bool,
    /// A finished reading and no copy at all: the book page asks *how did you
    /// read it?*.
    pub read_without_copy: bool,
}

/// One book's copy facts, for a library table's filters and marks. See
/// `Storage::library_copies` for the definitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyStanding {
    pub book_id: i64,
    pub have_now: bool,
    pub had: bool,
    pub borrowed: bool,
    pub copy_not_recorded: bool,
    /// The tenure to mark the row with: the first copy still had, else the
    /// oldest copy.
    pub tenure: Tenure,
}

/// How many in-library books each Library filter holds. They overlap
/// (`borrowed` is inside `have_now`), so they do not sum.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LibraryCopyCounts {
    pub have_now: i64,
    pub had: i64,
    pub borrowed: i64,
    pub copy_not_recorded: i64,
}

/// The Library table's copy facts: a row per in-library book that has copies
/// (no row: no copies, every flag false), and the filters' counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryCopies {
    pub rows: Vec<CopyStanding>,
    pub counts: LibraryCopyCounts,
}

/// What attaching a place did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceAttach {
    Added,
    /// Already on this copy.
    Present,
    /// It was on another copy of the same book, which is therefore the same
    /// copy: that copy was merged into this one.
    Merged {
        absorbed: i64,
    },
    /// It belongs to a copy of a different book. Nothing changed.
    OtherBook {
        book_id: i64,
        copy_id: i64,
    },
}

/// Facts about one copy that are derived rather than stored, so a frontend
/// never guesses them (see [`crate::Engine::copy_facts`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CopyFacts {
    /// The file's lowercased format when known; see [`derive_format`].
    pub format: Option<String>,
    /// *Keep a copy here* would work: a `path` place whose file is not marked
    /// missing and is on disk now, and no `store` place yet.
    pub can_keep_here: bool,
    /// For each `reader` place (by its `reference`), the name of the paired
    /// device recorded in its `detail`, where that device is known.
    pub reader_devices: Vec<(String, String)>,
}

/// Where one highlight came from, as facts (the wording is a frontend's).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightOriginFact {
    pub highlight_id: i64,
    pub origin: crate::HighlightOrigin,
    pub copy_id: Option<i64>,
    /// The paired reader the highlight's copy sits on, when known.
    pub device_name: Option<String>,
    /// The highlight's copy has a `path`, `store`, `calibre` or `apple_books`
    /// place: the file or app data is on this computer.
    pub on_this_computer: bool,
    pub pos_kind: Option<crate::PosKind>,
    pub text_unavailable: bool,
}

/// One file a place holds, as the origin last reported it (`copy_place_files`,
/// migration `0031`). Today only `calibre` places record these: one per format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaceFile {
    /// Lowercased, as [`crate::files::format_of`] derives it.
    pub format: String,
    /// Absolute, as reported at the last scan.
    pub path: std::path::PathBuf,
}

/// One file of a copy, for the book page's Copies place
/// ([`crate::Engine::copy_files`]). Facts only: where it is, how big, whether
/// it is there now. `on_disk` and `size` are read from the disk when asked,
/// because a moved file is not always marked missing (an epub `path` place
/// is not rescanned) and "where it was" is only honest as a look.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyFile {
    pub copy_id: i64,
    pub kind: PlaceKind,
    /// The place's reference (the column `ref`).
    pub reference: String,
    /// Lowercased; `None` when nothing says (no extension, no stored row, a
    /// calibre place whose files were never recorded).
    pub format: Option<String>,
    /// `None` for a calibre place scanned before `0031`, whose paths were
    /// never recorded.
    pub path: Option<std::path::PathBuf>,
    /// For a calibre place: the library root its `detail` records, where
    /// `Some("")` is calibre's own default library. `None` for other kinds and
    /// for a back-filled calibre place with no `detail`.
    pub calibre_library: Option<String>,
    /// Is a regular file at `path` now. `None` when there is no path to look at.
    pub on_disk: Option<bool>,
    /// Bytes: the stored row's for a `store` place, else read from the disk
    /// now (`None` when the file is not there).
    pub size: Option<i64>,
    /// The place's own facts, as recorded.
    pub missing_since: Option<i64>,
    pub last_seen_at: Option<i64>,
}

/// Where one copy's highlights (of one origin) came from and when that source
/// was last read in ([`crate::Engine::copy_highlight_sources`]). No count: a
/// number of highlights is not what this answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyHighlightSource {
    /// `None` is history with no copy.
    pub copy_id: Option<i64>,
    pub origin: crate::HighlightOrigin,
    /// The paired reader the copy sits on, as [`HighlightOriginFact`] derives it.
    pub device_name: Option<String>,
    /// When this source was last read in: KOReader, the copy's `reader` place's
    /// `last_seen_at`; Preview, its PDF `path` place's; Apple Books, its
    /// `apple_books` place's. `None` for Kindle, the Kindle app, a highlight
    /// made here, and a highlight with no copy — nothing records it for them.
    pub last_read_at: Option<i64>,
}

/// The library root a calibre place's `detail` records
/// (`<library root>\t<formats>`); empty is calibre's default library.
pub fn calibre_root(detail: &str) -> &str {
    detail.split('\t').next().unwrap_or("")
}

/// The first format a calibre place's `detail` lists
/// (`<library root>\t<formats, comma-joined>`).
pub fn calibre_format(detail: &str) -> Option<String> {
    let formats = detail.split_once('\t')?.1;
    formats
        .split(',')
        .map(|f| f.trim().to_lowercase())
        .find(|f| !f.is_empty())
}

/// A copy's format, lowercased. In order: a `path` place's extension, a
/// `store` place's `book_files` row (`stored` is the book's rows), then the
/// first format of a `calibre` place's `detail`. `None` when nothing says
/// (a reader-only or app copy).
pub fn derive_format(places: &[CopyPlace], stored: &[crate::storage::BookFile]) -> Option<String> {
    let of = |kind: PlaceKind| places.iter().filter(move |p| p.kind == kind);
    of(PlaceKind::Path)
        .map(|p| crate::files::format_of(std::path::Path::new(&p.reference)))
        .find(|f| f != "bin")
        .or_else(|| {
            of(PlaceKind::Store).find_map(|p| {
                stored
                    .iter()
                    .find(|f| f.sha256 == p.reference)
                    .map(|f| f.format.to_lowercase())
            })
        })
        .or_else(|| of(PlaceKind::Calibre).find_map(|p| calibre_format(p.detail.as_deref()?)))
}

#[cfg(test)]
mod facts_tests {
    use super::*;
    use crate::storage::BookFile;

    fn place(kind: PlaceKind, reference: &str, detail: Option<&str>) -> CopyPlace {
        CopyPlace {
            copy_id: 1,
            kind,
            reference: reference.into(),
            detail: detail.map(str::to_string),
            last_hash: None,
            last_seen_at: None,
            last_mtime: None,
            missing_since: None,
        }
    }

    fn file(sha: &str, format: &str) -> BookFile {
        BookFile {
            sha256: sha.into(),
            book_id: 1,
            format: format.into(),
            original_name: None,
            size: 1,
            added_at: 0,
        }
    }

    #[test]
    fn calibre_detail_gives_its_first_format() {
        assert_eq!(calibre_format("/lib\tEPUB,pdf"), Some("epub".into()));
        assert_eq!(calibre_format("\tazw3"), Some("azw3".into()));
        assert_eq!(calibre_format("/lib\t"), None);
        assert_eq!(calibre_format("no tab here"), None);
    }

    #[test]
    fn format_comes_from_path_then_store_then_calibre() {
        let path = place(PlaceKind::Path, "/home/me/Book.PDF", None);
        let store = place(PlaceKind::Store, "abc", None);
        let cal = place(PlaceKind::Calibre, "uuid", Some("/lib\tepub"));
        let stored = [file("abc", "MOBI")];
        assert_eq!(
            derive_format(&[cal.clone(), store.clone(), path.clone()], &stored),
            Some("pdf".into())
        );
        assert_eq!(
            derive_format(&[cal.clone(), store], &stored),
            Some("mobi".into())
        );
        assert_eq!(derive_format(&[cal], &[]), Some("epub".into()));
        // An extensionless path says nothing; a reader place says nothing.
        let bare = place(PlaceKind::Path, "/home/me/Book", None);
        let reader = place(PlaceKind::Reader, "md5", None);
        assert_eq!(derive_format(&[bare, reader], &[]), None);
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn string_forms_round_trip(
            f in 0..CopyForm::ALL.len(), t in 0..Tenure::ALL.len(),
            p in 0..PlaceKind::ALL.len(), l in 0..LearnedFrom::ALL.len(),
        ) {
            let (f, t, p, l) = (CopyForm::ALL[f], Tenure::ALL[t], PlaceKind::ALL[p], LearnedFrom::ALL[l]);
            prop_assert_eq!(f.as_str().parse::<CopyForm>().unwrap(), f);
            prop_assert_eq!(t.to_string().parse::<Tenure>().unwrap(), t);
            prop_assert_eq!(p.as_str().parse::<PlaceKind>().unwrap(), p);
            prop_assert_eq!(l.to_string().parse::<LearnedFrom>().unwrap(), l);
        }

        #[test]
        fn nothing_else_parses(s in "[a-z_]{0,12}") {
            prop_assert_eq!(Tenure::ALL.iter().any(|t| t.as_str() == s), s.parse::<Tenure>().is_ok());
            prop_assert_eq!(CopyForm::ALL.iter().any(|t| t.as_str() == s), s.parse::<CopyForm>().is_ok());
        }
    }
}
