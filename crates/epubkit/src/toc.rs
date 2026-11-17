//! Tables of contents: the EPUB 3 navigation document and the EPUB 2 NCX.
//!
//! Both read into the same tree of [`TocNode`]s. [`Package::nav`] and
//! [`Package::ncx`] give the whole of one document — its table of contents,
//! and the landmarks and page list beside it; [`Package::toc`] picks the
//! better of the two. Either document may be missing or broken: that is a
//! [`Warning`] and an empty answer, never an error. Nesting is bounded by
//! [`Limits::max_xml_depth`](crate::Limits::max_xml_depth), since a level of
//! the tree is at least one element of the document.

use std::io::{Read, Seek};
use std::iter::FusedIterator;

use crate::error::{Error, TocProblem, Warning};
use crate::package::{Package, parse_html, parse_xml};
use crate::path;
use crate::xml::{Document, Node, collapse};

/// One entry of a table of contents, landmarks list or page list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct TocNode {
    /// The label: the text of the NCX `navLabel`, or of the navigation
    /// document's `a`/`span`, with XML whitespace collapsed and trimmed
    /// (U+00A0 is kept). An image-only label falls back to the image's `alt`,
    /// then a `title`. Empty when the book gives none.
    pub label: String,
    /// The link as written: NCX `content@src`, or the `a`'s `href`. `None`
    /// for an unlinked heading (a navigation document's `span`, or a
    /// `navPoint` with no `content`).
    pub href: Option<String>,
    /// The archive path `href` points into, without its fragment: resolved
    /// against the directory of the document that holds the link, as the
    /// spec says (an NCX's links against the NCX, a navigation document's
    /// against it), by textual join like every other path here — not
    /// percent-decoded and not dot-folded. A link that is only a fragment
    /// (`#notes`) points into its own document. `None` with no `href`, and
    /// for an absolute URL (`https://…`), which is not inside the book.
    pub path: Option<String>,
    /// The fragment after `#`, without the `#`.
    pub fragment: Option<String>,
    /// NCX `playOrder`, when present and a number.
    pub play_order: Option<usize>,
    /// The link's `epub:type` (a landmark's `bodymatter`, `toc`, `cover`…),
    /// or an NCX `pageTarget`'s `type`.
    pub epub_type: Option<String>,
    /// Nested entries, in reading order.
    pub children: Vec<TocNode>,
}

impl TocNode {
    /// `path#fragment`, or `path` alone — the place this entry points at,
    /// as one string. `None` without a `path`.
    pub fn target(&self) -> Option<String> {
        let path = self.path.as_deref()?;
        Some(match &self.fragment {
            Some(f) => format!("{path}#{f}"),
            None => path.to_string(),
        })
    }
}

/// Which document a table of contents came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TocSource {
    /// The EPUB 3 navigation document (`properties="nav"`).
    Nav,
    /// The EPUB 2 NCX (`application/x-dtbncx+xml`).
    Ncx,
}

/// Everything one navigation document says.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Navigation {
    /// Which document this is.
    pub source: TocSource,
    /// The NCX's `docTitle`, or the heading of the navigation document's
    /// table-of-contents `nav`.
    pub title: Option<String>,
    /// The table of contents.
    pub toc: Vec<TocNode>,
    /// EPUB 3 `landmarks` (cover, start of the text, …); empty for an NCX.
    pub landmarks: Vec<TocNode>,
    /// The print edition's pages: EPUB 3 `page-list`, or NCX `pageList`.
    pub page_list: Vec<TocNode>,
}

/// A table of contents and the document it came from; see [`Package::toc`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Toc {
    /// Which document it came from.
    pub source: TocSource,
    /// The entries, in reading order. Never empty.
    pub entries: Vec<TocNode>,
}

impl Toc {
    /// Every entry with its depth, in reading order; see [`flatten`].
    pub fn iter(&self) -> Flatten<'_> {
        flatten(&self.entries)
    }
}

/// Walk a tree of entries in reading order — each entry, then its children —
/// yielding `(depth, entry)`, depth 0 at the top. Iterative, so it costs no
/// stack however deep the tree.
pub fn flatten(nodes: &[TocNode]) -> Flatten<'_> {
    Flatten {
        stack: vec![(0, nodes.iter())],
    }
}

/// The iterator [`flatten`] returns.
#[derive(Debug, Clone)]
pub struct Flatten<'a> {
    stack: Vec<(usize, std::slice::Iter<'a, TocNode>)>,
}

impl<'a> Iterator for Flatten<'a> {
    type Item = (usize, &'a TocNode);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let (depth, iter) = self.stack.last_mut()?;
            let depth = *depth;
            match iter.next() {
                Some(node) => {
                    if !node.children.is_empty() {
                        self.stack.push((depth + 1, node.children.iter()));
                    }
                    return Some((depth, node));
                }
                None => {
                    self.stack.pop();
                }
            }
        }
    }
}

impl FusedIterator for Flatten<'_> {}

/// The NCX media type.
const NCX_MEDIA_TYPE: &str = "application/x-dtbncx+xml";

impl<R: Read + Seek> Package<R> {
    /// The table of contents, from the best document the book has.
    ///
    /// The EPUB 3 navigation document is preferred when it yields at least
    /// one entry — it is the normative one in EPUB 3, and an NCX beside it is
    /// there for older reading systems. Otherwise the NCX, when it yields at
    /// least one. `None` when neither does. Each call reads the documents
    /// again.
    pub fn toc(&mut self) -> Option<Toc> {
        let nav = self.nav_toc();
        if !nav.is_empty() {
            return Some(Toc {
                source: TocSource::Nav,
                entries: nav,
            });
        }
        let ncx = self.ncx_toc();
        (!ncx.is_empty()).then_some(Toc {
            source: TocSource::Ncx,
            entries: ncx,
        })
    }

    /// The EPUB 3 navigation document's table of contents; empty without
    /// one. See [`Package::nav`].
    pub fn nav_toc(&mut self) -> Vec<TocNode> {
        self.nav().map(|n| n.toc).unwrap_or_default()
    }

    /// The NCX's table of contents; empty without one. See [`Package::ncx`].
    pub fn ncx_toc(&mut self) -> Vec<TocNode> {
        self.ncx().map(|n| n.toc).unwrap_or_default()
    }

    /// The EPUB 3 navigation document: the first manifest item whose
    /// `properties` include `nav`, read leniently as XHTML.
    ///
    /// Its table of contents is the `nav` whose `epub:type` includes `toc`
    /// (failing that, the one with `id="toc"`, failing that the first `nav`
    /// that is neither landmarks nor a page list): its `ol` of `li`, each
    /// labelled by an `a` (a link) or a `span` (an unlinked heading), nested
    /// by an `ol` inside the `li`. `landmarks` and `page-list` navs are read
    /// the same way. Links resolve against the navigation document's own
    /// directory.
    ///
    /// `None` when the book has no navigation document, or when it cannot be
    /// read — which also records a [`Warning::NavUnreadable`].
    pub fn nav(&mut self) -> Option<Navigation> {
        let id = self
            .manifest()
            .iter()
            .find(|m| m.has_property("nav"))?
            .id
            .clone();
        match self.read_toc_document(&id, true) {
            Ok((path, doc)) => match read_nav(&doc.root, &path) {
                Some(nav) => Some(nav),
                None => {
                    self.warn(Warning::NavUnreadable {
                        id,
                        problem: TocProblem::NoNav,
                    });
                    None
                }
            },
            Err(problem) => {
                self.warn(Warning::NavUnreadable { id, problem });
                None
            }
        }
    }

    /// The EPUB 2 NCX: the manifest item the spine's `toc` attribute names,
    /// or — when there is no such attribute — the first item of the NCX
    /// media type.
    ///
    /// Each `navPoint` becomes a node, with its `navLabel`'s text, its
    /// `content@src` resolved against the NCX's own directory, and its
    /// `playOrder`. A `navPoint` without a `playOrder` is kept. Siblings are
    /// put in `playOrder` order when **every** one of them has a numeric
    /// `playOrder` (a stable sort: equal ones keep document order), and kept
    /// in document order otherwise. `docTitle` and `pageList` are read too.
    ///
    /// `None` when the book has no NCX, or when it cannot be read — which
    /// also records a [`Warning::NcxUnreadable`].
    pub fn ncx(&mut self) -> Option<Navigation> {
        let id = match self.spine().toc.clone() {
            Some(id) => id,
            None => self
                .manifest()
                .iter()
                .find(|m| m.media_type == NCX_MEDIA_TYPE)?
                .id
                .clone(),
        };
        match self
            .read_toc_document(&id, false)
            .and_then(|(path, doc)| read_ncx(&doc.root, &path))
        {
            Ok(nav) => Some(nav),
            Err(problem) => {
                self.warn(Warning::NcxUnreadable { id, problem });
                None
            }
        }
    }

    /// Read and parse a navigation document by manifest id; its warnings go
    /// on the package.
    fn read_toc_document(
        &mut self,
        id: &str,
        html: bool,
    ) -> Result<(String, Document), TocProblem> {
        let path = self
            .manifest_item(id)
            .ok_or(TocProblem::NotInManifest)?
            .path
            .clone();
        let limits = *self.limits();
        let bytes = self.archive_mut().read(&path).map_err(|e| match e {
            Error::LimitExceeded { .. } => TocProblem::LimitExceeded,
            _ => TocProblem::Unreadable,
        })?;
        let mut warnings = Vec::new();
        let parsed = if html {
            parse_html(&bytes, &path, &limits, &mut warnings)
        } else {
            parse_xml(&bytes, &path, &limits, &mut warnings)
        };
        let doc = parsed.map_err(|e| match e {
            Error::LimitExceeded { .. } => TocProblem::LimitExceeded,
            _ => TocProblem::Malformed,
        })?;
        for w in warnings {
            self.warn(w);
        }
        Ok((path, doc))
    }
}

/// A link resolved against the document that holds it.
fn resolve(doc_path: &str, href: Option<&str>) -> (Option<String>, Option<String>) {
    let Some(href) = href else {
        return (None, None);
    };
    let (file, fragment) = match href.split_once('#') {
        Some((file, fragment)) => (file, Some(fragment.to_string())),
        None => (href, None),
    };
    if path::has_scheme(file) {
        return (None, fragment);
    }
    let resolved = if file.is_empty() {
        doc_path.to_string()
    } else {
        path::join(&path::parent_dir(doc_path), file)
    };
    (Some(resolved), fragment)
}

fn read_ncx(root: &Node, ncx_path: &str) -> Result<Navigation, TocProblem> {
    let map = root.find("navMap").ok_or(TocProblem::NoNavMap)?;
    let title = root
        .find("docTitle")
        .and_then(label_of_ncx)
        .filter(|t| !t.is_empty());
    let page_list = root
        .find("pageList")
        .map(|list| {
            list.children()
                .filter(|n| n.name == "pageTarget")
                .map(|t| {
                    let mut node = ncx_node(t, ncx_path);
                    node.epub_type = t.attr("type").map(str::to_string);
                    node
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Navigation {
        source: TocSource::Ncx,
        title,
        toc: nav_points(map, ncx_path),
        landmarks: Vec::new(),
        page_list,
    })
}

/// The text of a `navLabel` or `docTitle`: its `text` child (or, failing
/// that, its first child), collapsed.
fn label_of_ncx(label: &Node) -> Option<String> {
    label
        .child("text")
        .or_else(|| label.children().next())
        .map(|t| collapse(&t.text_content()))
}

fn ncx_node(item: &Node, ncx_path: &str) -> TocNode {
    let href = item.child("content").and_then(|c| c.attr("src"));
    let (path, fragment) = resolve(ncx_path, href);
    TocNode {
        label: item
            .child("navLabel")
            .and_then(label_of_ncx)
            .unwrap_or_default(),
        href: href.map(str::to_string),
        path,
        fragment,
        // Parsed exactly as the old crate did (no trimming), so a book keeps
        // the order it had.
        play_order: item.attr("playOrder").and_then(|n| n.parse::<usize>().ok()),
        epub_type: None,
        children: Vec::new(),
    }
}

fn nav_points(parent: &Node, ncx_path: &str) -> Vec<TocNode> {
    let mut out: Vec<TocNode> = parent
        .children()
        .filter(|n| n.name == "navPoint")
        .map(|item| {
            let mut node = ncx_node(item, ncx_path);
            node.children = nav_points(item, ncx_path);
            node
        })
        .collect();
    if out.iter().all(|p| p.play_order.is_some()) {
        // Stable: equal playOrders keep document order.
        out.sort_by_key(|p| p.play_order);
    }
    out
}

fn types(n: &Node) -> impl Iterator<Item = &str> {
    n.attr("type").unwrap_or_default().split_ascii_whitespace()
}

fn read_nav(root: &Node, nav_path: &str) -> Option<Navigation> {
    let navs = root.find_all("nav");
    if navs.is_empty() {
        return None;
    }
    let has = |n: &&Node, t: &str| types(n).any(|x| x == t);
    let toc = navs
        .iter()
        .find(|n| has(n, "toc"))
        .or_else(|| navs.iter().find(|n| n.attr("id") == Some("toc")))
        .or_else(|| {
            navs.iter()
                .find(|n| !has(n, "landmarks") && !has(n, "page-list"))
        });
    let list = |n: &Node| {
        n.child("ol")
            .or_else(|| n.find("ol"))
            .map(|ol| list_items(ol, nav_path))
            .unwrap_or_default()
    };
    let title = toc.and_then(|n| {
        n.children()
            .find(|c| matches!(c.name.as_str(), "h1" | "h2" | "h3" | "h4" | "h5" | "h6"))
            .map(|h| collapse(&h.text_content()))
            .filter(|t| !t.is_empty())
    });
    Some(Navigation {
        source: TocSource::Nav,
        title,
        toc: toc.map(|n| list(n)).unwrap_or_default(),
        landmarks: navs
            .iter()
            .find(|n| has(n, "landmarks"))
            .map(|n| list(n))
            .unwrap_or_default(),
        page_list: navs
            .iter()
            .find(|n| has(n, "page-list"))
            .map(|n| list(n))
            .unwrap_or_default(),
    })
}

fn list_items(ol: &Node, nav_path: &str) -> Vec<TocNode> {
    let mut out = Vec::new();
    for li in ol.children().filter(|n| n.name == "li") {
        let label_el = li.children().find(|c| c.name == "a" || c.name == "span");
        let href = label_el
            .filter(|e| e.name == "a")
            .and_then(|a| a.attr("href"));
        let (path, fragment) = resolve(nav_path, href);
        let children = li
            .child("ol")
            .map(|ol| list_items(ol, nav_path))
            .unwrap_or_default();
        let label = label_el.map(label_of_nav).unwrap_or_default();
        if label.is_empty() && href.is_none() && children.is_empty() {
            continue;
        }
        out.push(TocNode {
            label,
            href: href.map(str::to_string),
            path,
            fragment,
            play_order: None,
            epub_type: label_el.and_then(|e| e.attr("type")).map(str::to_string),
            children,
        });
    }
    out
}

/// A navigation label: its text, else an image's `alt`, else a `title`.
fn label_of_nav(el: &Node) -> String {
    let text = collapse(&el.text_content());
    if !text.is_empty() {
        return text;
    }
    let img = el.find_all("img").into_iter().next();
    [
        img.and_then(|i| i.attr("alt")),
        el.attr("title"),
        img.and_then(|i| i.attr("title")),
    ]
    .into_iter()
    .flatten()
    .map(collapse)
    .find(|s| !s.is_empty())
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::MarkupProblem;
    use crate::testkit::{container, opf, zip_of};
    use crate::{Limits, Warning};

    const CH: &str = r#"<item id="ch1" href="text/ch1.xhtml" media-type="application/xhtml+xml"/><item id="ch2" href="text/ch2.xhtml" media-type="application/xhtml+xml"/>"#;
    const SPINE: &str = r#"<itemref idref="ch1"/><itemref idref="ch2"/>"#;

    /// A book with a navigation document at `OEBPS/nav/nav.xhtml` and/or an
    /// NCX at `OEBPS/toc.ncx`.
    fn book(nav: Option<&str>, ncx: Option<&str>) -> Vec<u8> {
        let mut manifest = CH.to_string();
        if nav.is_some() {
            manifest.push_str(r#"<item id="nav" href="nav/nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>"#);
        }
        let spine = if ncx.is_some() {
            manifest.push_str(
                r#"<item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>"#,
            );
            format!(r#"<spine toc="ncx">{SPINE}</spine>"#)
        } else {
            format!("<spine>{SPINE}</spine>")
        };
        let opf = opf("3.0", "<dc:title>T</dc:title>", &manifest, &spine);
        let c = container("OEBPS/content.opf");
        let mut entries: Vec<(&str, &[u8])> = vec![
            ("META-INF/container.xml", c.as_bytes()),
            ("OEBPS/content.opf", opf.as_bytes()),
            ("OEBPS/text/ch1.xhtml", b"<html/>"),
            ("OEBPS/text/ch2.xhtml", b"<html/>"),
        ];
        if let Some(n) = nav {
            entries.push(("OEBPS/nav/nav.xhtml", n.as_bytes()));
        }
        if let Some(n) = ncx {
            entries.push(("OEBPS/toc.ncx", n.as_bytes()));
        }
        zip_of(&entries)
    }

    const NAV: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
<head><title>Nav</title></head>
<body>
  <nav epub:type="toc" id="toc">
    <h1>Contents</h1>
    <ol>
      <li><a href="../text/ch1.xhtml">Chapter
            One&nbsp;&mdash; Start</a></li>
      <li><span>Part Two</span>
        <ol>
          <li><a href="../text/ch2.xhtml#s1"><em>Two</em>, first</a></li>
          <li><a href="../text/ch2.xhtml#s2"><img src="x.png" alt="Pictured"/></a></li>
        </ol>
      </li>
      <li><a href="https://example.com/more">Online</a></li>
    </ol>
  </nav>
  <nav epub:type="landmarks" hidden="">
    <ol><li><a epub:type="bodymatter" href="../text/ch1.xhtml">Start</a></li></ol>
  </nav>
  <nav epub:type="page-list"><ol><li><a href="../text/ch1.xhtml#p1">1</a></li></ol></nav>
</body>
</html>"#;

    #[test]
    fn reads_the_navigation_document() {
        let bytes = book(Some(NAV), None);
        let mut p = Package::from_bytes(&bytes).unwrap();
        let nav = p.nav().unwrap();
        assert!(p.warnings().is_empty(), "{:?}", p.warnings());
        assert_eq!(nav.title.as_deref(), Some("Contents"));
        let flat: Vec<(usize, &str, Option<&str>, Option<&str>)> = flatten(&nav.toc)
            .map(|(d, n)| {
                (
                    d,
                    n.label.as_str(),
                    n.path.as_deref(),
                    n.fragment.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            flat,
            vec![
                (
                    0,
                    "Chapter One\u{a0}— Start",
                    Some("OEBPS/nav/../text/ch1.xhtml"),
                    None
                ),
                (0, "Part Two", None, None),
                (
                    1,
                    "Two, first",
                    Some("OEBPS/nav/../text/ch2.xhtml"),
                    Some("s1")
                ),
                (
                    1,
                    "Pictured",
                    Some("OEBPS/nav/../text/ch2.xhtml"),
                    Some("s2")
                ),
                (0, "Online", None, None),
            ]
        );
        assert_eq!(nav.toc[2].href.as_deref(), Some("https://example.com/more"));
        // A link out of the nav's own directory still finds its spine item.
        let spine: Vec<Option<usize>> = flatten(&nav.toc)
            .map(|(_, n)| n.path.as_deref().and_then(|x| p.spine_index_of_path(x)))
            .collect();
        assert_eq!(spine, vec![Some(0), None, Some(1), Some(1), None]);
        assert_eq!(nav.landmarks.len(), 1);
        assert_eq!(nav.landmarks[0].epub_type.as_deref(), Some("bodymatter"));
        assert_eq!(
            nav.page_list[0].target().as_deref(),
            Some("OEBPS/nav/../text/ch1.xhtml#p1")
        );
        let toc = p.toc().unwrap();
        assert_eq!(toc.source, TocSource::Nav);
        assert_eq!(toc.iter().count(), 5);
    }

    const NCX: &str = r#"<?xml version="1.0"?>
<!DOCTYPE ncx PUBLIC "-//NISO//DTD ncx 2005-1//EN" "http://www.daisy.org/z3986/2005/ncx-2005-1.dtd">
<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"><docTitle><text>The Book</text></docTitle><navMap>
  <navPoint id="b" playOrder="3"><navLabel><text>Three</text></navLabel><content src="text/ch2.xhtml#c"/></navPoint>
  <navPoint id="a" playOrder="1"><navLabel><text> One </text></navLabel><content src="text/ch1.xhtml"/>
    <navPoint id="a1" playOrder="2"><navLabel><text>One.1</text></navLabel><content src="text/ch1.xhtml#x"/></navPoint>
  </navPoint>
</navMap>
<pageList><pageTarget type="normal" value="1"><navLabel><text>1</text></navLabel><content src="text/ch1.xhtml#p1"/></pageTarget></pageList>
</ncx>"#;

    #[test]
    fn reads_the_ncx_by_play_order() {
        let bytes = book(None, Some(NCX));
        let mut p = Package::from_bytes(&bytes).unwrap();
        let ncx = p.ncx().unwrap();
        assert_eq!(ncx.title.as_deref(), Some("The Book"));
        let flat: Vec<(usize, &str, Option<String>, Option<usize>)> = flatten(&ncx.toc)
            .map(|(d, n)| (d, n.label.as_str(), n.target(), n.play_order))
            .collect();
        assert_eq!(
            flat,
            vec![
                (0, "One", Some("OEBPS/text/ch1.xhtml".into()), Some(1)),
                (1, "One.1", Some("OEBPS/text/ch1.xhtml#x".into()), Some(2)),
                (0, "Three", Some("OEBPS/text/ch2.xhtml#c".into()), Some(3)),
            ]
        );
        assert_eq!(ncx.page_list.len(), 1);
        assert_eq!(ncx.page_list[0].epub_type.as_deref(), Some("normal"));
        assert_eq!(p.toc().unwrap().source, TocSource::Ncx);
    }

    /// The old crate dropped a `navPoint` with no `playOrder`. It is kept,
    /// and a level where any sibling lacks one keeps document order.
    #[test]
    fn a_nav_point_without_a_play_order_is_kept_in_document_order() {
        let ncx = r#"<ncx><navMap>
  <navPoint playOrder="2"><navLabel><text>B</text></navLabel><content src="text/ch2.xhtml"/></navPoint>
  <navPoint><navLabel><text>No order</text></navLabel><content src="text/ch1.xhtml"/></navPoint>
  <navPoint playOrder="1"><navLabel><text>A</text></navLabel><content src="text/ch1.xhtml"/>
    <navPoint playOrder="9"><navLabel><text>A.2</text></navLabel><content src="text/ch1.xhtml#2"/></navPoint>
    <navPoint playOrder="8"><navLabel><text>A.1</text></navLabel><content src="text/ch1.xhtml#1"/></navPoint>
  </navPoint>
</navMap></ncx>"#;
        let bytes = book(None, Some(ncx));
        let mut p = Package::from_bytes(&bytes).unwrap();
        let toc = p.ncx_toc();
        let labels: Vec<(usize, &str)> =
            flatten(&toc).map(|(d, n)| (d, n.label.as_str())).collect();
        assert_eq!(
            labels,
            vec![(0, "B"), (0, "No order"), (0, "A"), (1, "A.1"), (1, "A.2")]
        );
    }

    /// With both documents, the library prefers the navigation document.
    #[test]
    fn toc_prefers_the_navigation_document_then_the_ncx() {
        let bytes = book(Some(NAV), Some(NCX));
        let mut p = Package::from_bytes(&bytes).unwrap();
        assert_eq!(p.toc().unwrap().source, TocSource::Nav);
        let empty_nav = r#"<html><body><nav epub:type="toc"><ol/></nav></body></html>"#;
        let bytes = book(Some(empty_nav), Some(NCX));
        let mut p = Package::from_bytes(&bytes).unwrap();
        assert_eq!(p.toc().unwrap().source, TocSource::Ncx);
        let bytes = book(None, None);
        let mut p = Package::from_bytes(&bytes).unwrap();
        assert_eq!(p.toc(), None);
        assert!(p.warnings().is_empty());
    }

    #[test]
    fn an_ncx_found_by_media_type_when_the_spine_names_none() {
        let bytes = book(None, Some(NCX));
        // Drop the spine's toc attribute.
        let mut p = Package::from_bytes(&bytes).unwrap();
        assert!(p.spine().toc.is_some());
        let opf = opf(
            "2.0",
            "<dc:title>T</dc:title>",
            &format!(r#"{CH}<item id="n" href="toc.ncx" media-type="application/x-dtbncx+xml"/>"#),
            &format!("<spine>{SPINE}</spine>"),
        );
        let c = container("OEBPS/content.opf");
        let bytes = zip_of(&[
            ("META-INF/container.xml", c.as_bytes()),
            ("OEBPS/content.opf", opf.as_bytes()),
            ("OEBPS/toc.ncx", NCX.as_bytes()),
        ]);
        let mut q = Package::from_bytes(&bytes).unwrap();
        assert_eq!(q.ncx_toc().len(), 2);
        assert_eq!(p.ncx_toc().len(), 2);
    }

    #[test]
    fn a_broken_or_missing_navigation_document_is_a_warning_and_nothing() {
        for (nav, problem) in [
            (
                "<html><body><p>no nav here</p></body></html>",
                TocProblem::NoNav,
            ),
            ("", TocProblem::Malformed),
        ] {
            let bytes = book(Some(nav), None);
            let mut p = Package::from_bytes(&bytes).unwrap();
            assert_eq!(p.nav(), None);
            assert!(p.nav_toc().is_empty());
            assert!(
                p.warnings().contains(&Warning::NavUnreadable {
                    id: "nav".into(),
                    problem
                }),
                "{:?}",
                p.warnings()
            );
        }
    }

    /// Real navigation documents are not always well-formed; what can be
    /// read is.
    #[test]
    fn a_sloppy_navigation_document_is_read_as_far_as_it_goes() {
        let nav = r#"<HTML><BODY><NAV epub:type="toc"><OL>
<LI><A HREF=../text/ch1.xhtml>One & Only<BR></A></LI>
<LI><A HREF="../text/ch2.xhtml">Two</A>
</OL></NAV></BODY>"#;
        let bytes = book(Some(nav), None);
        let mut p = Package::from_bytes(&bytes).unwrap();
        let toc = p.nav_toc();
        let labels: Vec<&str> = flatten(&toc).map(|(_, n)| n.label.as_str()).collect();
        assert_eq!(labels, vec!["One & Only", "Two"]);
        assert!(p.warnings().iter().any(|w| matches!(
            w,
            Warning::MarkupRecovered {
                problem: MarkupProblem::MismatchedEnd | MarkupProblem::Unclosed,
                ..
            }
        )));
    }

    #[test]
    fn a_navigation_document_nested_past_the_depth_cap_is_refused() {
        let deep = format!(
            r#"<html><body><nav epub:type="toc">{}{}</nav></body></html>"#,
            "<ol><li><a href='x'>x</a>".repeat(200),
            "</li></ol>".repeat(200)
        );
        let bytes = book(Some(&deep), None);
        let mut p = Package::from_bytes(&bytes).unwrap();
        assert!(p.nav_toc().is_empty());
        assert!(p.warnings().contains(&Warning::NavUnreadable {
            id: "nav".into(),
            problem: TocProblem::LimitExceeded
        }));
        // Under the cap, every level is kept.
        let limits = Limits::default().with_max_xml_depth(1024);
        let mut p = Package::from_bytes_with(&bytes, limits).unwrap();
        let toc = p.nav_toc();
        assert_eq!(flatten(&toc).map(|(d, _)| d).max(), Some(199));
    }

    #[test]
    fn a_navigation_document_over_the_size_cap_is_refused() {
        let big = format!(
            r#"<html><body><nav epub:type="toc"><ol>{}</ol></nav></body></html>"#,
            "<li><a href='x'>x</a></li>".repeat(1000)
        );
        let bytes = book(Some(&big), None);
        let limits = Limits::default().with_max_xml_bytes(4096);
        let mut p = Package::from_bytes_with(&bytes, limits).unwrap();
        assert!(p.nav_toc().is_empty());
        assert!(p.warnings().contains(&Warning::NavUnreadable {
            id: "nav".into(),
            problem: TocProblem::LimitExceeded
        }));
    }

    #[test]
    fn hrefs_resolve_against_the_document_that_holds_them() {
        assert_eq!(
            resolve("OEBPS/nav/nav.xhtml", Some("../a.xhtml#f")),
            (Some("OEBPS/nav/../a.xhtml".into()), Some("f".into()))
        );
        assert_eq!(
            resolve("OEBPS/nav.xhtml", Some("#notes")),
            (Some("OEBPS/nav.xhtml".into()), Some("notes".into()))
        );
        assert_eq!(
            resolve("toc.ncx", Some("a.xhtml")),
            (Some("a.xhtml".into()), None)
        );
        assert_eq!(resolve("x", None), (None, None));
        assert_eq!(resolve("x", Some("mailto:a@b")), (None, None));
    }

    /// A 20 000-entry table of contents over a 20 000-item manifest: every
    /// entry's spine position is looked up, which used to scan the manifest
    /// per entry. The bound is loose on purpose — the point is that it is
    /// linear, which a quadratic scan (4 × 10⁸ path comparisons) is not.
    #[test]
    fn a_huge_toc_over_a_huge_manifest_resolves_in_linear_time() {
        const N: usize = 20_000;
        let mut manifest = String::new();
        let mut spine = String::from(r#"<spine toc="ncx">"#);
        let mut ncx = String::from("<ncx><navMap>");
        for i in 0..N {
            manifest.push_str(&format!(
                r#"<item id="c{i}" href="t/c{i}.xhtml" media-type="application/xhtml+xml"/>"#
            ));
            spine.push_str(&format!(r#"<itemref idref="c{i}"/>"#));
            ncx.push_str(&format!(
                r#"<navPoint playOrder="{i}"><navLabel><text>C{i}</text></navLabel><content src="t/c{i}.xhtml#a"/></navPoint>"#
            ));
        }
        manifest
            .push_str(r#"<item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>"#);
        spine.push_str("</spine>");
        ncx.push_str("</navMap></ncx>");
        let opf = opf("2.0", "<dc:title>T</dc:title>", &manifest, &spine);
        let c = container("OEBPS/content.opf");
        let bytes = zip_of(&[
            ("META-INF/container.xml", c.as_bytes()),
            ("OEBPS/content.opf", opf.as_bytes()),
            ("OEBPS/toc.ncx", ncx.as_bytes()),
        ]);
        let mut p = Package::from_bytes(&bytes).unwrap();
        let started = std::time::Instant::now();
        let toc = p.ncx_toc();
        let resolved: Vec<Option<usize>> = flatten(&toc)
            .map(|(_, n)| n.path.as_deref().and_then(|x| p.spine_index_of_path(x)))
            .collect();
        let elapsed = started.elapsed();
        assert_eq!(resolved.len(), N);
        assert!(resolved.iter().enumerate().all(|(i, s)| *s == Some(i)));
        assert!(elapsed < std::time::Duration::from_secs(20), "{elapsed:?}");
        eprintln!("20k-entry toc read and resolved in {elapsed:?}");
    }

    /// An HTML entity in an NCX label resolves, and whitespace inside a label
    /// collapses to one space. (The old `epub` crate dropped the whole TOC on
    /// the entity, and trimmed only a label's ends.)
    #[test]
    fn ncx_labels_resolve_html_entities_and_collapse_whitespace() {
        let ncx = "<ncx><navMap><navPoint playOrder=\"1\"><navLabel><text>Part\n      One &mdash; Start</text></navLabel><content src=\"text/ch1.xhtml\"/></navPoint></navMap></ncx>";
        let bytes = book(None, Some(ncx));
        let mut p = Package::from_bytes(&bytes).unwrap();
        let toc = p.ncx_toc();
        assert_eq!(toc.len(), 1);
        assert_eq!(toc[0].label, "Part One — Start");
        assert_eq!(toc[0].target().as_deref(), Some("OEBPS/text/ch1.xhtml"));
    }

    /// An NCX in a subdirectory of the package document's resolves its links
    /// against its own directory, and the spine item is still found. (The
    /// old crate joined them to the package document's directory, so
    /// `../text/ch1.xhtml` named no spine item.)
    #[test]
    fn an_ncx_in_a_subdirectory_resolves_against_its_own_directory() {
        let ncx = r#"<ncx><navMap><navPoint playOrder="1"><navLabel><text>One</text></navLabel><content src="../text/ch1.xhtml"/></navPoint></navMap></ncx>"#;
        let opf = opf(
            "2.0",
            "<dc:title>T</dc:title>",
            &format!(
                r#"{CH}<item id="ncx" href="nav/toc.ncx" media-type="application/x-dtbncx+xml"/>"#
            ),
            &format!(r#"<spine toc="ncx">{SPINE}</spine>"#),
        );
        let c = container("OEBPS/content.opf");
        let bytes = zip_of(&[
            ("META-INF/container.xml", c.as_bytes()),
            ("OEBPS/content.opf", opf.as_bytes()),
            ("OEBPS/nav/toc.ncx", ncx.as_bytes()),
        ]);
        let mut p = Package::from_bytes(&bytes).unwrap();
        let toc = p.ncx_toc();
        assert_eq!(toc.len(), 1);
        assert_eq!(toc[0].path.as_deref(), Some("OEBPS/nav/../text/ch1.xhtml"));
        assert_eq!(
            p.spine_index_of_path("OEBPS/nav/../text/ch1.xhtml"),
            Some(0)
        );
    }

    /// A `navPoint` with no label is kept with an empty one, and one with no
    /// `content` of its own is kept with no link — it does not borrow a
    /// descendant's. (The old crate dropped the label-less point with its
    /// whole subtree, and searched depth-first for `content`.)
    #[test]
    fn a_nav_point_without_a_label_or_its_own_content_is_kept() {
        let ncx = r#"<ncx><navMap><navPoint playOrder="1"><content src="text/ch1.xhtml"/></navPoint><navPoint playOrder="2"><navLabel><text>Heading</text></navLabel><navPoint playOrder="3"><navLabel><text>Inside</text></navLabel><content src="text/ch1.xhtml#i"/></navPoint></navPoint></navMap></ncx>"#;
        let bytes = book(None, Some(ncx));
        let mut p = Package::from_bytes(&bytes).unwrap();
        let toc = p.ncx_toc();
        let flat: Vec<(usize, &str, Option<String>)> = flatten(&toc)
            .map(|(d, n)| (d, n.label.as_str(), n.target()))
            .collect();
        assert_eq!(
            flat,
            vec![
                (0, "", Some("OEBPS/text/ch1.xhtml".into())),
                (0, "Heading", None),
                (1, "Inside", Some("OEBPS/text/ch1.xhtml#i".into())),
            ]
        );
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use crate::testkit::{container, esc, opf, zip_of};
    use proptest::prelude::*;

    /// A tree of labels, at most four deep.
    #[derive(Debug, Clone)]
    struct Tree {
        label: String,
        children: Vec<Tree>,
    }

    fn label() -> impl Strategy<Value = String> {
        // Text with no XML whitespace at the ends and single spaces inside,
        // so it is its own collapsed form; markup characters included.
        "[A-Za-z0-9é&<>'\"][A-Za-z0-9é&<>'\",.-]{0,8}( [A-Za-z0-9é&<>'\"]{1,6}){0,2}"
    }

    fn tree() -> impl Strategy<Value = Vec<Tree>> {
        let leaf = label().prop_map(|label| Tree {
            label,
            children: Vec::new(),
        });
        let node = leaf.prop_recursive(4, 40, 4, |inner| {
            (label(), proptest::collection::vec(inner, 0..4))
                .prop_map(|(label, children)| Tree { label, children })
        });
        proptest::collection::vec(node, 1..5)
    }

    fn flat(trees: &[Tree], depth: usize, out: &mut Vec<(usize, String)>) {
        for t in trees {
            out.push((depth, t.label.clone()));
            flat(&t.children, depth + 1, out);
        }
    }

    fn nav_ol(trees: &[Tree], next: &mut usize, out: &mut String) {
        out.push_str("<ol>");
        for t in trees {
            let n = *next;
            *next += 1;
            out.push_str(&format!(
                "<li><a href=\"ch.xhtml#n{n}\">{}</a>",
                esc(&t.label)
            ));
            if !t.children.is_empty() {
                nav_ol(&t.children, next, out);
            }
            out.push_str("</li>");
        }
        out.push_str("</ol>");
    }

    fn ncx_points(trees: &[Tree], next: &mut usize, out: &mut String) {
        for t in trees {
            let n = *next;
            *next += 1;
            out.push_str(&format!(
                "<navPoint id=\"n{n}\" playOrder=\"{n}\"><navLabel><text>{}</text></navLabel><content src=\"ch.xhtml#n{n}\"/>",
                esc(&t.label)
            ));
            ncx_points(&t.children, next, out);
            out.push_str("</navPoint>");
        }
    }

    fn package(nav: Option<&[u8]>, ncx: Option<&[u8]>) -> Vec<u8> {
        let mut manifest =
            r#"<item id="ch" href="ch.xhtml" media-type="application/xhtml+xml"/>"#.to_string();
        if nav.is_some() {
            manifest.push_str(r#"<item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>"#);
        }
        if ncx.is_some() {
            manifest.push_str(
                r#"<item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>"#,
            );
        }
        let opf = opf(
            "3.0",
            "<dc:title>T</dc:title>",
            &manifest,
            r#"<spine toc="ncx"><itemref idref="ch"/></spine>"#,
        );
        let c = container("OEBPS/content.opf");
        let mut entries: Vec<(&str, &[u8])> = vec![
            ("META-INF/container.xml", c.as_bytes()),
            ("OEBPS/content.opf", opf.as_bytes()),
            ("OEBPS/ch.xhtml", b"<html/>"),
        ];
        if let Some(n) = nav {
            entries.push(("OEBPS/nav.xhtml", n));
        }
        if let Some(n) = ncx {
            entries.push(("OEBPS/toc.ncx", n));
        }
        zip_of(&entries)
    }

    fn check(toc: &[TocNode], want: &[(usize, String)]) -> Result<(), TestCaseError> {
        let got: Vec<(usize, String)> = flatten(toc).map(|(d, n)| (d, n.label.clone())).collect();
        prop_assert_eq!(&got, want);
        for (i, (_, n)) in flatten(toc).enumerate() {
            prop_assert_eq!(n.target(), Some(format!("OEBPS/ch.xhtml#n{i}")));
        }
        Ok(())
    }

    proptest! {
        /// A generated navigation document reads back as the tree it was
        /// written from, every target and every depth in place.
        #[test]
        fn a_generated_nav_tree_round_trips(t in tree()) {
            let mut ol = String::new();
            nav_ol(&t, &mut 0, &mut ol);
            let nav = format!(r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc">{ol}</nav></body></html>"#);
            let bytes = package(Some(nav.as_bytes()), None);
            let mut p = Package::from_bytes(&bytes).unwrap();
            let mut want = Vec::new();
            flat(&t, 0, &mut want);
            check(&p.nav_toc(), &want)?;
            prop_assert!(p.warnings().is_empty(), "{:?}", p.warnings());
        }

        /// A generated NCX likewise, with playOrder in document order.
        #[test]
        fn a_generated_ncx_tree_round_trips(t in tree()) {
            let mut points = String::new();
            ncx_points(&t, &mut 0, &mut points);
            let ncx = format!(r#"<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"><navMap>{points}</navMap></ncx>"#);
            let bytes = package(None, Some(ncx.as_bytes()));
            let mut p = Package::from_bytes(&bytes).unwrap();
            let mut want = Vec::new();
            flat(&t, 0, &mut want);
            check(&p.ncx_toc(), &want)?;
            prop_assert!(p.warnings().is_empty(), "{:?}", p.warnings());
        }

        /// Flattening visits every node once, parents before children, and a
        /// node's depth is one more than its parent's.
        #[test]
        fn flatten_preserves_order_and_depth(t in tree()) {
            fn to_nodes(t: &[Tree]) -> Vec<TocNode> {
                t.iter().map(|t| TocNode { label: t.label.clone(), children: to_nodes(&t.children), ..TocNode::default() }).collect()
            }
            let nodes = to_nodes(&t);
            let mut want = Vec::new();
            flat(&t, 0, &mut want);
            let got: Vec<(usize, String)> = flatten(&nodes).map(|(d, n)| (d, n.label.clone())).collect();
            prop_assert_eq!(&got, &want);
            let mut prev = 0usize;
            for (i, (d, _)) in got.iter().enumerate() {
                let ok = if i == 0 { *d == 0 } else { *d <= prev + 1 };
                prop_assert!(ok, "depth {} after {}", d, prev);
                prev = *d;
            }
        }

        /// Arbitrary bytes as a navigation document or an NCX never panic.
        #[test]
        fn arbitrary_toc_documents_never_panic(
            bytes in proptest::collection::vec(any::<u8>(), 0..1024),
            markup in "[<>/&;#a-zA-Z0-9 \"'=!?-]{0,300}",
        ) {
            for doc in [bytes.as_slice(), markup.as_bytes()] {
                let as_nav = package(Some(doc), None);
                let as_ncx = package(None, Some(doc));
                for b in [as_nav, as_ncx] {
                    let mut p = Package::from_bytes(&b).unwrap();
                    let _ = p.toc();
                    let _ = p.nav();
                    let _ = p.ncx();
                }
            }
        }
    }
}
