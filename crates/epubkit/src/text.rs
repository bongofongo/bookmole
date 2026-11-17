//! Chapter content: the spine's documents as bytes, and as plain text in
//! blocks.
//!
//! [`Package::spine_document`] reads one spine position's resource, bounded
//! like every other read. [`Package::spine_text`] reads it and turns its
//! XHTML into a sequence of [`Block`]s — headings, paragraphs, list items,
//! quotations, preformatted text — each holding its text with whitespace
//! collapsed (except in `pre`), and nothing of `head`, `script`, `style` or
//! `template`. Content is parsed leniently: the not-quite-XHTML real books
//! carry (upper-case tags, `<br>` left open, a stray `&`, an end tag that skips
//! one, a file that breaks off) yields what could be read plus a
//! [`Warning::MarkupRecovered`](crate::Warning::MarkupRecovered).
//!
//! This is the base for locating a passage in a book; it does not locate
//! anything itself.

use std::io::{Read, Seek};
use std::iter::FusedIterator;

use crate::error::Error;
use crate::package::{Package, parse_html};
use crate::xml::{Content, Node, is_xml_ws};

/// One spine position's resource.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SpineDocument {
    /// The spine position.
    pub index: usize,
    /// The manifest id the spine names there.
    pub idref: String,
    /// The resource's archive path, as [`ManifestItem::path`](crate::ManifestItem::path).
    pub path: String,
    /// Its declared media type.
    pub media_type: String,
    /// Its bytes.
    pub bytes: Vec<u8>,
}

/// What kind of block a piece of text sat in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BlockKind {
    /// `h1`–`h6`; `level` is 1–6.
    Heading {
        /// The heading level.
        level: u8,
    },
    /// `p`.
    Paragraph,
    /// `li`, and a paragraph or `div` inside one.
    ListItem,
    /// `blockquote`, and a paragraph or `div` inside one.
    Blockquote,
    /// `pre`: its text keeps its whitespace.
    Preformatted,
    /// Any other block (`div`, `section`, table cells, `dd`, …), and text
    /// that sits directly in `body`.
    Other,
}

/// One block of a chapter's text.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Block {
    /// What it was.
    pub kind: BlockKind,
    /// Its text: XML whitespace collapsed to single spaces and trimmed, a
    /// `<br>` kept as `\n`. Inside `pre`, the text exactly as written (less
    /// one leading newline). Never empty.
    pub text: String,
}

/// Media types read as markup. Everything else is refused as [`Error::NotText`].
fn is_markup(media_type: &str) -> bool {
    let m = media_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    m == "application/xhtml+xml"
        || m == "text/html"
        || m == "application/xml"
        || m == "text/xml"
        || m == "application/x-dtbook+xml"
        || m == "image/svg+xml"
        || (m.ends_with("+xml") && !m.starts_with("application/x-dtbncx"))
}

impl<R: Read + Seek> Package<R> {
    /// The resource at this spine position.
    pub fn spine_document(&mut self, index: usize) -> Result<SpineDocument, Error> {
        let len = self.spine().items.len();
        let idref = self
            .spine()
            .items
            .get(index)
            .ok_or(Error::SpineIndexOutOfRange { index, len })?
            .idref
            .clone();
        let item = self
            .manifest_item(&idref)
            .ok_or_else(|| Error::UnknownId { id: idref.clone() })?;
        let (path, media_type) = (item.path.clone(), item.media_type.clone());
        let bytes = self.resource_by_path(&path)?;
        Ok(SpineDocument {
            index,
            idref,
            path,
            media_type,
            bytes,
        })
    }

    /// Every spine position's resource, in reading order. Each read is
    /// separate: one that fails is an `Err` and the next is still tried.
    pub fn spine_documents(&mut self) -> SpineDocuments<'_, R> {
        SpineDocuments {
            package: self,
            next: 0,
        }
    }

    /// The text of the resource at this spine position, in blocks.
    ///
    /// Refused as [`Error::NotText`] when its media type is not markup, and
    /// as [`Error::LimitExceeded`] past a size, depth, attribute or node
    /// cap. A document that is not well-formed is read as far as it can be,
    /// with a [`Warning::MarkupRecovered`](crate::Warning::MarkupRecovered).
    pub fn spine_text(&mut self, index: usize) -> Result<Vec<Block>, Error> {
        let doc = self.spine_document(index)?;
        if !is_markup(&doc.media_type) {
            return Err(Error::NotText {
                id: doc.idref,
                media_type: doc.media_type,
            });
        }
        let limits = *self.limits();
        let mut warnings = Vec::new();
        let parsed = parse_html(&doc.bytes, &doc.path, &limits, &mut warnings);
        for w in warnings {
            self.warn(w);
        }
        Ok(blocks(&parsed?.root))
    }
}

/// The iterator [`Package::spine_documents`] returns.
pub struct SpineDocuments<'a, R> {
    package: &'a mut Package<R>,
    next: usize,
}

impl<R> std::fmt::Debug for SpineDocuments<'_, R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpineDocuments")
            .field("next", &self.next)
            .finish_non_exhaustive()
    }
}

impl<R: Read + Seek> Iterator for SpineDocuments<'_, R> {
    type Item = Result<SpineDocument, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.package.spine().items.len() {
            return None;
        }
        let index = self.next;
        self.next += 1;
        Some(self.package.spine_document(index))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let left = self.package.spine().items.len().saturating_sub(self.next);
        (left, Some(left))
    }
}

impl<R: Read + Seek> ExactSizeIterator for SpineDocuments<'_, R> {}
impl<R: Read + Seek> FusedIterator for SpineDocuments<'_, R> {}

/// What an element means to the text.
enum Role {
    /// Contributes nothing.
    Skip,
    /// A line break.
    Break,
    /// Starts and ends a block of this kind.
    Block(BlockKind),
    /// Text flows through it.
    Inline,
}

fn role(name: &str) -> Role {
    match name {
        "head" | "script" | "style" | "template" | "rt" | "rp" => Role::Skip,
        "br" => Role::Break,
        "p" => Role::Block(BlockKind::Paragraph),
        "li" => Role::Block(BlockKind::ListItem),
        "blockquote" => Role::Block(BlockKind::Blockquote),
        "pre" => Role::Block(BlockKind::Preformatted),
        "h1" => Role::Block(BlockKind::Heading { level: 1 }),
        "h2" => Role::Block(BlockKind::Heading { level: 2 }),
        "h3" => Role::Block(BlockKind::Heading { level: 3 }),
        "h4" => Role::Block(BlockKind::Heading { level: 4 }),
        "h5" => Role::Block(BlockKind::Heading { level: 5 }),
        "h6" => Role::Block(BlockKind::Heading { level: 6 }),
        "address" | "article" | "aside" | "body" | "caption" | "center" | "dd" | "details"
        | "dialog" | "div" | "dl" | "dt" | "fieldset" | "figcaption" | "figure" | "footer"
        | "form" | "header" | "hgroup" | "hr" | "main" | "nav" | "ol" | "section" | "summary"
        | "table" | "tbody" | "td" | "tfoot" | "th" | "thead" | "tr" | "ul" => {
            Role::Block(BlockKind::Other)
        }
        _ => Role::Inline,
    }
}

struct Extract {
    blocks: Vec<Block>,
    buf: String,
    /// The open blocks' kinds, innermost last.
    open: Vec<BlockKind>,
    /// How many of them are `pre`: a counter, so asking costs nothing per
    /// piece of text however deep the nesting.
    pre: usize,
}

impl Extract {
    fn kind(&self) -> BlockKind {
        self.open.last().copied().unwrap_or(BlockKind::Other)
    }

    fn in_pre(&self) -> bool {
        self.pre > 0
    }

    fn text(&mut self, t: &str) {
        if self.in_pre() {
            self.buf.push_str(t);
            return;
        }
        for c in t.chars() {
            if is_xml_ws(c) {
                if !self.buf.is_empty() && !self.buf.ends_with([' ', '\n']) {
                    self.buf.push(' ');
                }
            } else {
                self.buf.push(c);
            }
        }
    }

    fn line_break(&mut self) {
        if !self.in_pre() {
            while self.buf.ends_with(' ') {
                self.buf.pop();
            }
        }
        self.buf.push('\n');
    }

    /// The text gathered so far becomes a block of the current kind.
    fn flush(&mut self) {
        let kind = self.kind();
        let raw = std::mem::take(&mut self.buf);
        let text = if kind == BlockKind::Preformatted {
            let t = raw.strip_prefix('\n').unwrap_or(&raw);
            if t.chars().all(char::is_whitespace) {
                return;
            }
            t.to_string()
        } else {
            raw.trim_matches([' ', '\n']).to_string()
        };
        if !text.is_empty() {
            self.blocks.push(Block { kind, text });
        }
    }

    fn walk(&mut self, node: &Node) {
        let kind = match role(&node.name) {
            Role::Skip => return,
            Role::Break => {
                self.line_break();
                return;
            }
            Role::Inline => {
                self.children(node);
                return;
            }
            Role::Block(kind) => kind,
        };
        // A paragraph or a plain block inside a list item or a quotation
        // is part of that item or quotation.
        let kind = match (kind, self.open.last()) {
            (
                BlockKind::Paragraph | BlockKind::Other,
                Some(outer @ (BlockKind::ListItem | BlockKind::Blockquote)),
            ) => *outer,
            _ => kind,
        };
        let pre = usize::from(kind == BlockKind::Preformatted);
        self.flush();
        self.open.push(kind);
        self.pre += pre;
        self.children(node);
        self.flush();
        self.pre -= pre;
        self.open.pop();
    }

    fn children(&mut self, node: &Node) {
        for c in &node.content {
            match c {
                Content::Text(t) | Content::CData(t) => self.text(t),
                Content::Element(n) => self.walk(n),
            }
        }
    }
}

/// A document's text, in blocks: from `body` when there is one, else from
/// the root.
fn blocks(root: &Node) -> Vec<Block> {
    let start = if root.name == "body" {
        Some(root)
    } else {
        root.find("body")
    };
    let mut x = Extract {
        blocks: Vec::new(),
        buf: String::new(),
        open: Vec::new(),
        pre: 0,
    };
    match start {
        Some(body) => x.walk(body),
        None => x.walk(root),
    }
    x.flush();
    x.blocks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{Limit, MarkupProblem};
    use crate::testkit::{container, opf, zip_of};
    use crate::{Limits, Warning};

    fn book(chapters: &[(&str, &str, &[u8])]) -> Vec<u8> {
        let mut manifest = String::new();
        let mut spine = String::from("<spine>");
        for (i, (href, media, _)) in chapters.iter().enumerate() {
            manifest.push_str(&format!(
                r#"<item id="c{i}" href="{href}" media-type="{media}"/>"#
            ));
            spine.push_str(&format!(r#"<itemref idref="c{i}"/>"#));
        }
        spine.push_str("</spine>");
        let opf = opf("3.0", "<dc:title>T</dc:title>", &manifest, &spine);
        let c = container("OEBPS/content.opf");
        let names: Vec<String> = chapters
            .iter()
            .map(|(h, _, _)| format!("OEBPS/{h}"))
            .collect();
        let mut entries: Vec<(&str, &[u8])> = vec![
            ("META-INF/container.xml", c.as_bytes()),
            ("OEBPS/content.opf", opf.as_bytes()),
        ];
        for (name, (_, _, bytes)) in names.iter().zip(chapters) {
            entries.push((name.as_str(), bytes));
        }
        zip_of(&entries)
    }

    const XHTML: &str = "application/xhtml+xml";

    fn text_of(xhtml: &str) -> (Vec<(BlockKind, String)>, Vec<Warning>) {
        let bytes = book(&[("c.xhtml", XHTML, xhtml.as_bytes())]);
        let mut p = Package::from_bytes(&bytes).unwrap();
        let blocks = p.spine_text(0).unwrap();
        (
            blocks.into_iter().map(|b| (b.kind, b.text)).collect(),
            p.warnings().to_vec(),
        )
    }

    #[test]
    fn reads_blocks_of_a_chapter() {
        let (blocks, warnings) = text_of(
            r#"<?xml version="1.0" encoding="utf-8"?>
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Not text</title><style>p { x }</style></head>
<body>
  <h2 class="c">Chapter&nbsp;I.
     The   Beginning</h2>
  <p>It was a <em>dark</em> and
     stormy night&mdash;<script>alert(1)</script>the rain fell.</p>
  <blockquote><p>Quoted <i>words</i>.</p></blockquote>
  <ul><li>one</li><li><p>two</p></li></ul>
  <pre>
  keep   this
    as is</pre>
  <div class="poem">Line one<br/>
     Line two</div>
  loose text
</body></html>"#,
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            blocks,
            vec![
                (
                    BlockKind::Heading { level: 2 },
                    "Chapter\u{a0}I. The Beginning".into()
                ),
                (
                    BlockKind::Paragraph,
                    "It was a dark and stormy night—the rain fell.".into()
                ),
                (BlockKind::Blockquote, "Quoted words.".into()),
                (BlockKind::ListItem, "one".into()),
                (BlockKind::ListItem, "two".into()),
                (BlockKind::Preformatted, "  keep   this\n    as is".into()),
                (BlockKind::Other, "Line one\nLine two".into()),
                (BlockKind::Other, "loose text".into()),
            ]
        );
    }

    /// The XHTML real books carry: upper-case tags, no namespace, `<br>`
    /// left open, a stray `&`, a skipped end tag, and a file that stops.
    #[test]
    fn tolerates_sloppy_xhtml_and_says_so() {
        let (blocks, warnings) = text_of(
            "<HTML><BODY><P>Fish & chips<BR>and peas</P><DIV><B>bold</DIV><P>last &copy; one",
        );
        assert_eq!(
            blocks,
            vec![
                (BlockKind::Paragraph, "Fish & chips\nand peas".into()),
                (BlockKind::Other, "bold".into()),
                (BlockKind::Paragraph, "last © one".into()),
            ]
        );
        let problems: Vec<MarkupProblem> = warnings
            .iter()
            .filter_map(|w| match w {
                Warning::MarkupRecovered { problem, .. } => Some(*problem),
                _ => None,
            })
            .collect();
        assert_eq!(
            problems,
            vec![MarkupProblem::MismatchedEnd, MarkupProblem::Unclosed]
        );
    }

    #[test]
    fn a_document_that_breaks_off_keeps_what_came_before() {
        let (blocks, warnings) = text_of("<html><body><p>first</p><p>second</p><p class=\"");
        assert_eq!(
            blocks.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>(),
            vec!["first", "second"]
        );
        assert!(warnings.iter().any(|w| matches!(
            w,
            Warning::MarkupRecovered {
                problem: MarkupProblem::Truncated,
                ..
            }
        )));
    }

    #[test]
    fn unknown_entities_stay_literal_and_are_reported() {
        let (blocks, warnings) = text_of("<html><body><p>A &hellip; B &weird; C</p></body></html>");
        assert_eq!(blocks[0].1, "A … B &weird; C");
        assert!(warnings.contains(&Warning::UndeclaredEntity {
            path: "OEBPS/c.xhtml".into(),
            name: "weird".into()
        }));
    }

    #[test]
    fn spine_documents_in_order_and_errors_by_position() {
        let bytes = book(&[
            ("a.xhtml", XHTML, b"<html><body><p>A</p></body></html>"),
            ("b.png", "image/png", b"\x89PNG"),
        ]);
        let mut p = Package::from_bytes(&bytes).unwrap();
        let docs: Vec<SpineDocument> = p.spine_documents().map(Result::unwrap).collect();
        assert_eq!(docs.len(), 2);
        assert_eq!((docs[0].index, docs[0].path.as_str()), (0, "OEBPS/a.xhtml"));
        assert_eq!(docs[1].media_type, "image/png");
        assert!(matches!(p.spine_text(1), Err(Error::NotText { .. })));
        assert!(matches!(
            p.spine_text(2),
            Err(Error::SpineIndexOutOfRange { index: 2, len: 2 })
        ));
        assert_eq!(p.spine_text(0).unwrap()[0].text, "A");
    }

    #[test]
    fn content_is_bounded_by_size_depth_and_nodes() {
        let big = format!("<html><body>{}</body></html>", "<p>x</p>".repeat(10_000));
        let deep = format!(
            "<html><body>{}x{}</body></html>",
            "<div>".repeat(300),
            "</div>".repeat(300)
        );
        let bytes = book(&[
            ("big.xhtml", XHTML, big.as_bytes()),
            ("deep.xhtml", XHTML, deep.as_bytes()),
        ]);
        let limits = Limits::default().with_max_xml_nodes(5_000);
        let mut p = Package::from_bytes_with(&bytes, limits).unwrap();
        assert!(matches!(
            p.spine_text(0),
            Err(Error::LimitExceeded {
                limit: Limit::XmlNodes,
                ..
            })
        ));
        assert!(matches!(
            p.spine_text(1),
            Err(Error::LimitExceeded {
                limit: Limit::XmlDepth,
                ..
            })
        ));
        let small = Limits::default().with_max_xml_bytes(1024);
        let mut p = Package::from_bytes_with(&bytes, small).unwrap();
        assert!(matches!(
            p.spine_text(0),
            Err(Error::LimitExceeded {
                limit: Limit::XmlBytes,
                ..
            })
        ));
        let entry = Limits::default().with_max_entry_bytes(1024);
        let mut p = Package::from_bytes_with(&bytes, entry).unwrap();
        assert!(matches!(
            p.spine_document(0),
            Err(Error::LimitExceeded {
                limit: Limit::EntryBytes,
                ..
            })
        ));
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use crate::Limits;
    use crate::testkit::esc;
    use proptest::prelude::*;

    proptest! {
        /// Arbitrary bytes as a chapter never panic.
        #[test]
        fn arbitrary_content_never_panics(
            bytes in proptest::collection::vec(any::<u8>(), 0..1024),
            markup in "[<>/&;#a-zA-Z0-9 \"'=!?\\n-]{0,300}",
        ) {
            for doc in [bytes.as_slice(), markup.as_bytes()] {
                if let Ok(d) = crate::xml::parse_html(doc, "t", &Limits::default()) {
                    for b in blocks(&d.root) {
                        prop_assert!(!b.text.is_empty());
                    }
                }
            }
        }

        /// Paragraphs written out come back as paragraphs, in order, their
        /// whitespace collapsed.
        #[test]
        fn paragraphs_round_trip(paras in proptest::collection::vec("[A-Za-z0-9é&<>'\",.]{1,10}( {1,3}[A-Za-z0-9é&<>]{1,8}){0,4}", 1..12)) {
            let body: String = paras.iter().map(|p| format!("<p>\n  {}\n</p>\n", esc(p))).collect();
            let doc = format!("<html xmlns=\"http://www.w3.org/1999/xhtml\"><body>{body}</body></html>");
            let d = crate::xml::parse_html(doc.as_bytes(), "t", &Limits::default()).unwrap();
            prop_assert!(d.problems.is_empty());
            let got: Vec<Block> = blocks(&d.root);
            let want: Vec<String> = paras.iter().map(|p| crate::xml::collapse(p)).collect();
            prop_assert_eq!(got.iter().map(|b| b.text.clone()).collect::<Vec<_>>(), want);
            prop_assert!(got.iter().all(|b| b.kind == BlockKind::Paragraph));
        }
    }
}
