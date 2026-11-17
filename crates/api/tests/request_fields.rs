//! Requests are append-only. Generated client bindings treat every field as
//! required, so even a defaulted field added to an existing request breaks
//! them: an existing variant's fields stay as recorded in `request_fields.txt`
//! until `API_VERSION` moves. A new variant is recorded with `UPDATE_GOLDEN=1`
//! (or `make golden`).

use std::collections::BTreeMap;
use std::path::Path;

use bookmole_api::API_VERSION;

const SNAPSHOT: &str = "tests/request_fields.txt";

type Variants = Vec<(String, Vec<String>)>;

/// The text between the brace after `head` and its match.
fn body_after<'a>(src: &'a str, head: &str) -> &'a str {
    let start = src.find(head).expect("the Request enum") + head.len();
    let open = start + src[start..].find('{').expect("enum body");
    let mut depth = 0usize;
    for (i, c) in src[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &src[open + 1..open + i];
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced enum body")
}

/// `text` without comments or `#[...]` attributes.
fn bare(text: &str) -> String {
    let code: String = text
        .lines()
        .map(|l| l.find("//").map_or(l, |i| &l[..i]))
        .collect::<Vec<_>>()
        .join("\n");
    let mut out = String::new();
    let mut chars = code.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '#' && chars.peek() == Some(&'[') {
            let mut depth = 0usize;
            for c in chars.by_ref() {
                match c {
                    '[' => depth += 1,
                    ']' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Splits on commas outside any bracket.
fn top_level(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut from) = (0i32, 0);
    for (i, c) in text.char_indices() {
        match c {
            '{' | '(' | '[' | '<' => depth += 1,
            '}' | ')' | ']' | '>' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&text[from..i]);
                from = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&text[from..]);
    parts
        .into_iter()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect()
}

fn variants(src: &str) -> Variants {
    let body = bare(body_after(src, "pub enum Request"));
    top_level(&body)
        .into_iter()
        .map(|variant| {
            let name_end = variant
                .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                .unwrap_or(variant.len());
            let (name, rest) = variant.split_at(name_end);
            let rest = rest.trim();
            let fields = if let Some(inner) = rest.strip_prefix('{') {
                let inner = inner.strip_suffix('}').expect("struct variant");
                top_level(inner)
                    .into_iter()
                    .map(|f| f.split(':').next().unwrap_or(f).trim().to_string())
                    .collect()
            } else if let Some(inner) = rest.strip_prefix('(') {
                let inner = inner.strip_suffix(')').expect("tuple variant");
                (0..top_level(inner).len()).map(|i| i.to_string()).collect()
            } else {
                assert!(rest.is_empty(), "unparsed variant: {variant}");
                Vec::new()
            };
            (name.to_string(), fields)
        })
        .collect()
}

fn current() -> Variants {
    variants(include_str!("../src/protocol.rs"))
}

fn render(version: u32, variants: &Variants) -> String {
    let mut out = format!("api_version {version}\n");
    for (name, fields) in variants {
        out.push_str(name);
        for f in fields {
            out.push(' ');
            out.push_str(f);
        }
        out.push('\n');
    }
    out
}

fn parse(text: &str) -> (u32, Variants) {
    let mut lines = text.lines();
    let version = lines
        .next()
        .and_then(|l| l.strip_prefix("api_version "))
        .and_then(|v| v.trim().parse().ok())
        .expect("the snapshot starts with `api_version N`");
    let variants = lines
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let mut words = l.split_whitespace().map(String::from);
            let name = words.next().expect("variant name");
            (name, words.collect())
        })
        .collect();
    (version, variants)
}

fn sorted(fields: &[String]) -> Vec<&str> {
    let mut v: Vec<&str> = fields.iter().map(String::as_str).collect();
    v.sort_unstable();
    v
}

#[test]
fn existing_requests_keep_their_fields() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(SNAPSHOT);
    let update = std::env::var("UPDATE_GOLDEN").is_ok();
    let now = current();
    assert!(now.len() > 10, "parsed only {} variants", now.len());

    let recorded = std::fs::read_to_string(&path).expect("read the request snapshot");
    let (version, before) = parse(&recorded);

    if version != API_VERSION {
        if update {
            std::fs::write(&path, render(API_VERSION, &now)).expect("write snapshot");
            return;
        }
        panic!(
            "API_VERSION is {API_VERSION} but {SNAPSHOT} records {version}; \
             re-record it with UPDATE_GOLDEN=1"
        );
    }

    let now_by_name: BTreeMap<&str, &Vec<String>> =
        now.iter().map(|(n, f)| (n.as_str(), f)).collect();
    let mut changed = Vec::new();
    for (name, fields) in &before {
        match now_by_name.get(name.as_str()) {
            None => changed.push(format!("{name}: removed")),
            Some(f) if sorted(f) != sorted(fields) => {
                changed.push(format!("{name}: {fields:?} became {f:?}"))
            }
            Some(_) => {}
        }
    }
    assert!(
        changed.is_empty(),
        "existing requests changed shape without API_VERSION moving; add a new \
         request instead, or bump API_VERSION and re-record:\n{}",
        changed.join("\n")
    );

    let known: Vec<&str> = before.iter().map(|(n, _)| n.as_str()).collect();
    let added: Vec<&str> = now
        .iter()
        .map(|(n, _)| n.as_str())
        .filter(|n| !known.contains(n))
        .collect();
    if added.is_empty() {
        return;
    }
    if update {
        std::fs::write(&path, render(API_VERSION, &now)).expect("write snapshot");
        return;
    }
    panic!("new requests {added:?} are not in {SNAPSHOT}; record them with UPDATE_GOLDEN=1");
}

#[test]
fn the_parser_reads_every_variant_shape() {
    let src = "pub enum Request {\n    /// doc { not: a field }\n    Unit,\n    #[serde(rename_all = \"x\")]\n    Fields {\n        #[serde(default)]\n        a: Option<Vec<(u8, u8)>>, // b: c\n        b: BTreeMap<String, u8>,\n    },\n    Tuple(u8, String),\n}\npub enum Response { Other { z: u8 } }\n";
    let got = variants(src);
    let want: Variants = vec![
        ("Unit".into(), vec![]),
        ("Fields".into(), vec!["a".into(), "b".into()]),
        ("Tuple".into(), vec!["0".into(), "1".into()]),
    ];
    assert_eq!(got, want);
}
