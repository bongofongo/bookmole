//! The package document's `<metadata>`, typed.
//!
//! Two views of one block. The **typed** fields (`titles`, `creators`, …)
//! read each element's whole text, trimmed, and resolve EPUB 3 `refines`
//! (role, file-as, display-seq, title-type, identifier-type, collection-type,
//! group-position) as well as the EPUB 2 `opf:` attributes. The **flat**
//! [`Metadata::entries`] list is the key → value view the `epub` 2.x crate
//! offered, reproduced exactly, for callers that must keep giving the
//! answers they gave before.

use std::collections::HashMap;

use crate::error::Warning;
use crate::xml::Node;

/// The package's metadata.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Metadata {
    /// `dc:title`, in document order.
    pub titles: Vec<Title>,
    /// `dc:creator`, in document order.
    pub creators: Vec<Contributor>,
    /// `dc:contributor`, in document order.
    pub contributors: Vec<Contributor>,
    /// `dc:identifier`, in document order.
    pub identifiers: Vec<Identifier>,
    /// `dc:language`, in document order.
    pub languages: Vec<String>,
    /// `dc:publisher`, in document order.
    pub publishers: Vec<String>,
    /// `dc:date`, in document order.
    pub dates: Vec<Date>,
    /// `dcterms:modified` (EPUB 3), the first one not refining anything.
    pub modified: Option<String>,
    /// `dc:description`, in document order.
    pub descriptions: Vec<String>,
    /// `dc:subject`, in document order.
    pub subjects: Vec<String>,
    /// `dc:rights`, in document order.
    pub rights: Vec<String>,
    /// Series membership, by either convention, in document order.
    pub series: Vec<Series>,
    /// Every `meta` element, in document order.
    pub metas: Vec<Meta>,
    entries: Vec<MetaEntry>,
}

/// A `dc:title`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Title {
    /// The title, trimmed.
    pub value: String,
    /// The element's `id`.
    pub id: Option<String>,
    /// EPUB 3 `title-type` (`main`, `subtitle`, `collection`, …).
    pub title_type: Option<String>,
    /// `file-as`, from a refining meta or an `opf:file-as` attribute.
    pub file_as: Option<String>,
    /// EPUB 3 `display-seq`.
    pub display_seq: Option<u32>,
    /// `xml:lang`.
    pub lang: Option<String>,
}

/// A `dc:creator` or `dc:contributor`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Contributor {
    /// The name, trimmed, as the book displays it.
    pub name: String,
    /// The element's `id`.
    pub id: Option<String>,
    /// The role (a MARC relator such as `aut`), from `opf:role` or a refining
    /// `role` meta; the attribute wins when both are present.
    pub role: Option<String>,
    /// The sort form, from `opf:file-as` or a refining `file-as` meta; the
    /// attribute wins when both are present.
    pub file_as: Option<String>,
    /// EPUB 3 `display-seq`.
    pub display_seq: Option<u32>,
    /// `xml:lang`.
    pub lang: Option<String>,
}

/// A `dc:identifier`.
///
/// Whether the value is a *valid* ISBN is the caller's question; this only
/// reports what the book says about it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Identifier {
    /// The value, trimmed, exactly as written (`urn:isbn:…` prefixes kept).
    pub value: String,
    /// The element's `id`.
    pub id: Option<String>,
    /// The scheme: an `opf:scheme` attribute, else a refining
    /// `identifier-type` meta's value, else a `urn:` value's namespace
    /// (`isbn` for `urn:isbn:…`).
    pub scheme: Option<String>,
    /// The `scheme` attribute of the refining `identifier-type` meta, when
    /// the scheme came from one (`onix:codelist5`).
    pub scheme_authority: Option<String>,
    /// Whether this is the package's `unique-identifier`.
    pub is_unique: bool,
}

impl Identifier {
    /// Whether the book *says* this is an ISBN: scheme `ISBN` in any case, a
    /// `urn:isbn:` value, or ONIX code list 5's `02`/`15`.
    pub fn scheme_says_isbn(&self) -> bool {
        if self.value.len() >= 9
            && self
                .value
                .get(..9)
                .is_some_and(|p| p.eq_ignore_ascii_case("urn:isbn:"))
        {
            return true;
        }
        match self.scheme.as_deref() {
            Some(s) if s.eq_ignore_ascii_case("isbn") => true,
            Some("02" | "15") => self
                .scheme_authority
                .as_deref()
                .is_some_and(|a| a.eq_ignore_ascii_case("onix:codelist5")),
            _ => false,
        }
    }
}

/// A `dc:date`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Date {
    /// The date as written, trimmed.
    pub value: String,
    /// EPUB 2's `opf:event` (`publication`, `modification`, …).
    pub event: Option<String>,
}

/// Which convention a [`Series`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SeriesSource {
    /// EPUB 3 `belongs-to-collection`.
    Collection,
    /// calibre's `calibre:series` / `calibre:series_index` metas.
    Calibre,
}

/// A series (or other collection) the book belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Series {
    /// The series name, trimmed.
    pub name: String,
    /// The book's position, as written (`group-position` or
    /// `calibre:series_index`; may be fractional, `"1.5"`).
    pub position: Option<String>,
    /// EPUB 3 `collection-type` (`series`, `set`), when given.
    pub collection_type: Option<String>,
    /// Which convention said so.
    pub source: SeriesSource,
}

/// A `meta` element, either form.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Meta {
    /// EPUB 2 `name`.
    pub name: Option<String>,
    /// EPUB 2 `content`.
    pub content: Option<String>,
    /// EPUB 3 `property`.
    pub property: Option<String>,
    /// EPUB 3 `refines`, as written (`#id`).
    pub refines: Option<String>,
    /// `id`.
    pub id: Option<String>,
    /// EPUB 3 `scheme`.
    pub scheme: Option<String>,
    /// The element's text, trimmed (the EPUB 3 value).
    pub value: String,
}

/// One key → value pair of the flat view; see [`Metadata::entries`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct MetaEntry {
    /// An element's local name (`title`), a `meta`'s `name`, or a `meta`'s
    /// `property`.
    pub key: String,
    /// The value, untrimmed.
    pub value: String,
}

impl Metadata {
    /// The flat key → value view, in document order — what the `epub` 2.x
    /// crate's `metadata` map held, value for value:
    ///
    /// - every direct child of `<metadata>` other than `meta` gives
    ///   `(local name, text)`, where text is the element's last
    ///   non-whitespace run of direct character data, or `""`;
    /// - a `meta` with `name` and `content` gives `(name, content)`;
    /// - otherwise a `meta` with `property` gives `(property, text)`;
    /// - any other `meta` gives nothing.
    pub fn entries(&self) -> &[MetaEntry] {
        &self.entries
    }

    /// The flat view's values for one key, in document order.
    pub fn values<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.entries
            .iter()
            .filter(move |e| e.key == key)
            .map(|e| e.value.as_str())
    }

    /// The main title: the first with EPUB 3 `title-type` `main`, else the
    /// first title.
    pub fn main_title(&self) -> Option<&str> {
        self.titles
            .iter()
            .find(|t| t.title_type.as_deref() == Some("main"))
            .or_else(|| self.titles.first())
            .map(|t| t.value.as_str())
    }

    /// The publication date: the first `dc:date` with no `opf:event` or with
    /// event `publication`.
    pub fn publication_date(&self) -> Option<&str> {
        self.dates
            .iter()
            .find(|d| matches!(d.event.as_deref(), None | Some("publication")))
            .map(|d| d.value.as_str())
    }

    /// The identifier the package names as its `unique-identifier`.
    pub fn unique_identifier(&self) -> Option<&Identifier> {
        self.identifiers.iter().find(|i| i.is_unique)
    }
}

/// What parsing `<metadata>` produced beyond [`Metadata`].
pub(crate) struct Parsed {
    pub metadata: Metadata,
    /// The `unique-identifier` value, by the old crate's rule.
    pub unique_identifier: Option<String>,
    /// The last `<meta name="cover" content="…">`'s content.
    pub cover_meta: Option<String>,
    pub warnings: Vec<Warning>,
}

#[derive(Clone, Copy)]
enum Target {
    Title(usize),
    Creator(usize),
    Contributor(usize),
    Identifier(usize),
    Meta,
}

fn trimmed(n: &Node) -> String {
    n.text_content().trim().to_string()
}

fn owned(s: Option<&str>) -> Option<String> {
    s.map(str::to_string)
}

pub(crate) fn parse(
    node: &Node,
    unique_id: Option<&str>,
    is_manifest_id: &dyn Fn(&str) -> bool,
) -> Parsed {
    let mut md = Metadata::default();
    let mut unique_identifier = None;
    let mut cover_meta = None;
    let mut warnings = Vec::new();

    // The flat view: direct children only, exactly as the old crate read them.
    for item in node.children() {
        if item.name == "meta" {
            if let (Some(k), Some(v)) = (item.attr("name"), item.attr("content")) {
                if k == "cover" {
                    cover_meta = Some(v.to_string());
                }
                md.entries.push(MetaEntry {
                    key: k.to_string(),
                    value: v.to_string(),
                });
            } else if let Some(k) = item.attr("property") {
                md.entries.push(MetaEntry {
                    key: k.to_string(),
                    value: item.text.clone().unwrap_or_default(),
                });
            }
        } else {
            let value = item.text.clone().unwrap_or_default();
            if item.name == "identifier"
                && unique_identifier.is_none()
                && unique_id.is_some()
                && item.attr("id") == unique_id
            {
                unique_identifier = Some(value.clone());
            }
            md.entries.push(MetaEntry {
                key: item.name.clone(),
                value,
            });
        }
    }

    // The typed view. OPF 2.0's legacy `dc-metadata` / `x-metadata` wrappers
    // are looked through.
    let mut elements: Vec<&Node> = Vec::new();
    for item in node.children() {
        if item.name == "dc-metadata" || item.name == "x-metadata" {
            elements.extend(item.children());
        } else {
            elements.push(item);
        }
    }

    let mut ids: HashMap<String, Target> = HashMap::new();
    for el in &elements {
        let id = owned(el.attr("id"));
        let lang = owned(el.attr("lang"));
        let target = match el.name.as_str() {
            "title" => {
                md.titles.push(Title {
                    value: trimmed(el),
                    id: id.clone(),
                    title_type: None,
                    file_as: owned(el.attr("file-as")),
                    display_seq: None,
                    lang,
                });
                Some(Target::Title(md.titles.len() - 1))
            }
            "creator" | "contributor" => {
                let c = Contributor {
                    name: trimmed(el),
                    id: id.clone(),
                    role: owned(el.attr("role")),
                    file_as: owned(el.attr("file-as")),
                    display_seq: None,
                    lang,
                };
                if el.name == "creator" {
                    md.creators.push(c);
                    Some(Target::Creator(md.creators.len() - 1))
                } else {
                    md.contributors.push(c);
                    Some(Target::Contributor(md.contributors.len() - 1))
                }
            }
            "identifier" => {
                let is_unique = unique_id.is_some() && id.as_deref() == unique_id;
                md.identifiers.push(Identifier {
                    value: trimmed(el),
                    id: id.clone(),
                    scheme: owned(el.attr("scheme")),
                    scheme_authority: None,
                    is_unique,
                });
                Some(Target::Identifier(md.identifiers.len() - 1))
            }
            "language" => {
                md.languages.push(trimmed(el));
                None
            }
            "publisher" => {
                md.publishers.push(trimmed(el));
                None
            }
            "date" => {
                md.dates.push(Date {
                    value: trimmed(el),
                    event: owned(el.attr("event")),
                });
                None
            }
            "description" => {
                md.descriptions.push(trimmed(el));
                None
            }
            "subject" => {
                md.subjects.push(trimmed(el));
                None
            }
            "rights" => {
                md.rights.push(trimmed(el));
                None
            }
            "meta" => {
                md.metas.push(Meta {
                    name: owned(el.attr("name")),
                    content: owned(el.attr("content")),
                    property: owned(el.attr("property")),
                    refines: owned(el.attr("refines")),
                    id: id.clone(),
                    scheme: owned(el.attr("scheme")),
                    value: trimmed(el),
                });
                Some(Target::Meta)
            }
            _ => None,
        };
        if let (Some(id), Some(t)) = (id, target) {
            // The first element to carry an id is the one it names.
            ids.entry(id).or_insert(t);
        }
    }

    // Resolve `refines`. Refinements of manifest items (media overlays) are
    // legitimate and simply not ours to apply.
    for i in 0..md.metas.len() {
        let Some(meta) = md.metas.get(i).cloned() else {
            continue;
        };
        let (Some(refines), Some(property)) = (meta.refines.as_deref(), meta.property.as_deref())
        else {
            continue;
        };
        let key = refines.strip_prefix('#').unwrap_or(refines);
        let Some(target) = ids.get(key).copied() else {
            if !is_manifest_id(key) {
                warnings.push(Warning::UnresolvedRefines {
                    refines: refines.to_string(),
                });
            }
            continue;
        };
        apply_refinement(&mut md, target, property, &meta);
    }

    if md.modified.is_none() {
        md.modified = md
            .metas
            .iter()
            .find(|m| m.refines.is_none() && m.property.as_deref() == Some("dcterms:modified"))
            .map(|m| m.value.clone());
    }

    // Series, both conventions, in document order.
    let mut series_index_calibre = md
        .metas
        .iter()
        .find(|m| m.name.as_deref() == Some("calibre:series_index"))
        .and_then(|m| m.content.clone());
    // id → (group-position, collection-type), first of each, in one pass.
    let mut collection_refines: HashMap<&str, (Option<String>, Option<String>)> = HashMap::new();
    for r in md.metas.iter() {
        let Some(id) = r
            .refines
            .as_deref()
            .map(|s| s.strip_prefix('#').unwrap_or(s))
        else {
            continue;
        };
        let slot = match r.property.as_deref() {
            Some("group-position") => 0,
            Some("collection-type") => 1,
            _ => continue,
        };
        let entry = collection_refines.entry(id).or_default();
        let field = if slot == 0 {
            &mut entry.0
        } else {
            &mut entry.1
        };
        field.get_or_insert_with(|| r.value.clone());
    }
    for m in md.metas.iter() {
        if m.property.as_deref() == Some("belongs-to-collection") && m.refines.is_none() {
            let (position, collection_type) =
                m.id.as_deref()
                    .and_then(|id| collection_refines.get(id).cloned())
                    .unwrap_or_default();
            md.series.push(Series {
                name: m.value.clone(),
                position,
                collection_type,
                source: SeriesSource::Collection,
            });
        } else if m.name.as_deref() == Some("calibre:series")
            && let Some(name) = m.content.as_deref()
        {
            md.series.push(Series {
                name: name.trim().to_string(),
                position: series_index_calibre.take().map(|s| s.trim().to_string()),
                collection_type: None,
                source: SeriesSource::Calibre,
            });
        }
    }

    // A `urn:` value names its own scheme when nothing else did.
    for ident in &mut md.identifiers {
        if ident.scheme.is_none()
            && ident
                .value
                .get(..4)
                .is_some_and(|p| p.eq_ignore_ascii_case("urn:"))
            && let Some(nid) = ident.value.get(4..).and_then(|r| r.split(':').next())
            && !nid.is_empty()
        {
            ident.scheme = Some(nid.to_ascii_lowercase());
        }
    }

    Parsed {
        metadata: md,
        unique_identifier,
        cover_meta,
        warnings,
    }
}

fn apply_refinement(md: &mut Metadata, target: Target, property: &str, meta: &Meta) {
    let value = meta.value.clone();
    match (target, property) {
        (Target::Title(i), "title-type") => {
            if let Some(t) = md.titles.get_mut(i) {
                t.title_type.get_or_insert(value);
            }
        }
        (Target::Title(i), "file-as") => {
            if let Some(t) = md.titles.get_mut(i) {
                t.file_as.get_or_insert(value);
            }
        }
        (Target::Title(i), "display-seq") => {
            if let Some(t) = md.titles.get_mut(i) {
                t.display_seq = t.display_seq.or(value.parse().ok());
            }
        }
        (Target::Creator(i) | Target::Contributor(i), "role" | "file-as" | "display-seq") => {
            let list = if matches!(target, Target::Creator(_)) {
                &mut md.creators
            } else {
                &mut md.contributors
            };
            if let Some(c) = list.get_mut(i) {
                match property {
                    "role" => {
                        c.role.get_or_insert(value);
                    }
                    "file-as" => {
                        c.file_as.get_or_insert(value);
                    }
                    _ => c.display_seq = c.display_seq.or(value.parse().ok()),
                }
            }
        }
        (Target::Identifier(i), "identifier-type") => {
            if let Some(id) = md.identifiers.get_mut(i)
                && id.scheme.is_none()
            {
                id.scheme = Some(value);
                id.scheme_authority = meta.scheme.clone();
            }
        }
        _ => {}
    }
}
