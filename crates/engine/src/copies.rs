//! Copies: which of a book you have, and where. A book is in the library when a copy is recorded
//! for it. Only the user changes `Copy::have_now`; free text is private (never above `trace!`)
//! and empty is stored as NULL.

use std::fmt;
use std::str::FromStr;

use crate::error::EngineError;

macro_rules! string_enum {
    ($(#[$m:meta])* $name:ident { $($(#[$vm:meta])* $var:ident => $s:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name { $($(#[$vm])* $var),+ }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$var),+];

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
    CopyForm {
        File => "file",
        Print => "print",
        App => "app",
    }
}

string_enum! {
    Tenure {
        Own => "own",
        Borrowed => "borrowed",
        Rented => "rented",
        Subscription => "subscription",
        Unrecorded => "unrecorded",
    }
}

string_enum! {
    /// One file is one copy wherever it sits: the same epub in calibre, on a reader and in our store
    /// is one copy with three places.
    PlaceKind {
        Calibre => "calibre",
        Reader => "reader",
        Path => "path",
        Store => "store",
        AppleBooks => "apple_books",
    }
}

string_enum! {
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

/// `(kind, reference)` is a place's identity, not its bytes (Preview rewrites a PDF it annotates).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyPlace {
    pub copy_id: i64,
    pub kind: PlaceKind,
    pub reference: String,
    pub detail: Option<String>,
    pub last_hash: Option<String>,
    pub last_seen_at: Option<i64>,
    pub last_mtime: Option<i64>,
    /// A fact about the place, never a verdict on the copy: missing is not gone.
    pub missing_since: Option<i64>,
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Copy {
    pub id: i64,
    pub book_id: i64,
    pub form: Option<CopyForm>,
    pub tenure: Tenure,
    /// Only the user writes this: a lent-out copy or an expired loan stays `Some(true)` until they
    /// say otherwise.
    pub have_now: Option<bool>,
    pub from_whom: Option<String>,
    pub app: Option<String>,
    pub location: Option<String>,
    pub started_at: Option<i64>,
    pub due_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub lent_to: Option<String>,
    /// Copy-protected: we cannot open it. Never retry parsing.
    pub unreadable: bool,
    pub note: Option<String>,
    pub learned_from: LearnedFrom,
    pub created_at: i64,
    pub updated_at: i64,
    pub places: Vec<CopyPlace>,
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BookStanding {
    /// A copy not explicitly ended (`have_now` true or not said), or any copy
    /// at all and the book touched.
    pub in_library: bool,
    /// Has a reading, reading event, highlight, note or flashcard; being on a list or a metadata edit
    /// does not count.
    pub touched: bool,
    pub read_without_copy: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(clippy::struct_excessive_bools)]
pub struct CopyStanding {
    pub book_id: i64,
    pub have_now: bool,
    pub had: bool,
    pub borrowed: bool,
    pub copy_not_recorded: bool,
    pub tenure: Tenure,
}

/// Filters overlap (`borrowed` is inside `have_now`), so the counts do not sum.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LibraryCopyCounts {
    pub have_now: i64,
    pub had: i64,
    pub borrowed: i64,
    pub copy_not_recorded: i64,
}

/// A row per in-library book that has copies; no row means no copies and every flag false.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryCopies {
    pub rows: Vec<CopyStanding>,
    pub counts: LibraryCopyCounts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceAttach {
    Added,
    Present,
    Merged { absorbed: i64 },
    OtherBook { book_id: i64, copy_id: i64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CopyFacts {
    pub format: Option<String>,
    /// *Keep a copy here* would work: a `path` place on disk now, not marked missing, and no `store`
    /// place yet.
    pub can_keep_here: bool,
    pub reader_devices: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightOriginFact {
    pub highlight_id: i64,
    pub origin: crate::HighlightOrigin,
    pub copy_id: Option<i64>,
    pub device_name: Option<String>,
    pub on_this_computer: bool,
    pub pos_kind: Option<crate::PosKind>,
    pub text_unavailable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaceFile {
    pub format: String,
    pub path: std::path::PathBuf,
}

/// `on_disk` and `size` are read from the disk when asked: a moved file is not always marked
/// missing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyFile {
    pub copy_id: i64,
    pub kind: PlaceKind,
    pub reference: String,
    pub format: Option<String>,
    pub path: Option<std::path::PathBuf>,
    /// A calibre place's library root; `Some("")` is calibre's default library.
    pub calibre_library: Option<String>,
    pub on_disk: Option<bool>,
    pub size: Option<i64>,
    pub missing_since: Option<i64>,
    pub last_seen_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyHighlightSource {
    pub copy_id: Option<i64>,
    pub origin: crate::HighlightOrigin,
    pub device_name: Option<String>,
    /// When this source was last read in. `None` for Kindle, the Kindle app, highlights made here and
    /// highlights with no copy: nothing records it for them.
    pub last_read_at: Option<i64>,
}

pub fn calibre_root(detail: &str) -> &str {
    detail.split('\t').next().unwrap_or("")
}

pub fn calibre_format(detail: &str) -> Option<String> {
    let formats = detail.split_once('\t')?.1;
    formats
        .split(',')
        .map(|f| f.trim().to_lowercase())
        .find(|f| !f.is_empty())
}

/// Lowercased: a `path` place's extension, else a `store` place's `book_files` row, else the
/// first calibre format. `None` when nothing says.
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
