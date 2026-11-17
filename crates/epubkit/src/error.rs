//! The two outcomes short of success: an [`Error`] (the book cannot be read)
//! and a [`Warning`] (it can, but something in it is wrong).

use std::fmt;

/// Which of the [`Limits`](crate::Limits) a read ran into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Limit {
    /// [`Limits::max_entries`](crate::Limits::max_entries).
    Entries,
    /// [`Limits::max_entry_bytes`](crate::Limits::max_entry_bytes).
    EntryBytes,
    /// [`Limits::max_total_bytes`](crate::Limits::max_total_bytes).
    TotalBytes,
    /// [`Limits::max_xml_depth`](crate::Limits::max_xml_depth).
    XmlDepth,
    /// [`Limits::max_xml_bytes`](crate::Limits::max_xml_bytes).
    XmlBytes,
    /// [`Limits::max_xml_attributes`](crate::Limits::max_xml_attributes).
    XmlAttributes,
    /// [`Limits::max_xml_nodes`](crate::Limits::max_xml_nodes).
    XmlNodes,
}

impl fmt::Display for Limit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Limit::Entries => "the zip entry count limit",
            Limit::EntryBytes => "the single-entry size limit",
            Limit::TotalBytes => "the total decompressed size limit",
            Limit::XmlDepth => "the XML nesting depth limit",
            Limit::XmlBytes => "the XML document size limit",
            Limit::XmlAttributes => "the attributes-per-element limit",
            Limit::XmlNodes => "the XML node count limit",
        })
    }
}

/// Why a package, or a resource in it, could not be read.
///
/// Messages name archive paths and manifest ids, never the book's text.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The file could not be opened or read.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    /// The bytes are not a zip archive this reader can open, or an entry in
    /// it is corrupt, encrypted or compressed with an unsupported method.
    #[error("zip error: {0}")]
    Zip(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),
    /// The archive has no `META-INF/container.xml`.
    #[error("META-INF/container.xml is missing")]
    MissingContainer,
    /// `container.xml` names no `rootfile` with a `full-path`.
    #[error("container.xml names no package document")]
    NoRootfile,
    /// A path the package needs is not in the archive.
    #[error("{path} is missing from the archive")]
    MissingEntry {
        /// The path, as the package named it.
        path: String,
    },
    /// No manifest item has this id.
    #[error("no manifest item has id {id:?}")]
    UnknownId {
        /// The id that was asked for.
        id: String,
    },
    /// An XML document is not well-formed enough to read.
    #[error("{path}: malformed XML: {message}")]
    MalformedXml {
        /// The archive path of the document.
        path: String,
        /// What the parser objected to.
        message: String,
    },
    /// An XML document is not in an encoding this reader decodes (UTF-8,
    /// UTF-16 with a byte-order mark, ISO-8859-1, US-ASCII).
    #[error("{path}: unsupported or invalid text encoding")]
    Encoding {
        /// The archive path of the document.
        path: String,
    },
    /// The package document lacks an element it cannot do without.
    #[error("the package document has no <{element}> element")]
    MissingElement {
        /// `manifest`, `spine` or `metadata`.
        element: &'static str,
    },
    /// A spine position past the end of the spine was asked for.
    #[error("spine position {index} is out of range (the spine has {len})")]
    SpineIndexOutOfRange {
        /// The position asked for.
        index: usize,
        /// How many positions the spine has.
        len: usize,
    },
    /// A resource that is not a markup document was asked for as text.
    #[error("manifest item {id:?} is {media_type}, not a markup document")]
    NotText {
        /// The manifest id.
        id: String,
        /// Its declared media type.
        media_type: String,
    },
    /// A read ran into one of the [`Limits`](crate::Limits).
    #[error("{limit} was exceeded{}", path.as_deref().map(|p| format!(" reading {p}")).unwrap_or_default())]
    LimitExceeded {
        /// Which limit.
        limit: Limit,
        /// The archive path being read, when there was one.
        path: Option<String>,
    },
}

impl Error {
    pub(crate) fn zip(e: zip::result::ZipError) -> Self {
        match e {
            zip::result::ZipError::Io(io) => Error::Io(io),
            other => Error::Zip(Box::new(other)),
        }
    }
}

/// Why a table-of-contents document — an NCX or an EPUB 3 navigation
/// document — could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TocProblem {
    /// The id that names it is not in the manifest.
    NotInManifest,
    /// The manifest item's file could not be read from the archive.
    Unreadable,
    /// The file is not well-formed enough to read, or is in an unsupported
    /// encoding.
    Malformed,
    /// Reading it ran into a limit.
    LimitExceeded,
    /// An NCX with no `navMap`.
    NoNavMap,
    /// A navigation document with no `nav` element.
    NoNav,
}

/// What was wrong with a markup document that was read anyway. What came
/// before the problem, or around it, was kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MarkupProblem {
    /// A syntax error stopped the read; everything before it was kept.
    Truncated,
    /// The document ended with elements still open; they were closed.
    Unclosed,
    /// An end tag closed elements it did not match, or matched nothing open.
    MismatchedEnd,
    /// The text was in no encoding this reader recognises, and was decoded as
    /// UTF-8 with replacement characters.
    Encoding,
    /// A character reference named no valid character, or an attribute could
    /// not be read; it was kept as text, or skipped.
    BadReference,
}

/// Something wrong in a book that was still readable.
///
/// Collected on the [`Package`](crate::Package) as it is found — at open for
/// the package document, later for reads made on demand.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Warning {
    /// A manifest `item` lacks an `id`, `href` or `media-type` and was
    /// skipped.
    ManifestItemIncomplete {
        /// Its position among the manifest's children.
        index: usize,
        /// The first required attribute it lacks.
        missing: &'static str,
    },
    /// Two manifest items share an id; the later one is the one used.
    DuplicateManifestId {
        /// The shared id.
        id: String,
    },
    /// A manifest item names a file the archive does not contain.
    ManifestItemMissing {
        /// The item's id.
        id: String,
        /// Its resolved path.
        path: String,
    },
    /// A spine `itemref` has no `idref` and was skipped.
    SpineItemIncomplete {
        /// Its position among the spine's children.
        index: usize,
    },
    /// A spine `itemref` names an id the manifest does not have.
    SpineIdrefUnknown {
        /// The idref.
        idref: String,
    },
    /// A metadata `refines` points at an id nothing in the package carries.
    UnresolvedRefines {
        /// The `refines` value as written.
        refines: String,
    },
    /// `<meta name="cover">` names an id the manifest does not have.
    CoverMetaUnknownId {
        /// The id it named.
        id: String,
    },
    /// The cover's manifest item could not be read.
    CoverUnreadable {
        /// The cover's manifest id.
        id: String,
    },
    /// A document used a named entity that is neither one of XML's five nor
    /// one of HTML 4's 253; it was kept as literal text. An entity declared
    /// in a DTD is never expanded, and lands here too.
    UndeclaredEntity {
        /// The archive path of the document.
        path: String,
        /// The entity's name, without `&` and `;`.
        name: String,
    },
    /// The NCX table of contents could not be read, so it is empty.
    NcxUnreadable {
        /// The NCX's manifest id.
        id: String,
        /// Why.
        problem: TocProblem,
    },
    /// The EPUB 3 navigation document could not be read, so it is empty.
    NavUnreadable {
        /// The navigation document's manifest id.
        id: String,
        /// Why.
        problem: TocProblem,
    },
    /// A markup document (a navigation document, a chapter) was not
    /// well-formed, and was read as far as it could be.
    MarkupRecovered {
        /// The archive path of the document.
        path: String,
        /// What was wrong.
        problem: MarkupProblem,
    },
}

impl Warning {
    /// A short, stable, `snake_case` name for this kind of warning that
    /// carries none of its data — safe to log or count where a path, an id
    /// or a book's text is not.
    pub fn kind(&self) -> &'static str {
        match self {
            Warning::ManifestItemIncomplete { .. } => "manifest_item_incomplete",
            Warning::DuplicateManifestId { .. } => "duplicate_manifest_id",
            Warning::ManifestItemMissing { .. } => "manifest_item_missing",
            Warning::SpineItemIncomplete { .. } => "spine_item_incomplete",
            Warning::SpineIdrefUnknown { .. } => "spine_idref_unknown",
            Warning::UnresolvedRefines { .. } => "unresolved_refines",
            Warning::CoverMetaUnknownId { .. } => "cover_meta_unknown_id",
            Warning::CoverUnreadable { .. } => "cover_unreadable",
            Warning::UndeclaredEntity { .. } => "undeclared_entity",
            Warning::NcxUnreadable { .. } => "ncx_unreadable",
            Warning::NavUnreadable { .. } => "nav_unreadable",
            Warning::MarkupRecovered { .. } => "markup_recovered",
        }
    }
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Warning::ManifestItemIncomplete { index, missing } => {
                write!(f, "manifest item {index} has no {missing} and was skipped")
            }
            Warning::DuplicateManifestId { id } => {
                write!(f, "manifest id {id:?} is used more than once")
            }
            Warning::ManifestItemMissing { id, path } => {
                write!(
                    f,
                    "manifest item {id:?} names {path}, which is not in the archive"
                )
            }
            Warning::SpineItemIncomplete { index } => {
                write!(f, "spine itemref {index} has no idref and was skipped")
            }
            Warning::SpineIdrefUnknown { idref } => {
                write!(
                    f,
                    "spine itemref names {idref:?}, which is not in the manifest"
                )
            }
            Warning::UnresolvedRefines { refines } => {
                write!(f, "metadata refines {refines:?}, which nothing carries")
            }
            Warning::CoverMetaUnknownId { id } => {
                write!(
                    f,
                    "the cover meta names {id:?}, which is not in the manifest"
                )
            }
            Warning::CoverUnreadable { id } => write!(f, "the cover {id:?} could not be read"),
            Warning::UndeclaredEntity { path, name } => {
                write!(f, "{path}: undeclared entity &{name}; kept as text")
            }
            Warning::NcxUnreadable { id, problem } => {
                write!(
                    f,
                    "the NCX table of contents {id:?} could not be read: {problem:?}"
                )
            }
            Warning::NavUnreadable { id, problem } => {
                write!(
                    f,
                    "the navigation document {id:?} could not be read: {problem:?}"
                )
            }
            Warning::MarkupRecovered { path, problem } => {
                write!(
                    f,
                    "{path}: not well-formed ({problem:?}), read as far as it could be"
                )
            }
        }
    }
}
