//! Screens take their colours from `theme.rs`, so a terminal's own palette and
//! the user's accent reach every one of them. A literal RGB colour in a screen
//! would not follow either.

use std::path::{Path, PathBuf};

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

#[test]
fn screens_use_no_literal_rgb_colour() {
    let ui = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ui");
    let mut files = Vec::new();
    rust_files(&ui, &mut files);
    assert!(!files.is_empty(), "no screens under {}", ui.display());
    let mut found = Vec::new();
    for path in files {
        let text = std::fs::read_to_string(&path).expect("read source");
        for line in shipped(&text).lines() {
            if line.contains("Rgb(") {
                let rel = path.strip_prefix(&ui).unwrap_or(&path);
                found.push(format!("{}: {}", rel.display(), line.trim()));
            }
        }
    }
    assert!(
        found.is_empty(),
        "screens take colours from theme.rs:\n{}",
        found.join("\n")
    );
}
