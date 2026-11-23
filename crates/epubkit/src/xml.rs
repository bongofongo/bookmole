//! Bounded XML layer: bytes to text to a small element tree; names compare by local name.
//! `parse` is strict, `parse_html` repairs sloppy XHTML; unknown entities stay literal.

use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

use crate::Limits;
use crate::entities;
use crate::error::{Error, Limit, MarkupProblem};

/// One element.
#[derive(Debug, Clone, Default)]
pub(crate) struct Node {
    /// The local name (`title` for `dc:title`); lower-cased in HTML mode.
    pub name: String,
    /// `(local name, value)` in document order. A repeated name is kept; `attr` answers the first.
    pub attrs: Vec<(String, String)>,
    /// The last non-whitespace run of direct character data.
    pub text: Option<String>,
    /// Children and character data, in document order.
    pub content: Vec<Content>,
}

#[derive(Debug, Clone)]
pub(crate) enum Content {
    Text(String),
    CData(String),
    Element(Node),
}

impl Node {
    /// The first attribute with this local name.
    pub(crate) fn attr(&self, local: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == local)
            .map(|(_, v)| v.as_str())
    }

    /// Child elements, in order.
    pub(crate) fn children(&self) -> impl Iterator<Item = &Node> {
        self.content.iter().filter_map(|c| match c {
            Content::Element(n) => Some(n),
            _ => None,
        })
    }

    /// The first child element with this local name.
    pub(crate) fn child(&self, local: &str) -> Option<&Node> {
        self.children().find(|c| c.name == local)
    }

    /// The first descendant (not self) with this local name, depth first.
    pub(crate) fn find(&self, local: &str) -> Option<&Node> {
        for c in self.children() {
            if c.name == local {
                return Some(c);
            }
            if let Some(n) = c.find(local) {
                return Some(n);
            }
        }
        None
    }

    /// Every descendant (not self) with this local name, in document order.
    pub(crate) fn find_all(&self, local: &str) -> Vec<&Node> {
        let mut out = Vec::new();
        let mut stack: Vec<std::slice::Iter<'_, Content>> = vec![self.content.iter()];
        while let Some(top) = stack.last_mut() {
            match top.next() {
                Some(Content::Element(n)) => {
                    if n.name == local {
                        out.push(n);
                    }
                    stack.push(n.content.iter());
                }
                Some(_) => {}
                None => {
                    stack.pop();
                }
            }
        }
        out
    }

    /// All character data in the subtree, text and CDATA alike, in order.
    pub(crate) fn text_content(&self) -> String {
        let mut out = String::new();
        self.collect_text(&mut out);
        out
    }

    fn collect_text(&self, out: &mut String) {
        for c in &self.content {
            match c {
                Content::Text(t) | Content::CData(t) => out.push_str(t),
                Content::Element(n) => n.collect_text(out),
            }
        }
    }
}

/// A parsed document, the entities it used that were left unexpanded, and — in HTML mode —
/// what had to be repaired to read it.
pub(crate) struct Document {
    pub root: Node,
    pub undeclared_entities: Vec<String>,
    pub problems: Vec<MarkupProblem>,
}

pub(crate) fn is_xml_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

/// Collapses runs of XML whitespace (not U+00A0) to one space and trims.
pub(crate) fn collapse(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for word in s.split(is_xml_ws).filter(|w| !w.is_empty()) {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// A UTF-8 BOM is dropped; a UTF-16 BOM selects UTF-16 (lossy); otherwise the declared `encoding`
/// picks UTF-8, ISO-8859-1 or US-ASCII. Any other declaration is accepted only if valid UTF-8.
pub(crate) fn decode(bytes: &[u8], path: &str) -> Result<String, Error> {
    let bad = || Error::Encoding {
        path: path.to_string(),
    };
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8(rest.to_vec()).map_err(|_| bad());
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return Ok(utf16(rest, u16::from_be_bytes));
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return Ok(utf16(rest, u16::from_le_bytes));
    }
    // BOM-less UTF-16 still starts `<` `?` with a zero byte beside each; detect it before reading
    // the encoding declaration.
    if bytes.starts_with(&[b'<', 0, b'?', 0]) {
        return Ok(utf16(bytes, u16::from_le_bytes));
    }
    if bytes.starts_with(&[0, b'<', 0, b'?']) {
        return Ok(utf16(bytes, u16::from_be_bytes));
    }
    match declared_encoding(bytes).map(|e| e.to_ascii_lowercase()) {
        Some(e) if e == "iso-8859-1" || e == "latin1" => {
            Ok(bytes.iter().map(|&b| b as char).collect())
        }
        Some(e) if e == "ascii" || e == "us-ascii" => {
            if bytes.is_ascii() {
                Ok(bytes.iter().map(|&b| b as char).collect())
            } else {
                Err(bad())
            }
        }
        _ => String::from_utf8(bytes.to_vec()).map_err(|_| bad()),
    }
}

fn utf16(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> String {
    let (pairs, _odd_trailing_byte) = bytes.as_chunks::<2>();
    let units: Vec<u16> = pairs.iter().copied().map(unit).collect();
    String::from_utf16_lossy(&units)
}

/// The `encoding` pseudo-attribute of a leading `<?xml …?>`, read as ASCII.
fn declared_encoding(bytes: &[u8]) -> Option<&str> {
    let head = bytes.get(..bytes.len().min(256))?;
    if !head.starts_with(b"<?xml") {
        return None;
    }
    let end = head.windows(2).position(|w| w == b"?>")?;
    let decl = std::str::from_utf8(head.get(..end)?).ok()?;
    let at = decl.find("encoding")?;
    let rest = decl.get(at + "encoding".len()..)?.trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let rest = rest.get(1..)?;
    let close = rest.find(quote)?;
    rest.get(..close)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Xml,
    Html,
}

/// Decode and parse a well-formed XML document, under the XML limits.
pub(crate) fn parse(bytes: &[u8], path: &str, limits: &Limits) -> Result<Document, Error> {
    parse_mode(bytes, path, limits, Mode::Xml)
}

/// Lenient; fails only on a limit, or when not even a root element could be read.
pub(crate) fn parse_html(bytes: &[u8], path: &str, limits: &Limits) -> Result<Document, Error> {
    parse_mode(bytes, path, limits, Mode::Html)
}

fn parse_mode(bytes: &[u8], path: &str, limits: &Limits, mode: Mode) -> Result<Document, Error> {
    let too_big = || Error::LimitExceeded {
        limit: Limit::XmlBytes,
        path: Some(path.to_string()),
    };
    if bytes.len() > limits.max_xml_bytes {
        return Err(too_big());
    }
    let (text, lossy) = match decode(bytes, path) {
        Ok(t) => (t, false),
        Err(_) if mode == Mode::Html => (String::from_utf8_lossy(bytes).into_owned(), true),
        Err(e) => return Err(e),
    };
    if text.len() > limits.max_xml_bytes {
        return Err(too_big());
    }
    let mut doc = parse_str_mode(&text, path, limits, mode)?;
    if lossy {
        doc.problems.insert(0, MarkupProblem::Encoding);
    }
    Ok(doc)
}

/// HTML's void elements: never have content, whether written `<br>` or `<br/>`, and an end
/// tag for one is ignored.
fn is_void(name: &str) -> bool {
    matches!(
        name,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "keygen"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

/// Longest name searched for a reference: entity names are at most 8 bytes, numeric ones 9.
const MAX_REFERENCE_NAME: usize = 32;

struct Builder<'p> {
    path: &'p str,
    mode: Mode,
    limits: Limits,
    stack: Vec<Node>,
    /// HTML mode: open-element counts per name, so a stray end tag is detected in O(1).
    open_names: std::collections::HashMap<String, usize>,
    /// Character data since the last markup that splits a run.
    pending: String,
    undeclared: Vec<String>,
    seen: std::collections::HashSet<String>,
    problems: Vec<MarkupProblem>,
    /// Elements and character-data pieces created so far.
    nodes: usize,
}

impl Builder<'_> {
    fn malformed(&self, message: impl Into<String>) -> Error {
        Error::MalformedXml {
            path: self.path.to_string(),
            message: message.into(),
        }
    }

    fn limit(&self, limit: Limit) -> Error {
        Error::LimitExceeded {
            limit,
            path: Some(self.path.to_string()),
        }
    }

    fn problem(&mut self, p: MarkupProblem) {
        if !self.problems.contains(&p) {
            self.problems.push(p);
        }
    }

    /// Count one more node against the cap.
    fn count(&mut self) -> Result<(), Error> {
        self.nodes += 1;
        if self.nodes > self.limits.max_xml_nodes {
            return Err(self.limit(Limit::XmlNodes));
        }
        Ok(())
    }

    /// End the current run of character data.
    fn flush(&mut self) -> Result<(), Error> {
        if self.pending.is_empty() {
            return Ok(());
        }
        if self.stack.is_empty() {
            self.pending.clear();
            return Ok(());
        }
        self.count()?;
        let run = std::mem::take(&mut self.pending);
        if let Some(top) = self.stack.last_mut() {
            if !run.chars().all(is_xml_ws) {
                top.text = Some(run.clone());
            }
            top.content.push(Content::Text(run));
        }
        Ok(())
    }

    /// Unknown names stay literal.
    fn reference(&mut self, name: &str, out: &mut String) -> Result<(), Error> {
        if let Some(num) = name.strip_prefix('#') {
            let code = if let Some(hex) = num.strip_prefix('x').or_else(|| num.strip_prefix('X')) {
                u32::from_str_radix(hex, 16).ok()
            } else {
                num.parse::<u32>().ok()
            };
            match code.and_then(char::from_u32).filter(|c| is_xml_char(*c)) {
                Some(c) => out.push(c),
                None if self.mode == Mode::Html => {
                    self.problem(MarkupProblem::BadReference);
                    out.push('&');
                    out.push_str(name);
                    out.push(';');
                }
                None => return Err(self.malformed(format!("invalid character reference &{name};"))),
            }
            return Ok(());
        }
        match entities::lookup(name) {
            Some(c) => out.push(c),
            None => {
                out.push('&');
                out.push_str(name);
                out.push(';');
                if self.seen.insert(name.to_string()) {
                    self.undeclared.push(name.to_string());
                }
            }
        }
        Ok(())
    }

    fn attribute_value(&mut self, raw: &str) -> Result<String, Error> {
        let html = self.mode == Mode::Html;
        if raw.contains('<') && !html {
            return Err(self.malformed("'<' in an attribute value"));
        }
        let mut out = String::with_capacity(raw.len());
        let mut rest = raw;
        while let Some(amp) = rest.find('&') {
            out.push_str(rest.get(..amp).unwrap_or_default());
            let after = rest.get(amp + 1..).unwrap_or_default();
            let name = if html {
                // Look only a short run ahead for the `;`: searching for the next one each time makes a value
                // of many stray `&`s quadratic.
                let len = after
                    .bytes()
                    .take(MAX_REFERENCE_NAME)
                    .take_while(|b| b.is_ascii_alphanumeric() || *b == b'#')
                    .count();
                after
                    .get(..len)
                    .filter(|_| after.as_bytes().get(len) == Some(&b';'))
            } else {
                after.find(';').and_then(|semi| after.get(..semi))
            };
            let name = name.filter(|n| !n.is_empty());
            let Some(name) = name else {
                if html {
                    out.push('&');
                    rest = after;
                    continue;
                }
                return Err(self.malformed("unterminated or empty reference in an attribute value"));
            };
            self.reference(name, &mut out)?;
            rest = after.get(name.len() + 1..).unwrap_or_default();
        }
        out.push_str(rest);
        Ok(out)
    }

    fn element(&mut self, e: &BytesStart<'_>) -> Result<Node, Error> {
        let html = self.mode == Mode::Html;
        let mut name = local(e.name().as_ref());
        if html {
            name.make_ascii_lowercase();
        }
        self.count()?;
        let mut attrs = Vec::new();
        let mut iter = if html {
            e.html_attributes()
        } else {
            e.attributes()
        };
        // quick-xml's duplicate check is quadratic in attribute count; repeats are kept and
        // `Node::attr` answers the first.
        iter.with_checks(false);
        let mut seen = 0usize;
        for a in iter {
            seen += 1;
            if seen > self.limits.max_xml_attributes {
                return Err(self.limit(Limit::XmlAttributes));
            }
            let a = match a {
                Ok(a) => a,
                Err(_) if html => {
                    self.problem(MarkupProblem::BadReference);
                    break;
                }
                Err(err) => return Err(self.malformed(err.to_string())),
            };
            let key = a.key.as_ref();
            if key == b"xmlns" || key.starts_with(b"xmlns:") {
                continue;
            }
            let raw = match std::str::from_utf8(&a.value) {
                Ok(s) => s.to_string(),
                Err(_) if html => String::from_utf8_lossy(&a.value).into_owned(),
                Err(_) => return Err(self.malformed("attribute value is not UTF-8")),
            };
            let value = self.attribute_value(&raw)?;
            let mut key = local(key);
            if html {
                key.make_ascii_lowercase();
            }
            attrs.push((key, value));
        }
        Ok(Node {
            name,
            attrs,
            text: None,
            content: Vec::new(),
        })
    }

    fn open(&mut self, node: Node) {
        if self.mode == Mode::Html {
            *self.open_names.entry(node.name.clone()).or_default() += 1;
        }
        self.stack.push(node);
    }

    fn pop(&mut self) -> Option<Node> {
        let node = self.stack.pop()?;
        if self.mode == Mode::Html
            && let Some(n) = self.open_names.get_mut(&node.name)
        {
            *n = n.saturating_sub(1);
        }
        Some(node)
    }

    fn is_open(&self, name: &str) -> bool {
        self.open_names.get(name).is_some_and(|&n| n > 0)
    }

    /// Put a finished element where it belongs: into its parent, or — when it is the root — out
    /// as the document.
    fn place(&mut self, node: Node) -> Option<Node> {
        match self.stack.last_mut() {
            Some(parent) => {
                parent.content.push(Content::Element(node));
                None
            }
            None => Some(node),
        }
    }

    /// Close the top element.
    fn close_top(&mut self) -> Option<Node> {
        let node = self.pop()?;
        self.place(node)
    }

    /// Close every open element; the root comes out.
    fn close_all(&mut self) -> Option<Node> {
        while !self.stack.is_empty() {
            if let Some(root) = self.close_top() {
                return Some(root);
            }
        }
        None
    }

    fn done(self, root: Node) -> Document {
        Document {
            root,
            undeclared_entities: self.undeclared,
            problems: self.problems,
        }
    }
}

fn local(qname: &[u8]) -> String {
    let local = match qname.iter().rposition(|&b| b == b':') {
        Some(i) => qname.get(i + 1..).unwrap_or_default(),
        None => qname,
    };
    String::from_utf8_lossy(local).into_owned()
}

/// XML 1.0's `Char` production.
fn is_xml_char(c: char) -> bool {
    matches!(c as u32,
        0x9 | 0xA | 0xD | 0x20..=0xD7FF | 0xE000..=0xFFFD | 0x10000..=0x10FFFF)
}

#[expect(clippy::too_many_lines)]
fn parse_str_mode(text: &str, path: &str, limits: &Limits, mode: Mode) -> Result<Document, Error> {
    let html = mode == Mode::Html;
    let mut reader = Reader::from_str(text);
    {
        let config = reader.config_mut();
        config.trim_text(false);
        config.expand_empty_elements = false;
        // HTML mode matches end tags itself, so it can repair what they skip.
        config.check_end_names = !html;
        config.allow_unmatched_ends = html;
        config.allow_dangling_amp = html;
    }
    let mut b = Builder {
        path,
        mode,
        limits: *limits,
        stack: Vec::new(),
        open_names: std::collections::HashMap::new(),
        pending: String::new(),
        undeclared: Vec::new(),
        seen: std::collections::HashSet::new(),
        problems: Vec::new(),
        nodes: 0,
    };

    loop {
        let event = match reader.read_event() {
            Ok(e) => e,
            Err(e) => {
                if html && !b.stack.is_empty() {
                    b.flush()?;
                    b.problem(MarkupProblem::Truncated);
                    if let Some(root) = b.close_all() {
                        return Ok(b.done(root));
                    }
                }
                return Err(b.malformed(e.to_string()));
            }
        };
        match event {
            Event::Start(e) => {
                b.flush()?;
                if b.stack.len() >= limits.max_xml_depth {
                    return Err(b.limit(Limit::XmlDepth));
                }
                let node = b.element(&e)?;
                if html && is_void(&node.name) {
                    if b.stack.is_empty() {
                        return Ok(b.done(node));
                    }
                    let _ = b.place(node);
                } else {
                    b.open(node);
                }
            }
            Event::Empty(e) => {
                b.flush()?;
                if b.stack.len() >= limits.max_xml_depth {
                    return Err(b.limit(Limit::XmlDepth));
                }
                let node = b.element(&e)?;
                if let Some(root) = b.place(node) {
                    return Ok(b.done(root));
                }
            }
            Event::End(e) => {
                b.flush()?;
                if html {
                    let mut name = local(e.name().as_ref());
                    name.make_ascii_lowercase();
                    if is_void(&name) {
                        continue;
                    }
                    let at = b
                        .is_open(&name)
                        .then(|| b.stack.iter().rposition(|n| n.name == name))
                        .flatten();
                    let Some(at) = at else {
                        b.problem(MarkupProblem::MismatchedEnd);
                        continue;
                    };
                    if at + 1 != b.stack.len() {
                        b.problem(MarkupProblem::MismatchedEnd);
                    }
                    while b.stack.len() > at + 1 {
                        if let Some(root) = b.close_top() {
                            return Ok(b.done(root));
                        }
                    }
                }
                let Some(node) = b.pop() else {
                    return Err(b.malformed("closing tag with nothing open"));
                };
                // The root closed: anything after it is ignored.
                if let Some(root) = b.place(node) {
                    return Ok(b.done(root));
                }
            }
            Event::Text(t) => {
                if !b.stack.is_empty() {
                    let s = t.decode().map_err(|e| b.malformed(e.to_string()))?;
                    b.pending.push_str(&s);
                }
            }
            Event::GeneralRef(r) => {
                if !b.stack.is_empty() {
                    let name = r.decode().map_err(|e| b.malformed(e.to_string()))?;
                    let mut out = std::mem::take(&mut b.pending);
                    b.reference(&name, &mut out)?;
                    b.pending = out;
                }
            }
            Event::CData(c) => {
                b.flush()?;
                if !b.stack.is_empty() {
                    b.count()?;
                    let s = String::from_utf8_lossy(&c).into_owned();
                    if let Some(top) = b.stack.last_mut() {
                        top.content.push(Content::CData(s));
                    }
                }
            }
            Event::PI(_) => b.flush()?,
            Event::Comment(_) | Event::Decl(_) | Event::DocType(_) => {}
            Event::Eof => {
                if html && !b.stack.is_empty() {
                    b.flush()?;
                    b.problem(MarkupProblem::Unclosed);
                    if let Some(root) = b.close_all() {
                        return Ok(b.done(root));
                    }
                }
                return Err(b.malformed(if b.stack.is_empty() {
                    "no root element"
                } else {
                    "unexpected end of document"
                }));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<Document, Error> {
        parse(s.as_bytes(), "t.xml", &Limits::default())
    }

    fn h(s: &str) -> Document {
        parse_html(s.as_bytes(), "t.xhtml", &Limits::default()).unwrap()
    }

    #[test]
    fn text_is_the_last_non_whitespace_run() {
        let d = p("<a><b>one<i/>two</b><c>  </c><d>x<!-- c -->y</d></a>").unwrap();
        let kids: Vec<&Node> = d.root.children().collect();
        assert_eq!(kids[0].text.as_deref(), Some("two"));
        assert_eq!(kids[0].text_content(), "onetwo");
        assert_eq!(kids[1].text, None);
        assert_eq!(kids[2].text.as_deref(), Some("xy"));
    }

    #[test]
    fn cdata_is_not_text() {
        let d = p("<a><![CDATA[x<y]]></a>").unwrap();
        assert_eq!(d.root.text, None);
        assert_eq!(d.root.text_content(), "x<y");
    }

    #[test]
    fn entities_resolve_or_stay_literal() {
        let d =
            p("<a t='&amp;&#65;&nbsp;&bogus;'>&lt;&#x42;&copy;&mdash;&hellip;&zzz;</a>").unwrap();
        assert_eq!(d.root.attr("t"), Some("&A\u{a0}&bogus;"));
        assert_eq!(d.root.text.as_deref(), Some("<B©—…&zzz;"));
        assert_eq!(d.undeclared_entities, vec!["bogus", "zzz"]);
    }

    #[test]
    fn names_are_local_and_xmlns_is_not_an_attribute() {
        let d =
            p(r#"<opf:package xmlns:opf="x" opf:version="3.0"><dc:title/></opf:package>"#).unwrap();
        assert_eq!(d.root.name, "package");
        assert_eq!(
            d.root.attrs,
            vec![("version".to_string(), "3.0".to_string())]
        );
        assert!(d.root.find("title").is_some());
    }

    #[test]
    fn mismatched_or_unclosed_is_an_error_but_trailing_junk_is_not() {
        assert!(p("<a><b></a>").is_err());
        assert!(p("<a><b>").is_err());
        assert!(p("").is_err());
        assert!(p("<a/>trailing <<< junk").is_ok());
    }

    /// A repeated attribute is no longer refused; the first one is the answer, as `attr` has
    /// always given.
    #[test]
    fn a_repeated_attribute_keeps_the_first() {
        let d = p(r#"<a x="1" x="2"/>"#).unwrap();
        assert_eq!(d.root.attr("x"), Some("1"));
    }

    #[test]
    fn declared_latin1_is_decoded() {
        let mut bytes = b"<?xml version='1.0' encoding='ISO-8859-1'?><a>caf".to_vec();
        bytes.push(0xE9);
        bytes.extend_from_slice(b"</a>");
        let d = parse(&bytes, "t", &Limits::default()).unwrap();
        assert_eq!(d.root.text.as_deref(), Some("café"));
    }

    #[test]
    fn utf16_with_a_bom_is_decoded_both_ways() {
        let s = "<?xml version=\"1.0\" encoding=\"UTF-16\"?><a>ü</a>";
        let mut le = vec![0xFF, 0xFE];
        let mut be = vec![0xFE, 0xFF];
        for u in s.encode_utf16() {
            le.extend_from_slice(&u.to_le_bytes());
            be.extend_from_slice(&u.to_be_bytes());
        }
        for bytes in [le, be] {
            let d = parse(&bytes, "t", &Limits::default()).unwrap();
            assert_eq!(d.root.text.as_deref(), Some("ü"));
        }
    }

    #[test]
    fn depth_and_size_are_capped() {
        let deep = format!("{}{}", "<a>".repeat(300), "</a>".repeat(300));
        assert!(matches!(
            p(&deep),
            Err(Error::LimitExceeded {
                limit: Limit::XmlDepth,
                ..
            })
        ));
        let small = Limits::default().with_max_xml_bytes(10);
        assert!(matches!(
            parse(b"<a>0123456789</a>", "t", &small),
            Err(Error::LimitExceeded {
                limit: Limit::XmlBytes,
                ..
            })
        ));
    }

    /// One element with a hundred thousand attributes fits easily inside the byte cap.
    #[test]
    fn attributes_per_element_are_capped() {
        let mut doc = String::from("<a");
        for i in 0..100_000 {
            doc.push_str(&format!(" a{i}=\"\""));
        }
        doc.push_str("/>");
        for parse_fn in [parse, parse_html] {
            assert!(matches!(
                parse_fn(doc.as_bytes(), "t", &Limits::default()),
                Err(Error::LimitExceeded {
                    limit: Limit::XmlAttributes,
                    ..
                })
            ));
        }
        let started = std::time::Instant::now();
        let raised = Limits::default().with_max_xml_attributes(200_000);
        let d = parse(doc.as_bytes(), "t", &raised).unwrap();
        assert_eq!(d.root.attrs.len(), 100_000);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
        let at_cap = format!(
            "<a{}/>",
            (0..Limits::DEFAULT_MAX_XML_ATTRIBUTES)
                .map(|i| format!(" a{i}='1'"))
                .collect::<String>()
        );
        assert!(p(&at_cap).is_ok());
    }

    /// Two million empty elements fit in eight MiB; the tree for them would not fit in a few
    /// hundred.
    #[test]
    fn nodes_per_document_are_capped() {
        let doc = format!("<r>{}</r>", "<a/>".repeat(2_000_000));
        assert!(doc.len() < Limits::DEFAULT_MAX_XML_BYTES);
        for parse_fn in [parse, parse_html] {
            assert!(matches!(
                parse_fn(doc.as_bytes(), "t", &Limits::default()),
                Err(Error::LimitExceeded {
                    limit: Limit::XmlNodes,
                    ..
                })
            ));
        }
        let small = Limits::default().with_max_xml_nodes(3);
        assert!(p("<r><a/>x</r>").is_ok());
        assert!(parse(b"<r><a/>x</r>", "t", &small).is_ok());
        assert!(matches!(
            parse(b"<r><a/>x<b/></r>", "t", &small),
            Err(Error::LimitExceeded {
                limit: Limit::XmlNodes,
                ..
            })
        ));
    }

    /// What the node cap's documentation claims: a node is about a hundred bytes before its heap
    /// allocations (its name, its slot in the parent's vector), so the default cap bounds a tree
    /// near 50–100 MiB.
    #[test]
    fn a_node_is_about_a_hundred_bytes() {
        let size = std::mem::size_of::<Content>();
        eprintln!("size_of::<Content>() = {size}");
        assert!((64..=128).contains(&size), "{size}");
    }

    /// The billion-laughs shape: declared in the DTD, never expanded.
    #[test]
    fn an_entity_bomb_is_not_expanded() {
        let mut doc = String::from("<?xml version=\"1.0\"?><!DOCTYPE lolz [<!ENTITY lol \"lol\">");
        for i in 1..10 {
            let prev = if i == 1 {
                "lol".to_string()
            } else {
                format!("lol{}", i - 1)
            };
            doc.push_str(&format!(
                "<!ENTITY lol{i} \"{}\">",
                format!("&{prev};").repeat(10)
            ));
        }
        doc.push_str("]><lolz>&lol9;</lolz>");
        let d = p(&doc).unwrap();
        assert_eq!(d.root.text.as_deref(), Some("&lol9;"));
        assert_eq!(d.undeclared_entities, vec!["lol9"]);
    }

    #[test]
    fn html_mode_lowercases_and_treats_void_elements_as_empty() {
        let d = h("<HTML><Body><P>one<BR>two<img src=x.png alt='A'>three</P></Body></HTML>");
        assert!(d.problems.is_empty(), "{:?}", d.problems);
        let body = d.root.child("body").unwrap();
        let para = body.child("p").unwrap();
        let names: Vec<&str> = para.children().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["br", "img"]);
        assert_eq!(para.child("img").unwrap().attr("src"), Some("x.png"));
        assert_eq!(para.text_content(), "onetwothree");
    }

    #[test]
    fn html_mode_repairs_end_tags() {
        let d = h("<html><div><b>bold</div><p>after</i></p></html>");
        assert_eq!(d.problems, vec![MarkupProblem::MismatchedEnd]);
        let names: Vec<&str> = d.root.children().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["div", "p"]);
        assert_eq!(d.root.child("p").unwrap().text_content(), "after");
    }

    #[test]
    fn html_mode_keeps_what_it_read_before_giving_up() {
        let d = h("<html><body><p>kept & stray</p><p>also kept</p><p class=");
        assert!(
            d.problems.contains(&MarkupProblem::Truncated),
            "{:?}",
            d.problems
        );
        let body = d.root.child("body").unwrap();
        let texts: Vec<String> = body.children().map(|n| n.text_content()).collect();
        assert_eq!(
            texts[..2],
            ["kept & stray".to_string(), "also kept".to_string()]
        );

        let d = h("<html><body><p>unclosed");
        assert_eq!(d.problems, vec![MarkupProblem::Unclosed]);
        assert_eq!(d.root.text_content(), "unclosed");
    }

    #[test]
    fn html_mode_reads_invalid_utf8_lossily() {
        let d = parse_html(b"<p>caf\xE9</p>", "t", &Limits::default()).unwrap();
        assert_eq!(d.problems, vec![MarkupProblem::Encoding]);
        assert_eq!(d.root.text_content(), "caf\u{FFFD}");
    }

    #[test]
    fn html_mode_keeps_a_bad_character_reference_as_text() {
        let d = h("<p a='&#0;'>x&#xFFFFFFFF;y</p>");
        assert_eq!(d.problems, vec![MarkupProblem::BadReference]);
        assert_eq!(d.root.attr("a"), Some("&#0;"));
        assert_eq!(d.root.text_content(), "x&#xFFFFFFFF;y");
    }

    /// Two shapes that are linear now and were not: an attribute of stray `&`s (each once
    /// searched the rest of the value for a `;`), and a run of end tags matching nothing under a
    /// deep stack (each once scanned the whole stack).
    #[test]
    fn html_mode_repairs_cost_linear_time() {
        let started = std::time::Instant::now();
        let amps = format!("<a v=\"{};\">x</a>", "&".repeat(1_000_000));
        let d = h(&amps);
        assert_eq!(d.root.attr("v").map(str::len), Some(1_000_001));

        let deep = format!(
            "<html>{}{}x{}</html>",
            "<div>".repeat(250),
            "</zz>".repeat(1_000_000),
            "</div>".repeat(250),
        );
        let d = h(&deep);
        assert_eq!(d.problems, vec![MarkupProblem::MismatchedEnd]);
        assert_eq!(d.root.text_content(), "x");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(20),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn html_mode_still_needs_a_root() {
        assert!(parse_html(b"", "t", &Limits::default()).is_err());
        assert!(parse_html(b"just text", "t", &Limits::default()).is_err());
    }

    #[test]
    fn collapse_squeezes_xml_whitespace_but_not_nbsp() {
        assert_eq!(collapse("  a \n\t b  "), "a b");
        assert_eq!(collapse("a\u{a0} b"), "a\u{a0} b");
        assert_eq!(collapse(" \n "), "");
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// Arbitrary bytes are parsed or refused, never a panic — in either mode.
        #[test]
        fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
            let _ = parse(&bytes, "t", &Limits::default());
            let _ = parse_html(&bytes, "t", &Limits::default());
        }

        /// Arbitrary text with markup characters in it, likewise.
        #[test]
        fn arbitrary_markupish_text_never_panics(s in "[<>/&;#a-zA-Z0-9 \"'=!?\\[\\]-]{0,200}") {
            let _ = parse(s.as_bytes(), "t", &Limits::default());
            let _ = parse_html(s.as_bytes(), "t", &Limits::default());
        }

        /// Escaped text round-trips through an element and an attribute.
        #[test]
        fn escaped_text_round_trips(s in "[^\\x00-\\x08\\x0B\\x0C\\x0E-\\x1F\\u{FFFE}\\u{FFFF}]{0,40}") {
            let esc = s.replace('&', "&amp;").replace('<', "&lt;").replace('"', "&quot;");
            let doc = format!("<a v=\"{esc}\">{esc}</a>");
            let d = parse(doc.as_bytes(), "t", &Limits::default()).unwrap();
            prop_assert_eq!(d.root.attr("v"), Some(s.as_str()));
            prop_assert_eq!(d.root.text_content(), s);
        }

        /// Every named entity in the table decodes to its character, in text and in an attribute, in
        /// both modes.
        #[test]
        fn every_named_entity_decodes(i in 0..entities::ENTITIES.len()) {
            let (name, c) = entities::ENTITIES[i];
            let doc = format!("<a v=\"&{name};\">&{name};</a>");
            for d in [
                parse(doc.as_bytes(), "t", &Limits::default()).unwrap(),
                parse_html(doc.as_bytes(), "t", &Limits::default()).unwrap(),
            ] {
                let want = c.to_string();
                prop_assert_eq!(d.root.attr("v"), Some(want.as_str()));
                prop_assert_eq!(d.root.text_content(), want);
                prop_assert!(d.undeclared_entities.is_empty());
            }
        }
    }
}
