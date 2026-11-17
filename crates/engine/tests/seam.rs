//! Frontends reach the library through `bookmole-api`, so no public function may hand a crate
//! without the `internals` feature a `Storage` or its pool. A source scan, because this suite is
//! always built with `internals` on and cannot observe the build where it is off.

use std::path::{Path, PathBuf};

const LEAKS: &[&str] = &["Storage", "SqlitePool"];
const GATE: &str = "feature = \"internals\"";

/// `src` with comments removed and the contents of string and char literals blanked.
fn code_only(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < b.len() {
        let rest = &src[i..];
        if rest.starts_with("//") {
            i += rest.find('\n').unwrap_or(rest.len());
        } else if rest.starts_with("/*") {
            let mut depth = 0usize;
            while i < b.len() {
                if src[i..].starts_with("/*") {
                    depth += 1;
                    i += 2;
                } else if src[i..].starts_with("*/") {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            out.push(' ');
        } else if rest.starts_with("#[") || rest.starts_with("#![") {
            let len = attribute_len(rest);
            out.push_str(&rest[..len]);
            i += len;
        } else if let Some(hashes) = raw_string_hashes(rest) {
            let close = format!("\"{}", "#".repeat(hashes));
            let body = 2 + hashes;
            let end = rest[body..]
                .find(&close)
                .map_or(rest.len(), |e| body + e + close.len());
            out.push_str("\"\"");
            i += end;
        } else if b[i] == b'"' {
            i += 1;
            while i < b.len() && b[i] != b'"' {
                i += if b[i] == b'\\' { 2 } else { 1 };
            }
            i += 1;
            out.push_str("\"\"");
        } else if b[i] == b'\'' && is_char_literal(rest) {
            i += 1;
            while i < b.len() && b[i] != b'\'' {
                i += if b[i] == b'\\' { 2 } else { 1 };
            }
            i += 1;
            out.push_str("' '");
        } else {
            let c = rest.chars().next().expect("non-empty rest");
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

/// Length of the attribute `s` opens with, kept whole because its strings are what a gate says.
fn attribute_len(s: &str) -> usize {
    let b = s.as_bytes();
    let mut depth = 0usize;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
            }
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    b.len()
}

/// `Some(n)` when `s` opens a raw string `r#…#"` with `n` hashes.
fn raw_string_hashes(s: &str) -> Option<usize> {
    let after_r = s.strip_prefix("br").or_else(|| s.strip_prefix('r'))?;
    let hashes = after_r.len() - after_r.trim_start_matches('#').len();
    after_r[hashes..].starts_with('"').then_some(hashes)
}

/// A `'` that opens a char literal rather than a lifetime or label.
fn is_char_literal(s: &str) -> bool {
    let mut chars = s.chars().skip(1);
    match chars.next() {
        Some('\\') => true,
        Some(_) => chars.next() == Some('\''),
        None => false,
    }
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn names_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + word.len()..].chars().next();
        !before.is_some_and(is_ident) && !after.is_some_and(is_ident)
    })
}

/// What precedes an item's `fn`: its visibility, if any, and its attributes.
struct Head {
    public: bool,
    attributes: Vec<String>,
}

fn head_before(code: &str) -> Head {
    let mut rest = code.trim_end();
    let mut public = false;
    let mut attributes = Vec::new();
    loop {
        if let Some(r) = ["async", "const", "unsafe"]
            .iter()
            .find_map(|m| rest.strip_suffix(m))
            .filter(|r| !r.chars().next_back().is_some_and(is_ident))
        {
            rest = r.trim_end();
        } else if let Some(r) = rest
            .strip_suffix("pub")
            .filter(|r| !r.chars().next_back().is_some_and(is_ident))
        {
            public = true;
            rest = r.trim_end();
        } else if rest.ends_with(')')
            && open_of(rest, '(', ')').is_some_and(|o| rest[..o].trim_end().ends_with("pub"))
        {
            let o = open_of(rest, '(', ')').expect("checked above");
            rest = rest[..o]
                .trim_end()
                .strip_suffix("pub")
                .expect("checked above")
                .trim_end();
        } else if rest.ends_with(']') {
            let Some(o) = open_of(rest, '[', ']') else {
                break;
            };
            let Some(r) = rest[..o].strip_suffix('#') else {
                break;
            };
            attributes.push(rest[o..].to_string());
            rest = r.trim_end();
        } else {
            break;
        }
    }
    Head { public, attributes }
}

/// Index of the bracket that opens the one `s` ends with.
fn open_of(s: &str, open: char, close: char) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in s.char_indices().rev() {
        if c == close {
            depth += 1;
        } else if c == open {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

fn rust_files(dir: &Path, into: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read src dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rust_files(&path, into);
        } else if path.extension().is_some_and(|e| e == "rs") {
            into.push(path);
        }
    }
}

/// `(file, function name, public, gated)` for every function returning a leak type.
fn handouts() -> Vec<(PathBuf, String, bool, bool)> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    let mut found = Vec::new();
    for file in files {
        let code = code_only(&std::fs::read_to_string(&file).expect("read source"));
        for (at, _) in code.match_indices("fn ") {
            if code[..at].chars().next_back().is_some_and(is_ident) {
                continue;
            }
            let sig_end = code[at..].find(['{', ';']).map_or(code.len(), |e| at + e);
            let signature = &code[at..sig_end];
            let Some(arrow) = signature.find("->") else {
                continue;
            };
            if !LEAKS.iter().any(|w| names_word(&signature[arrow..], w)) {
                continue;
            }
            let name = signature[3..]
                .trim_start()
                .split(|c: char| !is_ident(c))
                .next()
                .unwrap_or_default()
                .to_string();
            let head = head_before(&code[..at]);
            let gated = head.attributes.iter().any(|a| a.contains(GATE));
            found.push((file.clone(), name, head.public, gated));
        }
    }
    found
}

#[test]
fn only_internals_hands_out_the_database() {
    let found = handouts();
    for expected in ["connect", "storage"] {
        assert!(
            found
                .iter()
                .any(|(_, name, public, _)| name == expected && *public),
            "the scan no longer sees `pub fn {expected}`; fix the scanner before trusting it"
        );
    }
    let open: Vec<String> = found
        .iter()
        .filter(|(_, _, public, gated)| *public && !gated)
        .map(|(file, name, _, _)| {
            let file = file
                .strip_prefix(env!("CARGO_MANIFEST_DIR"))
                .unwrap_or(file);
            format!("{}: fn {name}", file.display())
        })
        .collect();
    assert!(
        open.is_empty(),
        "these return a Storage or pool to any dependent; make them pub(crate) or gate them on \
         `{GATE}`:\n{}",
        open.join("\n")
    );
}

#[test]
fn the_scanner_reads_visibility_and_gates() {
    let code = code_only(
        "/* fn x() -> Storage */ // pub fn y() -> Storage\n\
         #[cfg(feature = \"internals\")]\n#[doc(hidden)]\npub async fn a() -> Storage {}\n\
         pub(crate) fn b() -> &SqlitePool {}\nconst S: &str = \"pub fn c() -> Storage\";\n\
         pub fn d<'a>(x: &'a u8) -> Result<Storage> {}\n",
    );
    let heads: Vec<(String, bool, bool)> = code
        .match_indices("fn ")
        .map(|(at, _)| {
            let name = code[at + 3..]
                .split('(')
                .next()
                .unwrap()
                .split('<')
                .next()
                .unwrap();
            let head = head_before(&code[..at]);
            let gated = head.attributes.iter().any(|a| a.contains(GATE));
            (name.to_string(), head.public, gated)
        })
        .collect();
    assert_eq!(
        heads,
        vec![
            ("a".to_string(), true, true),
            ("b".to_string(), false, false),
            ("d".to_string(), true, false),
        ]
    );
}
