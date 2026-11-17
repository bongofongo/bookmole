//! A bounded, read-only reader for EPUB 2 and EPUB 3 packages.
//!
//! `epubkit` opens an EPUB's zip container, follows `META-INF/container.xml`
//! to the package document (the OPF), and parses the manifest, the spine and
//! the whole metadata block **once**, at open. Nothing else is read until it
//! is asked for: a resource's bytes, the cover, the table of contents (EPUB 3
//! navigation document or EPUB 2 NCX, see [`Package::toc`]), a chapter's
//! bytes or its text as blocks ([`Package::spine_text`]).
//!
//! It is written for files that arrive from e-readers, downloads and other
//! people's tools, so every read is bounded by a [`Limits`] — the number of zip
//! entries, a single entry's decompressed size, the total decompressed bytes
//! read through one [`Package`], and the size, nesting depth, attribute count
//! and node count of any markup document. Caps are enforced while bytes are
//! decompressed, not trusted from the archive's own declarations. Nothing is
//! ever extracted to disk, no DTD is processed, and no entity is expanded
//! beyond XML's five, HTML 4's 253 named ones and numeric character
//! references.
//!
//! The package document, the container and an NCX are read as XML and must
//! be well-formed. Navigation documents and chapters are XHTML as real books
//! carry it, so they are read leniently — upper-case tags, `<br>` left open,
//! a stray `&`, an end tag that skips one — and a document that breaks off
//! keeps what came before the break.
//!
//! A book that is readable but imperfect — a manifest item absent from the
//! archive, an unresolvable `refines`, an undeclared entity, a chapter that is
//! not well-formed — opens anyway and records a typed [`Warning`] on the
//! package. A book that cannot be read at all is an [`Error`]. Nothing here
//! panics on any input, logs, or holds global state.
//!
//! ```no_run
//! use epubkit::Package;
//!
//! # fn main() -> Result<(), epubkit::Error> {
//! let mut book = Package::open("book.epub")?;
//! let md = book.metadata();
//! println!("{:?} by {:?}", md.titles.first().map(|t| &t.value), md.creators.first().map(|c| &c.name));
//! for item in book.spine().items.iter() {
//!     println!("spine: {}", item.idref);
//! }
//! if let Some(cover) = book.cover() {
//!     println!("cover: {} bytes of {}", cover.bytes.len(), cover.media_type);
//! }
//! if let Some(toc) = book.toc() {
//!     for (depth, entry) in toc.iter() {
//!         println!("{}{} -> {:?}", "  ".repeat(depth), entry.label, entry.path);
//!     }
//! }
//! for block in book.spine_text(0)? {
//!     println!("{:?}: {}", block.kind, block.text);
//! }
//! for w in book.warnings() {
//!     eprintln!("warning: {w}");
//! }
//! # Ok(())
//! # }
//! ```
#![warn(missing_docs)]
#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)
)]

mod archive;
mod entities;
mod error;
mod metadata;
mod package;
mod path;
mod text;
mod toc;
mod xml;

#[cfg(test)]
mod testkit;

pub use error::{Error, Limit, MarkupProblem, TocProblem, Warning};
pub use metadata::{
    Contributor, Date, Identifier, Meta, MetaEntry, Metadata, Series, SeriesSource, Title,
};
pub use package::{Cover, ManifestItem, Package, Spine, SpineItem};
pub use text::{Block, BlockKind, SpineDocument, SpineDocuments};
pub use toc::{Flatten, Navigation, Toc, TocNode, TocSource, flatten};

/// The bounds every read through a [`Package`] is held to.
///
/// The [`Default`] is conservative but generous enough for real books,
/// including image-heavy ones: it exists to stop a zip bomb or a pathological
/// XML document, not to police ordinary files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Limits {
    /// The most entries the zip's central directory may list.
    pub max_entries: usize,
    /// The most bytes a single entry may decompress to.
    pub max_entry_bytes: u64,
    /// The most decompressed bytes that may be read through one `Package`,
    /// across every read it makes, including the ones made at open.
    pub max_total_bytes: u64,
    /// The deepest element nesting any markup document may have. It also
    /// bounds how deep a table of contents can nest. Parsed trees are walked
    /// and dropped recursively, so keep this in the hundreds.
    pub max_xml_depth: usize,
    /// The largest a markup document (container, OPF, NCX, navigation
    /// document, chapter) may be, in bytes, after decoding to UTF-8.
    pub max_xml_bytes: usize,
    /// The most attributes one element may carry.
    pub max_xml_attributes: usize,
    /// The most nodes (elements and runs of character data) one markup
    /// document may produce. The parsed tree costs on the order of a hundred
    /// bytes a node, so this — not [`Limits::max_xml_bytes`] — is what bounds
    /// the memory a document of tiny elements can claim.
    pub max_xml_nodes: usize,
}

impl Limits {
    /// The default number of zip entries: [`Limits::max_entries`].
    pub const DEFAULT_MAX_ENTRIES: usize = 10_000;
    /// The default single-entry cap: 64 MiB.
    pub const DEFAULT_MAX_ENTRY_BYTES: u64 = 64 * 1024 * 1024;
    /// The default whole-package cap: 512 MiB.
    pub const DEFAULT_MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
    /// The default XML nesting cap.
    pub const DEFAULT_MAX_XML_DEPTH: usize = 256;
    /// The default XML document cap: 8 MiB. Real package documents and NCX
    /// files are a few hundred KiB at most; the cap bounds the element tree
    /// built from one.
    pub const DEFAULT_MAX_XML_BYTES: usize = 8 * 1024 * 1024;
    /// The default attributes-per-element cap. Real elements carry a handful;
    /// the cap stops one element with a hundred thousand of them.
    pub const DEFAULT_MAX_XML_ATTRIBUTES: usize = 256;
    /// The default node cap: half a million. A 20 000-entry NCX, pretty
    /// printed, is about 200 000 nodes; a long single-file chapter rarely
    /// passes 100 000. At the cap the tree holds roughly 50–100 MiB.
    pub const DEFAULT_MAX_XML_NODES: usize = 500_000;

    /// Every limit at its default; the same as [`Limits::default`], usable in
    /// a `const`.
    pub const fn new() -> Self {
        Limits {
            max_entries: Self::DEFAULT_MAX_ENTRIES,
            max_entry_bytes: Self::DEFAULT_MAX_ENTRY_BYTES,
            max_total_bytes: Self::DEFAULT_MAX_TOTAL_BYTES,
            max_xml_depth: Self::DEFAULT_MAX_XML_DEPTH,
            max_xml_bytes: Self::DEFAULT_MAX_XML_BYTES,
            max_xml_attributes: Self::DEFAULT_MAX_XML_ATTRIBUTES,
            max_xml_nodes: Self::DEFAULT_MAX_XML_NODES,
        }
    }

    /// Set [`Limits::max_entries`].
    pub const fn with_max_entries(mut self, n: usize) -> Self {
        self.max_entries = n;
        self
    }

    /// Set [`Limits::max_entry_bytes`].
    pub const fn with_max_entry_bytes(mut self, n: u64) -> Self {
        self.max_entry_bytes = n;
        self
    }

    /// Set [`Limits::max_total_bytes`].
    pub const fn with_max_total_bytes(mut self, n: u64) -> Self {
        self.max_total_bytes = n;
        self
    }

    /// Set [`Limits::max_xml_depth`].
    pub const fn with_max_xml_depth(mut self, n: usize) -> Self {
        self.max_xml_depth = n;
        self
    }

    /// Set [`Limits::max_xml_bytes`].
    pub const fn with_max_xml_bytes(mut self, n: usize) -> Self {
        self.max_xml_bytes = n;
        self
    }

    /// Set [`Limits::max_xml_attributes`].
    pub const fn with_max_xml_attributes(mut self, n: usize) -> Self {
        self.max_xml_attributes = n;
        self
    }

    /// Set [`Limits::max_xml_nodes`].
    pub const fn with_max_xml_nodes(mut self, n: usize) -> Self {
        self.max_xml_nodes = n;
        self
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self::new()
    }
}
