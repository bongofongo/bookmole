//! A minimal EPUB reader for the generator: spine order and manifest item bytes only.
//! Independent of `epubkit` and the engine on purpose; its parsing rules are pinned to the corpus.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read, Seek};
use std::path::Path;

use quick_xml::XmlVersion;
use quick_xml::events::{BytesStart, Event};

pub struct Epub<R> {
    zip: zip::ZipArchive<R>,
    manifest: HashMap<String, String>,
    spine: Vec<String>,
}

impl Epub<BufReader<File>> {
    pub fn open(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::from_reader(BufReader::new(file))
    }
}

impl<R: Read + Seek> Epub<R> {
    pub fn from_reader(reader: R) -> Result<Self, String> {
        let mut zip = zip::ZipArchive::new(reader).map_err(|e| e.to_string())?;

        let container =
            entry(&mut zip, "META-INF/container.xml").ok_or("META-INF/container.xml is missing")?;
        let container = parse(&container)?;
        let full_path = container
            .find("rootfile")
            .and_then(|r| r.attr("full-path"))
            .ok_or("container.xml names no rootfile full-path")?
            .to_string();
        let base = Path::new(&full_path)
            .parent()
            .ok_or("the rootfile full-path has no directory")?
            .to_path_buf();

        let opf = entry(&mut zip, &full_path).ok_or_else(|| format!("{full_path} is missing"))?;
        let opf = parse(&opf)?;
        let manifest_el = opf.find("manifest").ok_or("no manifest")?;
        let spine_el = opf.find("spine").ok_or("no spine")?;
        opf.find("metadata").ok_or("no metadata")?;

        let mut manifest = HashMap::new();
        for item in &manifest_el.children {
            if let (Some(id), Some(href), Some(_media_type)) =
                (item.attr("id"), item.attr("href"), item.attr("media-type"))
            {
                let joined = base.join(href);
                let joined = joined.to_str().ok_or("a non-UTF-8 manifest path")?;
                manifest.insert(id.to_string(), joined.to_string());
            }
        }
        let spine = spine_el
            .children
            .iter()
            .filter_map(|it| it.attr("idref").map(str::to_string))
            .collect();

        Ok(Self {
            zip,
            manifest,
            spine,
        })
    }

    pub fn spine(&self) -> &[String] {
        &self.spine
    }

    pub fn resource(&mut self, id: &str) -> Option<Vec<u8>> {
        let path = self.manifest.get(id)?.clone();
        entry(&mut self.zip, &path)
    }
}

fn entry<R: Read + Seek>(zip: &mut zip::ZipArchive<R>, name: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    match zip.by_name(name) {
        Ok(mut f) => return f.read_to_end(&mut out).ok().map(|_| out),
        Err(zip::result::ZipError::FileNotFound) => {}
        Err(_) => return None,
    }
    let mut f = zip.by_name(&percent_decode(name)?).ok()?;
    f.read_to_end(&mut out).ok().map(|_| out)
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
        if b[i] == b'%'
            && let (Some(hi), Some(lo)) = (
                b.get(i + 1).copied().and_then(hex),
                b.get(i + 2).copied().and_then(hex),
            )
        {
            out.push(hi << 4 | lo);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[derive(Default)]
struct El {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<El>,
}

impl El {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    fn find(&self, name: &str) -> Option<&El> {
        self.children.iter().find_map(|c| {
            if c.name == name {
                Some(c)
            } else {
                c.find(name)
            }
        })
    }
}

fn decode(bytes: &[u8]) -> Result<String, String> {
    if bytes.len() < 4 {
        return Err("an XML document under four bytes".into());
    }
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8(rest.to_vec()).map_err(|e| e.to_string());
    }
    let be = match bytes[..2] {
        [0xFE, 0xFF] => true,
        [0xFF, 0xFE] => false,
        _ => return String::from_utf8(bytes.to_vec()).map_err(|e| e.to_string()),
    };
    let units: Vec<u16> = bytes[2..]
        .chunks_exact(2)
        .map(|p| {
            if be {
                u16::from_be_bytes([p[0], p[1]])
            } else {
                u16::from_le_bytes([p[0], p[1]])
            }
        })
        .collect();
    Ok(String::from_utf16_lossy(&units))
}

fn local(qname: &[u8]) -> String {
    let local = qname.rsplit(|&c| c == b':').next().unwrap_or(qname);
    String::from_utf8_lossy(local).into_owned()
}

fn element(e: &BytesStart<'_>) -> Option<El> {
    let mut attrs: Vec<(String, String)> = Vec::new();
    for a in e.attributes() {
        let a = a.ok()?;
        let key = a.key.as_ref();
        if key == b"xmlns" || key.starts_with(b"xmlns:") {
            continue;
        }
        let value = a
            .normalized_value_with(XmlVersion::Implicit1_0, 1, |name| match name {
                "nbsp" => Some(" "),
                "copy" => Some("©"),
                "reg" => Some("®"),
                other => quick_xml::escape::resolve_predefined_entity(other),
            })
            .ok()?;
        let key = local(key);
        if !attrs.iter().any(|(k, _)| *k == key) {
            attrs.push((key, value.into_owned()));
        }
    }
    Some(El {
        name: local(e.name().as_ref()),
        attrs,
        children: Vec::new(),
    })
}

fn parse(bytes: &[u8]) -> Result<El, String> {
    let text = decode(bytes)?;
    let mut reader = quick_xml::Reader::from_str(&text);
    let mut stack: Vec<El> = Vec::new();
    let mut root: Option<El> = None;

    let close = |el: El, stack: &mut Vec<El>, root: &mut Option<El>| match stack.last_mut() {
        Some(parent) => parent.children.push(el),
        None => {
            root.get_or_insert(el);
        }
    };

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match element(&e) {
                Some(el) => stack.push(el),
                None => break,
            },
            Ok(Event::Empty(e)) => match element(&e) {
                Some(el) => close(el, &mut stack, &mut root),
                None => break,
            },
            Ok(Event::End(_)) => {
                if let Some(el) = stack.pop() {
                    close(el, &mut stack, &mut root);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
    }
    while let Some(el) = stack.pop() {
        close(el, &mut stack, &mut root);
    }
    root.ok_or_else(|| "no XML element".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    type Book = Epub<Cursor<Vec<u8>>>;

    fn zip_of(entries: &[(&str, &[u8])]) -> Cursor<Vec<u8>> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default();
        for (name, bytes) in entries {
            w.start_file(*name, opts).unwrap();
            w.write_all(bytes).unwrap();
        }
        Cursor::new(w.finish().unwrap().into_inner())
    }

    fn book(opf: &str, extra: &[(&str, &[u8])]) -> Result<Book, String> {
        let container = r#"<?xml version="1.0"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles></container>"#;
        let mut entries: Vec<(&str, &[u8])> = vec![
            ("META-INF/container.xml", container.as_bytes()),
            ("OEBPS/content.opf", opf.as_bytes()),
        ];
        entries.extend_from_slice(extra);
        Epub::from_reader(zip_of(&entries))
    }

    const OPF: &str = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" xmlns:opf="http://www.idpf.org/2007/opf" version="2.0">
  <metadata/>
  <manifest>
    <item id="a" href="a.xhtml" media-type="application/xhtml+xml"/>
    <item id="b" href="text/b%20c.xhtml" media-type="application/xhtml+xml"/>
    <item id="nomime" href="a.xhtml"/>
    <item id="gone" href="gone.xhtml" media-type="application/xhtml+xml"/>
    <item id="dup" href="a.xhtml" media-type="application/xhtml+xml"/>
    <item id="dup" href="text/b c.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine toc="ncx"><itemref idref="b"/><itemref/><itemref opf:idref="a"/><itemref idref="gone"/></spine>
</package>"#;

    #[test]
    fn reads_the_spine_in_order_and_resources_by_id() {
        let mut e = book(
            OPF,
            &[("OEBPS/a.xhtml", b"A"), ("OEBPS/text/b c.xhtml", b"B")],
        )
        .unwrap();
        assert_eq!(e.spine(), ["b", "a", "gone"]);
        assert_eq!(e.resource("a").as_deref(), Some(&b"A"[..]));
        assert_eq!(e.resource("b").as_deref(), Some(&b"B"[..]));
        assert_eq!(e.resource("gone"), None);
        assert_eq!(e.resource("nomime"), None);
        assert_eq!(e.resource("dup").as_deref(), Some(&b"B"[..]));
    }

    #[test]
    fn a_book_without_a_manifest_spine_or_metadata_is_an_error() {
        for (open, close) in [
            ("<metadata/>", None),
            ("<manifest>", Some("</manifest>")),
            ("<spine toc=\"ncx\">", Some("</spine>")),
        ] {
            let mut opf = OPF.replace(open, "<other>");
            opf = match close {
                Some(c) => opf.replace(c, "</other>"),
                None => opf.replace("<other>", "<other/>"),
            };
            assert!(book(&opf, &[]).is_err(), "{open} removed, yet it opened");
        }
        assert!(book(OPF, &[]).is_ok());
    }

    #[test]
    fn broken_input_is_an_error_not_a_panic() {
        assert!(Epub::from_reader(Cursor::new(b"not a zip".to_vec())).is_err());
        assert!(Epub::open(Path::new("/nonexistent/absent.epub")).is_err());
        assert!(Epub::from_reader(zip_of(&[("mimetype", b"x")])).is_err());
        for opf in ["", "<", "\u{feff}", "<package"] {
            assert!(book(opf, &[]).is_err(), "{opf:?} opened");
        }
        let empty_path =
            r#"<container><rootfiles><rootfile full-path=""/></rootfiles></container>"#;
        let z = zip_of(&[("META-INF/container.xml", empty_path.as_bytes())]);
        assert!(Epub::from_reader(z).is_err());
    }

    #[test]
    fn a_utf16_package_document_with_a_bom_is_read() {
        let mut le = vec![0xFF, 0xFE];
        for u in OPF.replace("<?xml version=\"1.0\"?>", "").encode_utf16() {
            le.extend_from_slice(&u.to_le_bytes());
        }
        let container =
            br#"<container><rootfiles><rootfile full-path="content.opf"/></rootfiles></container>"#;
        let z = zip_of(&[
            ("META-INF/container.xml", container),
            ("content.opf", &le),
            ("a.xhtml", b"A"),
        ]);
        let mut e = Epub::from_reader(z).unwrap();
        assert_eq!(e.spine(), ["b", "a", "gone"]);
        assert_eq!(e.resource("a").as_deref(), Some(&b"A"[..]));
    }

    #[test]
    fn percent_decoding_keeps_a_stray_percent() {
        assert_eq!(percent_decode("a%20b").as_deref(), Some("a b"));
        assert_eq!(percent_decode("100%").as_deref(), Some("100%"));
        assert_eq!(percent_decode("%zz%4").as_deref(), Some("%zz%4"));
        assert_eq!(percent_decode("%C3%A9").as_deref(), Some("é"));
        assert_eq!(percent_decode("%FF"), None);
    }
}
