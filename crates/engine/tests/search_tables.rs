//! The search indexes are kept by triggers, and user text reaches fts5 `MATCH`
//! only through the modules that quote it. Both are read off the source.

use std::path::{Path, PathBuf};

use regex::Regex;

const TRIGGER_MAINTAINED: &[&str] = &["highlights_fts", "books_search", "provider_record_index"];

/// The only files whose SQL may say `MATCH`: each builds its expression from
/// quoted terms, never from the user's raw text.
const MATCH_FILES: &[&str] = &[
    "storage/fts.rs",
    "storage/book_search.rs",
    "storage/search_cache.rs",
    "storage/note_rows.rs",
    "search/wellknown.rs",
];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read dir") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// `src` with comments and every `#[cfg(test)]` item removed.
fn shipped(src: &str) -> String {
    let code: String = src
        .lines()
        .map(|l| l.find("//").map_or(l, |i| &l[..i]))
        .collect::<Vec<_>>()
        .join("\n");
    let mut out = String::new();
    let mut rest = code.as_str();
    while let Some(at) = rest.find("#[cfg(test)]") {
        out.push_str(&rest[..at]);
        let item = &rest[at..];
        let mut depth = 0usize;
        let mut end = item.len();
        for (i, c) in item.char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = i + 1;
                        break;
                    }
                }
                ';' if depth == 0 => {
                    end = i + 1;
                    break;
                }
                _ => {}
            }
        }
        rest = &item[end..];
    }
    out.push_str(rest);
    out
}

fn crates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn write_pattern() -> Regex {
    Regex::new(&format!(
        r"(?i)\b(INSERT(\s+OR\s+\w+)?\s+INTO|REPLACE\s+INTO|DELETE\s+FROM|UPDATE)\s+(\w+\.)?({})\b",
        TRIGGER_MAINTAINED.join("|")
    ))
    .expect("regex")
}

#[test]
fn nothing_but_triggers_writes_the_search_indexes() {
    let pattern = write_pattern();
    let mut files = Vec::new();
    for entry in std::fs::read_dir(crates_dir()).expect("read crates") {
        let src = entry.expect("entry").path().join("src");
        if src.is_dir() {
            rust_files(&src, &mut files);
        }
    }
    assert!(files.len() > 50, "found only {} source files", files.len());
    let mut found = Vec::new();
    for path in files {
        let text = std::fs::read_to_string(&path).expect("read source");
        for m in pattern.find_iter(&shipped(&text)) {
            let rel = path.strip_prefix(crates_dir()).unwrap_or(&path);
            found.push(format!("{}: {}", rel.display(), m.as_str()));
        }
    }
    assert!(
        found.is_empty(),
        "these tables are maintained by migration triggers; write the base table instead:\n{}",
        found.join("\n")
    );
}

#[test]
fn match_appears_only_where_the_query_is_quoted() {
    let pattern = Regex::new(r"\bMATCH\b").expect("regex");
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    let mut found = Vec::new();
    for path in files {
        let rel = path
            .strip_prefix(&src)
            .expect("under src")
            .to_string_lossy()
            .replace('\\', "/");
        if MATCH_FILES.contains(&rel.as_str()) {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("read source");
        if pattern.is_match(&shipped(&text)) {
            found.push(rel);
        }
    }
    assert!(
        found.is_empty(),
        "fts5 MATCH outside {MATCH_FILES:?}; route the query through their quoting: {found:?}"
    );
}

#[test]
fn the_scans_see_what_they_look_for() {
    let pattern = write_pattern();
    for sql in [
        "INSERT INTO highlights_fts (rowid) VALUES (?)",
        "insert or replace into books_search VALUES (1)",
        "DELETE FROM main.provider_record_index WHERE rowid = ?",
        "UPDATE books_search SET x = 1",
    ] {
        assert!(pattern.is_match(sql), "{sql}");
    }
    assert!(!pattern.is_match("INSERT INTO highlights (id) VALUES (?)"));
    let src = "// INSERT INTO books_search\n#[cfg(test)]\nmod t { const S: &str = \"DELETE FROM books_search\"; }\nfn f() {}\n";
    assert!(!pattern.is_match(&shipped(src)));
}
