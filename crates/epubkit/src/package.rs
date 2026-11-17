//! [`Package`]: one EPUB, opened once.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek};
use std::path::{Path, PathBuf};

use crate::archive::Archive;
use crate::error::{Error, Warning};
use crate::metadata::{self, Metadata};
use crate::{Limits, path, xml};

/// The path every EPUB keeps its container document at.
const CONTAINER: &str = "META-INF/container.xml";

/// One manifest `item`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ManifestItem {
    /// `id`.
    pub id: String,
    /// `href`, as written.
    pub href: String,
    /// Not percent-decoded or dot-folded: reads try both, but this is the name as the book gives it.
    pub path: String,
    /// `media-type`.
    pub media_type: String,
    /// EPUB 3 `properties`, as written (space-separated).
    pub properties: Option<String>,
    /// `fallback`.
    pub fallback: Option<String>,
}

impl ManifestItem {
    /// Whether `properties` lists this property.
    pub fn has_property(&self, property: &str) -> bool {
        self.properties
            .as_deref()
            .is_some_and(|p| p.split_ascii_whitespace().any(|t| t == property))
    }
}

/// One spine `itemref`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SpineItem {
    /// `idref`: the manifest id this position reads.
    pub idref: String,
    /// `id`.
    pub id: Option<String>,
    /// Absent counts as `yes`; any value other than `yes` makes it false.
    pub linear: bool,
    /// EPUB 3 `properties`, as written.
    pub properties: Option<String>,
}

/// The spine: reading order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Spine {
    /// The itemrefs, in reading order.
    pub items: Vec<SpineItem>,
    /// EPUB 2's `toc` attribute: the manifest id of the NCX.
    pub toc: Option<String>,
    /// `page-progression-direction`.
    pub page_progression_direction: Option<String>,
}

/// The cover image.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Cover {
    /// Its manifest id.
    pub id: String,
    /// The book's declared media type, not sniffed from the bytes.
    pub media_type: String,
    /// The image bytes.
    pub bytes: Vec<u8>,
}

/// An opened EPUB. Reads take `&mut self` because reading a zip moves its cursor.
pub struct Package<R = BufReader<File>> {
    archive: Archive<R>,
    opf_path: String,
    opf_dir: String,
    version: Option<String>,
    unique_identifier: Option<String>,
    /// Complete items, in document order, duplicates of an id kept.
    manifest: Vec<ManifestItem>,
    /// id -> index of the *last* manifest item with that id.
    by_id: HashMap<String, usize>,
    spine: Spine,
    /// idref → its first spine position.
    spine_by_id: HashMap<String, usize>,
    /// Manifest path, compared by path components -> earliest spine position that reads it.
    spine_by_path: HashMap<PathBuf, usize>,
    /// Same, keyed on the percent-decoded, dot-folded path: the fallback when components do not match.
    spine_by_folded: HashMap<String, usize>,
    metadata: Metadata,
    cover_id: Option<String>,
    warnings: Vec<Warning>,
}

impl<R> std::fmt::Debug for Package<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Package")
            .field("opf_path", &self.opf_path)
            .field("version", &self.version)
            .field("manifest", &self.manifest.len())
            .field("spine", &self.spine.items.len())
            .field("warnings", &self.warnings.len())
            .finish_non_exhaustive()
    }
}

impl Package<BufReader<File>> {
    /// Open the EPUB at `path` with the default [`Limits`].
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        Self::open_with(path, Limits::default())
    }

    /// Open the EPUB at `path` under `limits`.
    pub fn open_with(path: impl AsRef<Path>, limits: Limits) -> Result<Self, Error> {
        let file = File::open(path)?;
        Self::from_reader_with(BufReader::new(file), limits)
    }
}

impl<'a> Package<Cursor<&'a [u8]>> {
    /// Open an EPUB held in memory, with the default [`Limits`].
    pub fn from_bytes(bytes: &'a [u8]) -> Result<Self, Error> {
        Self::from_bytes_with(bytes, Limits::default())
    }

    /// Open an EPUB held in memory, under `limits`.
    pub fn from_bytes_with(bytes: &'a [u8], limits: Limits) -> Result<Self, Error> {
        Self::from_reader_with(Cursor::new(bytes), limits)
    }
}

impl<R: Read + Seek> Package<R> {
    /// Open an EPUB from any seekable reader, with the default [`Limits`].
    pub fn from_reader(reader: R) -> Result<Self, Error> {
        Self::from_reader_with(reader, Limits::default())
    }

    /// Open an EPUB from any seekable reader, under `limits`.
    #[expect(clippy::too_many_lines)]
    pub fn from_reader_with(reader: R, limits: Limits) -> Result<Self, Error> {
        let mut archive = Archive::new(reader, limits)?;
        let mut warnings = Vec::new();

        // Only the first `<rootfile>` is read; `full-path` is relative to the archive root, not the OPF.
        let container = archive.read(CONTAINER).map_err(|e| match e {
            Error::MissingEntry { .. } => Error::MissingContainer,
            other => other,
        })?;
        let container = parse_xml(&container, CONTAINER, &limits, &mut warnings)?;
        let opf_path = container
            .root
            .find("rootfile")
            .and_then(|r| r.attr("full-path"))
            .ok_or(Error::NoRootfile)?
            .to_string();
        let opf_dir = path::parent_dir(&opf_path);

        let opf = archive.read(&opf_path)?;
        let opf = parse_xml(&opf, &opf_path, &limits, &mut warnings)?;
        let root = &opf.root;
        let version = root.attr("version").map(str::to_string);
        let unique_id = root.attr("unique-identifier").map(str::to_string);

        // On a duplicate id the later item wins lookups; both stay in `manifest`.
        let manifest_node = root.find("manifest").ok_or(Error::MissingElement {
            element: "manifest",
        })?;
        let mut manifest: Vec<ManifestItem> = Vec::new();
        let mut by_id: HashMap<String, usize> = HashMap::new();
        for (index, item) in manifest_node.children().enumerate() {
            let missing = ["id", "href", "media-type"]
                .into_iter()
                .find(|a| item.attr(a).is_none());
            if let Some(missing) = missing {
                warnings.push(Warning::ManifestItemIncomplete { index, missing });
                continue;
            }
            let id = item.attr("id").unwrap_or_default().to_string();
            let href = item.attr("href").unwrap_or_default().to_string();
            if by_id.insert(id.clone(), manifest.len()).is_some() {
                warnings.push(Warning::DuplicateManifestId { id: id.clone() });
            }
            manifest.push(ManifestItem {
                path: path::join(&opf_dir, &href),
                id,
                href,
                media_type: item.attr("media-type").unwrap_or_default().to_string(),
                properties: item.attr("properties").map(str::to_string),
                fallback: item.attr("fallback").map(str::to_string),
            });
        }
        for m in &manifest {
            if !archive.contains(&m.path) {
                warnings.push(Warning::ManifestItemMissing {
                    id: m.id.clone(),
                    path: m.path.clone(),
                });
            }
        }

        // An idref listed twice maps to its first position.
        let spine_node = root
            .find("spine")
            .ok_or(Error::MissingElement { element: "spine" })?;
        let mut spine = Spine {
            items: Vec::new(),
            toc: spine_node.attr("toc").map(str::to_string),
            page_progression_direction: spine_node
                .attr("page-progression-direction")
                .map(str::to_string),
        };
        let mut spine_by_id: HashMap<String, usize> = HashMap::new();
        for (index, item) in spine_node.children().enumerate() {
            let Some(idref) = item.attr("idref") else {
                warnings.push(Warning::SpineItemIncomplete { index });
                continue;
            };
            if !by_id.contains_key(idref) {
                warnings.push(Warning::SpineIdrefUnknown {
                    idref: idref.to_string(),
                });
            }
            spine_by_id
                .entry(idref.to_string())
                .or_insert(spine.items.len());
            spine.items.push(SpineItem {
                idref: idref.to_string(),
                id: item.attr("id").map(str::to_string),
                linear: item.attr("linear").unwrap_or("yes") == "yes",
                properties: item.attr("properties").map(str::to_string),
            });
        }

        // Only the item an id lookup answers counts: a shadowed duplicate is not what idref reads.
        let mut spine_by_path: HashMap<PathBuf, usize> = HashMap::new();
        let mut spine_by_folded: HashMap<String, usize> = HashMap::new();
        for (i, m) in manifest.iter().enumerate() {
            if by_id.get(&m.id) != Some(&i) {
                continue;
            }
            let Some(&position) = spine_by_id.get(&m.id) else {
                continue;
            };
            let earliest = |slot: &mut usize| *slot = (*slot).min(position);
            spine_by_path
                .entry(PathBuf::from(&m.path))
                .and_modify(earliest)
                .or_insert(position);
            if let Some(folded) = path::folded(&m.path) {
                spine_by_folded
                    .entry(folded)
                    .and_modify(earliest)
                    .or_insert(position);
            }
        }

        let metadata_node = root.find("metadata").ok_or(Error::MissingElement {
            element: "metadata",
        })?;
        let parsed = metadata::parse(metadata_node, unique_id.as_deref(), &|id| {
            by_id.contains_key(id)
        });
        warnings.extend(parsed.warnings);

        // EPUB 2's `<meta name="cover">` wins when it names a manifest item; else EPUB 3 `cover-image`.
        let mut cover_id = None;
        if let Some(meta) = parsed.cover_meta {
            if by_id.contains_key(&meta) {
                cover_id = Some(meta);
            } else {
                warnings.push(Warning::CoverMetaUnknownId { id: meta });
            }
        }
        if cover_id.is_none() {
            cover_id = manifest
                .iter()
                .find(|m| m.has_property("cover-image"))
                .map(|m| m.id.clone());
        }

        Ok(Package {
            archive,
            opf_path,
            opf_dir,
            version,
            unique_identifier: parsed.unique_identifier,
            manifest,
            by_id,
            spine,
            spine_by_id,
            spine_by_path,
            spine_by_folded,
            metadata: parsed.metadata,
            cover_id,
            warnings,
        })
    }

    /// Read a manifest item's bytes by its id.
    pub fn resource(&mut self, id: &str) -> Result<Vec<u8>, Error> {
        let path = self
            .manifest_item(id)
            .ok_or_else(|| Error::UnknownId { id: id.to_string() })?
            .path
            .clone();
        self.archive.read(&path)
    }

    /// Reads an archive entry: exactly, then percent-decoded, then with `.`/`..` folded.
    pub fn resource_by_path(&mut self, path: &str) -> Result<Vec<u8>, Error> {
        self.archive.read(path)
    }

    /// A named but unreadable cover records `Warning::CoverUnreadable`.
    pub fn cover(&mut self) -> Option<Cover> {
        let id = self.cover_id.clone()?;
        let media_type = self.manifest_item(&id)?.media_type.clone();
        match self.resource(&id) {
            Ok(bytes) => Some(Cover {
                id,
                media_type,
                bytes,
            }),
            Err(_) => {
                self.warnings.push(Warning::CoverUnreadable { id });
                None
            }
        }
    }

    /// Every archive entry name, in the zip's own order.
    pub fn entry_names(&self) -> Vec<String> {
        self.archive.names().collect()
    }

    /// The limits this package reads under.
    pub fn limits(&self) -> &Limits {
        self.archive.limits()
    }

    pub(crate) fn archive_mut(&mut self) -> &mut Archive<R> {
        &mut self.archive
    }

    pub(crate) fn warn(&mut self, w: Warning) {
        self.warnings.push(w);
    }
}

impl<R> Package<R> {
    /// The archive path of the package document (`OEBPS/content.opf`).
    pub fn opf_path(&self) -> &str {
        &self.opf_path
    }

    /// The directory the package document sits in; hrefs resolve against it.
    pub fn opf_dir(&self) -> &str {
        &self.opf_dir
    }

    /// The package's `version` attribute, as written (`"2.0"`, `"3.0"`).
    pub fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    /// The identifier's value, untrimmed.
    pub fn unique_identifier(&self) -> Option<&str> {
        self.unique_identifier.as_deref()
    }

    /// The metadata.
    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    /// Only items carrying an id, href and media type.
    pub fn manifest(&self) -> &[ManifestItem] {
        &self.manifest
    }

    /// The last item when an id is repeated.
    pub fn manifest_item(&self, id: &str) -> Option<&ManifestItem> {
        self.by_id.get(id).and_then(|&i| self.manifest.get(i))
    }

    /// The spine.
    pub fn spine(&self) -> &Spine {
        &self.spine
    }

    /// The first spine position that reads the manifest item with this id.
    pub fn spine_index_of_id(&self, id: &str) -> Option<usize> {
        self.spine_by_id.get(id).copied()
    }

    /// Matches by path components first, then percent-decoded with `.`/`..` folded; the first spine
    /// position wins when several items share a path.
    pub fn spine_index_of_path(&self, path: &str) -> Option<usize> {
        self.spine_by_path
            .get(Path::new(path))
            .or_else(|| path::folded(path).and_then(|folded| self.spine_by_folded.get(&folded)))
            .copied()
    }

    /// The cover's manifest id, when the book names one that exists.
    pub fn cover_id(&self) -> Option<&str> {
        self.cover_id.as_deref()
    }

    /// What was wrong but survivable, in the order it was found.
    pub fn warnings(&self) -> &[Warning] {
        &self.warnings
    }
}

/// Parse a well-formed XML document, turning unexpanded entities into warnings.
pub(crate) fn parse_xml(
    bytes: &[u8],
    path: &str,
    limits: &Limits,
    warnings: &mut Vec<Warning>,
) -> Result<xml::Document, Error> {
    let doc = xml::parse(bytes, path, limits)?;
    report(&doc, path, warnings);
    Ok(doc)
}

/// Parse an XHTML document leniently, turning unexpanded entities and every repair into
/// warnings.
pub(crate) fn parse_html(
    bytes: &[u8],
    path: &str,
    limits: &Limits,
    warnings: &mut Vec<Warning>,
) -> Result<xml::Document, Error> {
    let doc = xml::parse_html(bytes, path, limits)?;
    report(&doc, path, warnings);
    Ok(doc)
}

fn report(doc: &xml::Document, path: &str, warnings: &mut Vec<Warning>) {
    warnings.extend(
        doc.undeclared_entities
            .iter()
            .map(|name| Warning::UndeclaredEntity {
                path: path.to_string(),
                name: name.clone(),
            }),
    );
    warnings.extend(
        doc.problems
            .iter()
            .map(|&problem| Warning::MarkupRecovered {
                path: path.to_string(),
                problem,
            }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SeriesSource;
    use crate::error::Limit;
    use crate::testkit::{container, minimal_book, opf, zip_of};

    fn book(metadata: &str, manifest: &str, spine: &str, extra: &[(&str, &[u8])]) -> Vec<u8> {
        let opf = opf("3.0", metadata, manifest, spine);
        let container = container("OEBPS/content.opf");
        let mut entries: Vec<(&str, &[u8])> = vec![
            ("mimetype", b"application/epub+zip"),
            ("META-INF/container.xml", container.as_bytes()),
            ("OEBPS/content.opf", opf.as_bytes()),
        ];
        entries.extend_from_slice(extra);
        zip_of(&entries)
    }

    const CH1: &str = r#"<item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/>"#;
    const SPINE: &str = r#"<spine><itemref idref="ch1"/></spine>"#;
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nfake";

    #[test]
    fn reads_the_minimal_book() {
        let bytes =
            minimal_book(r#"<dc:title>T</dc:title><dc:identifier id="uid">x</dc:identifier>"#);
        let mut p = Package::from_bytes(&bytes).unwrap();
        assert_eq!(p.version(), Some("3.0"));
        assert_eq!(p.opf_dir(), "OEBPS");
        assert_eq!(p.unique_identifier(), Some("x"));
        assert_eq!(p.spine().items.len(), 1);
        assert_eq!(p.manifest_item("ch1").unwrap().path, "OEBPS/ch1.xhtml");
        assert_eq!(p.spine_index_of_path("OEBPS/ch1.xhtml"), Some(0));
        assert!(p.resource("ch1").unwrap().starts_with(b"<html>"));
        assert!(p.warnings().is_empty(), "{:?}", p.warnings());
    }

    #[test]
    fn not_a_zip_is_a_zip_error() {
        assert!(matches!(
            Package::from_bytes(b"plainly not a zip archive"),
            Err(Error::Zip(_))
        ));
    }

    #[test]
    fn a_missing_container_is_its_own_error() {
        let bytes = zip_of(&[("mimetype", b"application/epub+zip")]);
        assert!(matches!(
            Package::from_bytes(&bytes),
            Err(Error::MissingContainer)
        ));
    }

    #[test]
    fn a_missing_package_document_names_its_path() {
        let c = container("OEBPS/content.opf");
        let bytes = zip_of(&[("META-INF/container.xml", c.as_bytes())]);
        match Package::from_bytes(&bytes) {
            Err(Error::MissingEntry { path }) => assert_eq!(path, "OEBPS/content.opf"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_container_without_a_rootfile_is_refused() {
        let bytes = zip_of(&[(
            "META-INF/container.xml",
            b"<container><rootfiles/></container>",
        )]);
        assert!(matches!(
            Package::from_bytes(&bytes),
            Err(Error::NoRootfile)
        ));
    }

    /// An empty `full-path` is an error.
    #[test]
    fn an_empty_rootfile_path_is_an_error_not_a_panic() {
        let bytes = zip_of(&[(
            "META-INF/container.xml",
            br#"<container><rootfiles><rootfile full-path=""/></rootfiles></container>"#,
        )]);
        assert!(Package::from_bytes(&bytes).is_err());
    }

    #[test]
    fn a_package_without_a_spine_is_refused() {
        let bytes = book("<dc:title>T</dc:title>", CH1, "", &[]);
        assert!(matches!(
            Package::from_bytes(&bytes),
            Err(Error::MissingElement { element: "spine" })
        ));
    }

    #[test]
    fn a_manifest_item_absent_from_the_archive_is_a_warning() {
        let bytes = book("<dc:title>T</dc:title>", CH1, SPINE, &[]);
        let mut p = Package::from_bytes(&bytes).unwrap();
        assert_eq!(
            p.warnings(),
            &[Warning::ManifestItemMissing {
                id: "ch1".into(),
                path: "OEBPS/ch1.xhtml".into()
            }]
        );
        assert!(matches!(p.resource("ch1"), Err(Error::MissingEntry { .. })));
        assert!(matches!(p.resource("nope"), Err(Error::UnknownId { .. })));
    }

    #[test]
    fn incomplete_items_and_unknown_idrefs_are_warnings() {
        let manifest = format!(r#"{CH1}<item id="x" href="x.xhtml"/>"#);
        let spine = r#"<spine><itemref idref="ch1"/><itemref idref="ghost"/><itemref/></spine>"#;
        let bytes = book("", &manifest, spine, &[("OEBPS/ch1.xhtml", b"<html/>")]);
        let p = Package::from_bytes(&bytes).unwrap();
        assert!(p.warnings().contains(&Warning::ManifestItemIncomplete {
            index: 1,
            missing: "media-type"
        }));
        assert!(p.warnings().contains(&Warning::SpineIdrefUnknown {
            idref: "ghost".into()
        }));
        assert!(
            p.warnings()
                .contains(&Warning::SpineItemIncomplete { index: 2 })
        );
        assert_eq!(p.spine().items.len(), 2);
    }

    #[test]
    fn a_percent_encoded_href_finds_its_file() {
        let manifest =
            r#"<item id="ch" href="my%20chapter.xhtml" media-type="application/xhtml+xml"/>"#;
        let bytes = book(
            "",
            manifest,
            r#"<spine><itemref idref="ch"/></spine>"#,
            &[("OEBPS/my chapter.xhtml", b"<html>found</html>")],
        );
        let mut p = Package::from_bytes(&bytes).unwrap();
        assert!(p.warnings().is_empty(), "{:?}", p.warnings());
        assert_eq!(
            p.manifest_item("ch").unwrap().path,
            "OEBPS/my%20chapter.xhtml"
        );
        assert_eq!(p.resource("ch").unwrap(), b"<html>found</html>");
    }

    #[test]
    fn the_epub3_cover_image_property_names_the_cover() {
        let manifest = format!(
            r#"{CH1}<item id="img" href="c.png" media-type="image/png" properties="svg cover-image"/>"#
        );
        let bytes = book("", &manifest, SPINE, &[("OEBPS/c.png", PNG)]);
        let mut p = Package::from_bytes(&bytes).unwrap();
        let c = p.cover().unwrap();
        assert_eq!((c.id.as_str(), c.media_type.as_str()), ("img", "image/png"));
        assert_eq!(c.bytes, PNG);
    }

    #[test]
    fn the_epub2_cover_meta_names_the_cover_and_wins() {
        let manifest = format!(
            r#"{CH1}<item id="a" href="a.png" media-type="image/png" properties="cover-image"/><item id="b" href="b.png" media-type="image/png"/>"#
        );
        let bytes = book(
            r#"<meta name="cover" content="b"/>"#,
            &manifest,
            SPINE,
            &[("OEBPS/a.png", b"A"), ("OEBPS/b.png", b"B")],
        );
        let mut p = Package::from_bytes(&bytes).unwrap();
        assert_eq!(p.cover_id(), Some("b"));
        assert_eq!(p.cover().unwrap().bytes, b"B");
    }

    #[test]
    fn an_unreadable_cover_is_none_and_a_warning() {
        let manifest = format!(r#"{CH1}<item id="img" href="c.png" media-type="image/png"/>"#);
        let bytes = book(
            r#"<meta name="cover" content="img"/>"#,
            &manifest,
            SPINE,
            &[],
        );
        let mut p = Package::from_bytes(&bytes).unwrap();
        assert_eq!(p.cover(), None);
        assert!(
            p.warnings()
                .contains(&Warning::CoverUnreadable { id: "img".into() })
        );
    }

    #[test]
    fn an_entry_bigger_than_the_cap_is_refused_while_reading() {
        let big = vec![0u8; 2 * 1024 * 1024];
        let manifest = format!(
            r#"{CH1}<item id="big" href="big.bin" media-type="application/octet-stream"/>"#
        );
        let bytes = book("", &manifest, SPINE, &[("OEBPS/big.bin", &big)]);
        let limits = Limits::default().with_max_entry_bytes(1024 * 1024);
        let mut p = Package::from_bytes_with(&bytes, limits).unwrap();
        assert!(matches!(
            p.resource("big"),
            Err(Error::LimitExceeded {
                limit: Limit::EntryBytes,
                ..
            })
        ));
    }

    /// The archive declares 10 bytes but the read must still stop at the cap.
    #[test]
    fn a_lying_declared_size_does_not_lift_the_cap() {
        let big = vec![b'a'; 4 * 1024 * 1024];
        let manifest = format!(
            r#"{CH1}<item id="big" href="big.bin" media-type="application/octet-stream"/>"#
        );
        let mut bytes = book("", &manifest, SPINE, &[("OEBPS/big.bin", &big)]);
        let name = b"OEBPS/big.bin";
        let mut patched = 0;
        let mut i = 0;
        while i + 4 <= bytes.len() {
            let sig = &bytes[i..i + 4];
            let (size_at, name_at) = if sig == b"PK\x03\x04" {
                (i + 22, i + 30)
            } else if sig == b"PK\x01\x02" {
                (i + 24, i + 46)
            } else {
                i += 1;
                continue;
            };
            if bytes.get(name_at..name_at + name.len()) == Some(&name[..]) {
                bytes[size_at..size_at + 4].copy_from_slice(&10u32.to_le_bytes());
                patched += 1;
            }
            i += 4;
        }
        assert_eq!(patched, 2, "both headers patched");
        let limits = Limits::default().with_max_entry_bytes(1024 * 1024);
        if let Ok(mut p) = Package::from_bytes_with(&bytes, limits)
            && let Ok(b) = p.resource("big")
        {
            assert!(b.len() as u64 <= 1024 * 1024, "read {} bytes", b.len());
        }
    }

    #[test]
    fn the_total_budget_spans_reads() {
        let chunk = vec![1u8; 600 * 1024];
        let manifest = format!(
            r#"{CH1}<item id="a" href="a.bin" media-type="x/y"/><item id="b" href="b.bin" media-type="x/y"/>"#
        );
        let bytes = book(
            "",
            &manifest,
            SPINE,
            &[("OEBPS/a.bin", &chunk), ("OEBPS/b.bin", &chunk)],
        );
        let limits = Limits::default().with_max_total_bytes(1024 * 1024);
        let mut p = Package::from_bytes_with(&bytes, limits).unwrap();
        assert!(p.resource("a").is_ok());
        assert!(matches!(
            p.resource("b"),
            Err(Error::LimitExceeded {
                limit: Limit::TotalBytes,
                ..
            })
        ));
    }

    #[test]
    fn too_many_entries_is_refused_at_open() {
        let names: Vec<String> = (0..20).map(|i| format!("f{i}")).collect();
        let entries: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), &b"x"[..])).collect();
        let bytes = zip_of(&entries);
        let limits = Limits::default().with_max_entries(10);
        assert!(matches!(
            Package::from_bytes_with(&bytes, limits),
            Err(Error::LimitExceeded {
                limit: Limit::Entries,
                ..
            })
        ));
    }

    #[test]
    fn a_package_nested_past_the_depth_cap_is_refused() {
        let deep = format!("{}{}", "<x>".repeat(400), "</x>".repeat(400));
        let bytes = book(&deep, CH1, SPINE, &[]);
        assert!(matches!(
            Package::from_bytes(&bytes),
            Err(Error::LimitExceeded {
                limit: Limit::XmlDepth,
                ..
            })
        ));
    }

    #[test]
    fn a_dtd_entity_bomb_in_the_package_is_not_expanded() {
        let mut dtd = String::from("<!DOCTYPE package [<!ENTITY a \"aaaaaaaaaa\">");
        for i in 1..12 {
            let prev = if i == 1 {
                "a".to_string()
            } else {
                format!("a{}", i - 1)
            };
            dtd.push_str(&format!(
                "<!ENTITY a{i} \"{}\">",
                format!("&{prev};").repeat(10)
            ));
        }
        dtd.push_str("]>");
        let opf = opf("3.0", "<dc:title>&a11;</dc:title>", CH1, SPINE).replacen(
            "<package",
            &format!("{dtd}<package"),
            1,
        );
        let c = container("OEBPS/content.opf");
        let bytes = zip_of(&[
            ("META-INF/container.xml", c.as_bytes()),
            ("OEBPS/content.opf", opf.as_bytes()),
            ("OEBPS/ch1.xhtml", b"<html/>"),
        ]);
        let p = Package::from_bytes(&bytes).unwrap();
        assert_eq!(p.metadata().titles[0].value, "&a11;");
        assert!(p.warnings().contains(&Warning::UndeclaredEntity {
            path: "OEBPS/content.opf".into(),
            name: "a11".into()
        }));
    }

    #[test]
    fn a_utf16_package_document_with_a_bom_is_read() {
        let text =
            opf("3.0", "<dc:title>Ünïcode</dc:title>", CH1, SPINE).replace("UTF-8", "UTF-16");
        for big_endian in [false, true] {
            let mut utf16 = if big_endian {
                vec![0xFE, 0xFF]
            } else {
                vec![0xFF, 0xFE]
            };
            for u in text.encode_utf16() {
                utf16.extend_from_slice(&if big_endian {
                    u.to_be_bytes()
                } else {
                    u.to_le_bytes()
                });
            }
            let c = container("OEBPS/content.opf");
            let bytes = zip_of(&[
                ("META-INF/container.xml", c.as_bytes()),
                ("OEBPS/content.opf", &utf16),
                ("OEBPS/ch1.xhtml", b"<html/>"),
            ]);
            let p = Package::from_bytes(&bytes).unwrap();
            assert_eq!(p.metadata().main_title(), Some("Ünïcode"));
        }
    }

    #[test]
    fn refines_carry_role_file_as_and_identifier_type() {
        let md = r##"
            <dc:title id="t">Main</dc:title>
            <meta refines="#t" property="title-type">main</meta>
            <dc:creator id="c1">Ursula K. Le Guin</dc:creator>
            <meta refines="#c1" property="role" scheme="marc:relators">aut</meta>
            <meta refines="#c1" property="file-as">Le Guin, Ursula K.</meta>
            <meta refines="#c1" property="display-seq">1</meta>
            <dc:identifier id="uid">9780441478125</dc:identifier>
            <meta refines="#uid" property="identifier-type" scheme="onix:codelist5">15</meta>
            <meta refines="#ghost" property="role">ill</meta>
            <meta property="dcterms:modified">2020-01-01T00:00:00Z</meta>"##;
        let bytes = minimal_book(md);
        let p = Package::from_bytes(&bytes).unwrap();
        let m = p.metadata();
        assert_eq!(m.titles[0].title_type.as_deref(), Some("main"));
        let c = &m.creators[0];
        assert_eq!(c.role.as_deref(), Some("aut"));
        assert_eq!(c.file_as.as_deref(), Some("Le Guin, Ursula K."));
        assert_eq!(c.display_seq, Some(1));
        let id = &m.identifiers[0];
        assert!(id.is_unique);
        assert_eq!(id.scheme.as_deref(), Some("15"));
        assert!(id.scheme_says_isbn());
        assert_eq!(m.modified.as_deref(), Some("2020-01-01T00:00:00Z"));
        assert_eq!(
            p.warnings(),
            &[Warning::UnresolvedRefines {
                refines: "#ghost".into()
            }]
        );
    }

    #[test]
    fn epub2_attributes_carry_role_file_as_and_scheme() {
        let md = r#"
            <dc:creator opf:role="aut" opf:file-as="Austen, Jane">Jane Austen</dc:creator>
            <dc:identifier id="uid" opf:scheme="ISBN">0141439513</dc:identifier>
            <dc:identifier>urn:uuid:1234</dc:identifier>
            <dc:date opf:event="publication">1813</dc:date>"#;
        let bytes = minimal_book(md);
        let p = Package::from_bytes(&bytes).unwrap();
        let m = p.metadata();
        assert_eq!(m.creators[0].role.as_deref(), Some("aut"));
        assert_eq!(m.creators[0].file_as.as_deref(), Some("Austen, Jane"));
        assert!(m.identifiers[0].scheme_says_isbn());
        assert_eq!(m.identifiers[1].scheme.as_deref(), Some("uuid"));
        assert!(!m.identifiers[1].scheme_says_isbn());
        assert_eq!(m.publication_date(), Some("1813"));
    }

    #[test]
    fn series_by_both_conventions() {
        let md = r##"
            <meta property="belongs-to-collection" id="s">Earthsea</meta>
            <meta refines="#s" property="collection-type">series</meta>
            <meta refines="#s" property="group-position">2</meta>
            <meta name="calibre:series" content="Earthsea Cycle"/>
            <meta name="calibre:series_index" content="2.0"/>"##;
        let bytes = minimal_book(md);
        let p = Package::from_bytes(&bytes).unwrap();
        let s = &p.metadata().series;
        assert_eq!(s.len(), 2);
        assert_eq!(
            (
                s[0].name.as_str(),
                s[0].position.as_deref(),
                s[0].collection_type.as_deref(),
                s[0].source
            ),
            (
                "Earthsea",
                Some("2"),
                Some("series"),
                SeriesSource::Collection
            )
        );
        assert_eq!(
            (s[1].name.as_str(), s[1].position.as_deref(), s[1].source),
            ("Earthsea Cycle", Some("2.0"), SeriesSource::Calibre)
        );
        assert!(p.warnings().is_empty(), "{:?}", p.warnings());
    }

    #[test]
    fn the_flat_view_matches_the_old_crate_rules() {
        let md = r##"
            <dc:title>  Spaced  </dc:title>
            <dc:title><![CDATA[In CDATA]]></dc:title>
            <meta name="calibre:series" content="S"/>
            <meta property="dcterms:modified">2020</meta>
            <meta refines="#x"/>"##;
        let bytes = minimal_book(md);
        let p = Package::from_bytes(&bytes).unwrap();
        let m = p.metadata();
        let titles: Vec<&str> = m.values("title").collect();
        // The flat view is untrimmed and does not count CDATA as text.
        assert_eq!(titles, vec!["  Spaced  ", ""]);
        assert_eq!(m.values("calibre:series").collect::<Vec<_>>(), vec!["S"]);
        assert_eq!(
            m.values("dcterms:modified").collect::<Vec<_>>(),
            vec!["2020"]
        );
        assert_eq!(m.titles[1].value, "In CDATA");
        assert_eq!(m.titles[0].value, "Spaced");
    }

    #[test]
    fn the_ncx_is_read_by_play_order() {
        let ncx = r#"<?xml version="1.0"?>
<!DOCTYPE ncx PUBLIC "-//NISO//DTD ncx 2005-1//EN" "http://www.daisy.org/z3986/2005/ncx-2005-1.dtd">
<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"><navMap>
  <navPoint id="b" playOrder="3"><navLabel><text>Three</text></navLabel><content src="ch1.xhtml#c"/></navPoint>
  <navPoint id="a" playOrder="1"><navLabel><text> One </text></navLabel><content src="ch1.xhtml"/>
    <navPoint id="a1" playOrder="2"><navLabel><text>One.1</text></navLabel><content src="ch1.xhtml#x"/></navPoint>
  </navPoint>
</navMap></ncx>"#;
        let manifest = format!(
            r#"{CH1}<item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>"#
        );
        let bytes = book(
            "",
            &manifest,
            r#"<spine toc="ncx"><itemref idref="ch1"/></spine>"#,
            &[
                ("OEBPS/ch1.xhtml", b"<html/>"),
                ("OEBPS/toc.ncx", ncx.as_bytes()),
            ],
        );
        let mut p = Package::from_bytes(&bytes).unwrap();
        let toc = p.ncx_toc();
        let shape: Vec<(&str, Option<usize>, Option<String>, usize)> = toc
            .iter()
            .map(|n| (n.label.as_str(), n.play_order, n.target(), n.children.len()))
            .collect();
        assert_eq!(
            shape,
            vec![
                ("One", Some(1), Some("OEBPS/ch1.xhtml".into()), 1),
                ("Three", Some(3), Some("OEBPS/ch1.xhtml#c".into()), 0)
            ]
        );
        assert_eq!(
            toc[0].children[0].target().as_deref(),
            Some("OEBPS/ch1.xhtml#x")
        );
    }

    /// The spine index answers the same for every spelling of a path that compares equal by
    /// component, and the earliest spine position wins.
    #[test]
    fn spine_index_of_path_by_component_then_folded() {
        let manifest = r#"<item id="a" href="t/a.xhtml" media-type="application/xhtml+xml"/>
            <item id="a2" href="t//a.xhtml" media-type="application/xhtml+xml"/>
            <item id="b" href="t/b%20c.xhtml" media-type="application/xhtml+xml"/>"#;
        let bytes = book(
            "",
            manifest,
            r#"<spine><itemref idref="b"/><itemref idref="a2"/><itemref idref="a"/></spine>"#,
            &[],
        );
        let p = Package::from_bytes(&bytes).unwrap();
        for spelling in ["OEBPS/t/a.xhtml", "OEBPS/./t/a.xhtml", "OEBPS//t/a.xhtml/"] {
            assert_eq!(p.spine_index_of_path(spelling), Some(1), "{spelling}");
        }
        assert_eq!(p.spine_index_of_path("OEBPS/x/../t/a.xhtml"), Some(1));
        assert_eq!(p.spine_index_of_path("OEBPS/t/b c.xhtml"), Some(0));
        assert_eq!(p.spine_index_of_path("OEBPS/t/b%20c.xhtml"), Some(0));
        assert_eq!(p.spine_index_of_path("OEBPS/t/none.xhtml"), None);
    }

    #[test]
    fn a_broken_ncx_is_an_empty_toc_and_a_warning() {
        let manifest = format!(
            r#"{CH1}<item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>"#
        );
        let bytes = book(
            "",
            &manifest,
            r#"<spine toc="ncx"><itemref idref="ch1"/></spine>"#,
            &[
                ("OEBPS/ch1.xhtml", b"<html/>"),
                ("OEBPS/toc.ncx", b"<ncx><navMap>"),
            ],
        );
        let mut p = Package::from_bytes(&bytes).unwrap();
        assert!(p.ncx_toc().is_empty());
        assert!(p.warnings().contains(&Warning::NcxUnreadable {
            id: "ncx".into(),
            problem: crate::TocProblem::Malformed
        }));
    }

    #[test]
    fn opens_from_a_path() {
        let bytes = minimal_book("<dc:title>On disk</dc:title>");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b.epub");
        std::fs::write(&path, &bytes).unwrap();
        let p = Package::open(&path).unwrap();
        assert_eq!(p.metadata().main_title(), Some("On disk"));
        assert!(matches!(
            Package::open(dir.path().join("absent.epub")),
            Err(Error::Io(_))
        ));
    }

    /// A `<meta name="cover">` naming no manifest item, beside a valid `cover-image` item, falls
    /// back to that item.
    #[test]
    fn a_dangling_cover_meta_falls_back_to_the_cover_image_item() {
        let manifest = format!(
            r#"{CH1}<item id="c" href="c.png" media-type="image/png" properties="cover-image"/>"#
        );
        let bytes = book(
            r#"<meta name="cover" content="cover.png"/>"#,
            &manifest,
            SPINE,
            &[("OEBPS/c.png", PNG)],
        );
        let mut p = Package::from_bytes(&bytes).unwrap();
        assert_eq!(p.cover().unwrap().bytes, PNG);
    }

    /// A cover outside the package document's directory (`../Images/c.png`) is found by folding
    /// `..` at read time; the manifest path itself stays as joined.
    #[test]
    fn a_cover_outside_the_package_directory_is_found_and_its_path_kept() {
        let manifest =
            format!(r#"{CH1}<item id="c" href="../Images/c.png" media-type="image/png"/>"#);
        let bytes = book(
            r#"<meta name="cover" content="c"/>"#,
            &manifest,
            SPINE,
            &[("Images/c.png", PNG)],
        );
        let mut p = Package::from_bytes(&bytes).unwrap();
        assert_eq!(p.manifest_item("c").unwrap().path, "OEBPS/../Images/c.png");
        assert_eq!(p.cover().unwrap().bytes, PNG);
    }

    /// A namespace prefix nobody declared is read by its local part, like every other name.
    #[test]
    fn an_undeclared_namespace_prefix_does_not_refuse_the_book() {
        let opf = opf(
            "2.0",
            r#"<dc:title>T</dc:title><dc:creator opf:role="aut">A</dc:creator>"#,
            CH1,
            SPINE,
        )
        .replace(r#" xmlns:opf="http://www.idpf.org/2007/opf""#, "");
        assert!(!opf.contains("xmlns:opf"));
        let c = container("OEBPS/content.opf");
        let bytes = zip_of(&[
            ("META-INF/container.xml", c.as_bytes()),
            ("OEBPS/content.opf", opf.as_bytes()),
            ("OEBPS/ch1.xhtml", b"<html/>"),
        ]);
        let p = Package::from_bytes(&bytes).unwrap();
        assert_eq!(p.metadata().main_title(), Some("T"));
        assert_eq!(p.metadata().creators[0].role.as_deref(), Some("aut"));
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use crate::testkit::{container, esc, minimal_book, opf, zip_of};
    use crate::{SeriesSource, metadata::Contributor};
    use proptest::prelude::*;

    /// Text with no leading or trailing whitespace, and markup characters that must be escaped on
    /// the way in.
    fn text() -> impl Strategy<Value = String> {
        "[A-Za-z0-9é][A-Za-z0-9 &<>'\"é,.-]{0,14}[A-Za-z0-9é]"
    }

    #[derive(Debug, Clone)]
    struct Person {
        name: String,
        role: Option<String>,
        file_as: Option<String>,
        /// EPUB 3 refines rather than EPUB 2 `opf:` attributes.
        refined: bool,
    }

    fn person() -> impl Strategy<Value = Person> {
        (
            text(),
            proptest::option::of(prop_oneof![Just("aut"), Just("edt"), Just("ill")]),
            proptest::option::of(text()),
            any::<bool>(),
        )
            .prop_map(|(name, role, file_as, refined)| Person {
                name,
                role: role.map(str::to_string),
                file_as,
                refined,
            })
    }

    #[derive(Debug, Clone)]
    struct Model {
        titles: Vec<String>,
        creators: Vec<Person>,
        contributors: Vec<Person>,
        /// (value, scheme, scheme via refines)
        identifiers: Vec<(String, Option<String>, bool)>,
        languages: Vec<String>,
        publishers: Vec<String>,
        descriptions: Vec<String>,
        subjects: Vec<String>,
        rights: Vec<String>,
        date: Option<String>,
        collection: Option<(String, Option<String>)>,
        calibre: Option<(String, Option<String>)>,
    }

    fn model() -> impl Strategy<Value = Model> {
        let v = |n| proptest::collection::vec(text(), 0..n);
        (
            proptest::collection::vec(text(), 1..3),
            proptest::collection::vec(person(), 0..3),
            proptest::collection::vec(person(), 0..2),
            proptest::collection::vec(
                (
                    text(),
                    proptest::option::of(prop_oneof![Just("ISBN"), Just("UUID")]),
                    any::<bool>(),
                ),
                1..3,
            ),
            v(2),
            v(2),
            v(2),
            v(3),
            v(2),
            proptest::option::of(text()),
            proptest::option::of((text(), proptest::option::of("[1-9][0-9]?"))),
            proptest::option::of((text(), proptest::option::of("[1-9]\\.[05]"))),
        )
            .prop_map(
                |(
                    titles,
                    creators,
                    contributors,
                    ids,
                    languages,
                    publishers,
                    descriptions,
                    subjects,
                    rights,
                    date,
                    collection,
                    calibre,
                )| Model {
                    titles,
                    creators,
                    contributors,
                    identifiers: ids
                        .into_iter()
                        .map(|(v, s, r)| (v, s.map(str::to_string), r))
                        .collect(),
                    languages,
                    publishers,
                    descriptions,
                    subjects,
                    rights,
                    date,
                    collection,
                    calibre,
                },
            )
    }

    fn person_xml(tag: &str, n: usize, p: &Person, out: &mut String) {
        let id = format!("{tag}{n}");
        if p.refined {
            out.push_str(&format!(
                "<dc:{tag} id=\"{id}\">{}</dc:{tag}>\n",
                esc(&p.name)
            ));
            if let Some(r) = &p.role {
                out.push_str(&format!("<meta refines=\"#{id}\" property=\"role\" scheme=\"marc:relators\">{r}</meta>\n"));
            }
            if let Some(f) = &p.file_as {
                out.push_str(&format!(
                    "<meta refines=\"#{id}\" property=\"file-as\">{}</meta>\n",
                    esc(f)
                ));
            }
        } else {
            let mut attrs = String::new();
            if let Some(r) = &p.role {
                attrs.push_str(&format!(" opf:role=\"{r}\""));
            }
            if let Some(f) = &p.file_as {
                attrs.push_str(&format!(" opf:file-as=\"{}\"", esc(f)));
            }
            out.push_str(&format!("<dc:{tag}{attrs}>{}</dc:{tag}>\n", esc(&p.name)));
        }
    }

    fn to_opf_metadata(m: &Model) -> String {
        let mut x = String::new();
        for t in &m.titles {
            x.push_str(&format!("<dc:title>{}</dc:title>\n", esc(t)));
        }
        for (i, p) in m.creators.iter().enumerate() {
            person_xml("creator", i, p, &mut x);
        }
        for (i, p) in m.contributors.iter().enumerate() {
            person_xml("contributor", i, p, &mut x);
        }
        for (i, (v, scheme, refined)) in m.identifiers.iter().enumerate() {
            let id = if i == 0 {
                "uid".to_string()
            } else {
                format!("id{i}")
            };
            match (scheme, refined) {
                (Some(s), false) => x.push_str(&format!(
                    "<dc:identifier id=\"{id}\" opf:scheme=\"{s}\">{}</dc:identifier>\n",
                    esc(v)
                )),
                (Some(s), true) => {
                    x.push_str(&format!(
                        "<dc:identifier id=\"{id}\">{}</dc:identifier>\n",
                        esc(v)
                    ));
                    x.push_str(&format!(
                        "<meta refines=\"#{id}\" property=\"identifier-type\">{s}</meta>\n"
                    ));
                }
                (None, _) => x.push_str(&format!(
                    "<dc:identifier id=\"{id}\">{}</dc:identifier>\n",
                    esc(v)
                )),
            }
        }
        for (tag, list) in [
            ("language", &m.languages),
            ("publisher", &m.publishers),
            ("description", &m.descriptions),
            ("subject", &m.subjects),
            ("rights", &m.rights),
        ] {
            for v in list {
                x.push_str(&format!("<dc:{tag}>{}</dc:{tag}>\n", esc(v)));
            }
        }
        if let Some(d) = &m.date {
            x.push_str(&format!("<dc:date>{}</dc:date>\n", esc(d)));
        }
        if let Some((name, pos)) = &m.collection {
            x.push_str(&format!(
                "<meta property=\"belongs-to-collection\" id=\"coll\">{}</meta>\n",
                esc(name)
            ));
            x.push_str("<meta refines=\"#coll\" property=\"collection-type\">series</meta>\n");
            if let Some(p) = pos {
                x.push_str(&format!(
                    "<meta refines=\"#coll\" property=\"group-position\">{p}</meta>\n"
                ));
            }
        }
        if let Some((name, idx)) = &m.calibre {
            x.push_str(&format!(
                "<meta name=\"calibre:series\" content=\"{}\"/>\n",
                esc(name)
            ));
            if let Some(i) = idx {
                x.push_str(&format!(
                    "<meta name=\"calibre:series_index\" content=\"{i}\"/>\n"
                ));
            }
        }
        x
    }

    fn check_people(got: &[Contributor], want: &[Person]) -> Result<(), TestCaseError> {
        prop_assert_eq!(got.len(), want.len());
        for (g, w) in got.iter().zip(want) {
            prop_assert_eq!(&g.name, &w.name);
            prop_assert_eq!(&g.role, &w.role);
            prop_assert_eq!(&g.file_as, &w.file_as);
        }
        Ok(())
    }

    proptest! {

        /// An OPF written from an arbitrary metadata model, zipped, reads back as that model —
        /// through both conventions for every field that has two.
        #[test]
        fn a_generated_package_round_trips(m in model()) {
            let bytes = minimal_book(&to_opf_metadata(&m));
            let p = Package::from_bytes(&bytes).unwrap();
            prop_assert!(
                !p.warnings().iter().any(|w| !matches!(w, Warning::ManifestItemMissing { .. })),
                "{:?}", p.warnings()
            );
            let md = p.metadata();
            prop_assert_eq!(md.titles.iter().map(|t| t.value.clone()).collect::<Vec<_>>(), m.titles.clone());
            prop_assert_eq!(md.values("title").map(str::to_string).collect::<Vec<_>>(), m.titles.clone());
            check_people(&md.creators, &m.creators)?;
            check_people(&md.contributors, &m.contributors)?;
            prop_assert_eq!(md.identifiers.len(), m.identifiers.len());
            for (i, (g, (v, s, _))) in md.identifiers.iter().zip(&m.identifiers).enumerate() {
                prop_assert_eq!(&g.value, v);
                prop_assert_eq!(&g.scheme, s);
                prop_assert_eq!(g.is_unique, i == 0);
                prop_assert_eq!(g.scheme_says_isbn(), s.as_deref() == Some("ISBN"));
            }
            prop_assert_eq!(p.unique_identifier(), m.identifiers.first().map(|i| i.0.as_str()));
            prop_assert_eq!(&md.languages, &m.languages);
            prop_assert_eq!(&md.publishers, &m.publishers);
            prop_assert_eq!(&md.descriptions, &m.descriptions);
            prop_assert_eq!(&md.subjects, &m.subjects);
            prop_assert_eq!(&md.rights, &m.rights);
            prop_assert_eq!(md.publication_date(), m.date.as_deref());
            let mut want = Vec::new();
            if let Some((n, pos)) = &m.collection {
                want.push((n.clone(), pos.clone(), SeriesSource::Collection));
            }
            if let Some((n, idx)) = &m.calibre {
                want.push((n.clone(), idx.clone(), SeriesSource::Calibre));
            }
            let got: Vec<_> = md.series.iter().map(|s| (s.name.clone(), s.position.clone(), s.source)).collect();
            prop_assert_eq!(got, want);
        }

        /// Arbitrary bytes are opened or refused — never a panic — and so is everything a caller
        /// might then ask of what opened.
        #[test]
        fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
            if let Ok(mut p) = Package::from_bytes(&bytes) {
                let _ = p.cover();
                let _ = p.ncx();
                let _ = p.nav();
                for i in 0..p.spine().items.len() {
                    let _ = p.spine_text(i);
                }
            }
        }

        /// A valid book cut short or with bits flipped anywhere is opened or refused, never a panic,
        /// and every read on it is bounded.
        #[test]
        fn a_damaged_book_never_panics(
            cut in any::<prop::sample::Index>(),
            flips in proptest::collection::vec((any::<prop::sample::Index>(), 0u8..8), 0..6),
        ) {
            let good = minimal_book(r#"<dc:title>T</dc:title><dc:identifier id="uid">x</dc:identifier><meta name="cover" content="ch1"/>"#);
            let mut bytes = good.clone();
            for (at, bit) in &flips {
                let i = at.index(bytes.len());
                if let Some(b) = bytes.get_mut(i) {
                    *b ^= 1 << bit;
                }
            }
            bytes.truncate(cut.index(good.len() + 1));
            if let Ok(mut p) = Package::from_bytes(&bytes) {
                let _ = p.cover();
                let _ = p.toc();
                let _ = p.spine_text(0);
                let ids: Vec<String> = p.manifest().iter().map(|m| m.id.clone()).collect();
                for id in ids {
                    let _ = p.resource(&id);
                }
            }
        }

        /// Any file name, in any directory relative to the package document, is found from its href —
        /// written plain or percent-encoded — and the spine position of its resolved path is the one
        /// it was given.
        #[test]
        fn hrefs_resolve_to_their_files(
            opf_dir in proptest::collection::vec("[A-Za-z0-9_-]{1,6}", 0..3),
            file_dir in proptest::collection::vec("[A-Za-z0-9_-]{1,6}", 0..2),
            stem in "[A-Za-z0-9 _()é-]{1,12}",
            encode in any::<bool>(),
        ) {
            let opf_dir = opf_dir.join("/");
            let rel = if file_dir.is_empty() { format!("{stem}.xhtml") } else { format!("{}/{stem}.xhtml", file_dir.join("/")) };
            let opf_path = if opf_dir.is_empty() { "content.opf".to_string() } else { format!("{opf_dir}/content.opf") };
            let entry = if opf_dir.is_empty() { rel.clone() } else { format!("{opf_dir}/{rel}") };
            let href = if encode {
                rel.split('/').map(|s| percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()).collect::<Vec<_>>().join("/")
            } else {
                esc(&rel)
            };
            let manifest = format!(r#"<item id="ch" href="{href}" media-type="application/xhtml+xml"/>"#);
            let opf = opf("3.0", "<dc:title>T</dc:title>", &manifest, r#"<spine><itemref idref="ch"/></spine>"#);
            let c = container(&esc(&opf_path));
            let bytes = zip_of(&[
                ("META-INF/container.xml", c.as_bytes()),
                (opf_path.as_str(), opf.as_bytes()),
                (entry.as_str(), b"<html>here</html>"),
            ]);
            let mut p = Package::from_bytes(&bytes).unwrap();
            prop_assert!(p.warnings().is_empty(), "{:?}", p.warnings());
            prop_assert_eq!(p.resource("ch").unwrap(), b"<html>here</html>".to_vec());
            let resolved = p.manifest_item("ch").unwrap().path.clone();
            prop_assert_eq!(p.spine_index_of_path(&resolved), Some(0));
            if !encode {
                prop_assert_eq!(&resolved, &entry);
            }
        }
    }
}
