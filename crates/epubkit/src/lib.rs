//! A bounded, read-only reader for EPUB 2 and EPUB 3 packages. Every read is held to
//! [`Limits`] and nothing panics on any input.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Limits {
    /// The most entries the zip's central directory may list.
    pub max_entries: usize,
    /// The most bytes a single entry may decompress to.
    pub max_entry_bytes: u64,
    /// Counted across every read through one `Package`, including those made at open.
    pub max_total_bytes: u64,
    /// Also bounds table-of-contents nesting. Trees are walked recursively: keep this in the hundreds.
    pub max_xml_depth: usize,
    /// The largest a markup document (container, OPF, NCX, navigation document, chapter) may be,
    /// in bytes, after decoding to UTF-8.
    pub max_xml_bytes: usize,
    /// The most attributes one element may carry.
    pub max_xml_attributes: usize,
    /// A parsed node costs about a hundred bytes, so this, not `max_xml_bytes`, bounds the memory of
    /// a document of tiny elements.
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
    /// The default XML document cap: 8 MiB.
    pub const DEFAULT_MAX_XML_BYTES: usize = 8 * 1024 * 1024;
    /// The default attributes-per-element cap.
    pub const DEFAULT_MAX_XML_ATTRIBUTES: usize = 256;
    /// The default node cap: half a million.
    pub const DEFAULT_MAX_XML_NODES: usize = 500_000;

    /// Every limit at its default; the same as [`Limits::default`], usable in a `const`.
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
