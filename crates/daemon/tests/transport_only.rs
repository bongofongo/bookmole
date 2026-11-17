//! The daemon moves calls and never names one: a branch on a `Request` variant
//! here would give a socket client behaviour an in-process host does not get.

use std::path::Path;

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

#[test]
fn the_daemon_names_no_request_variant() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = Vec::new();
    for entry in std::fs::read_dir(&src).expect("read src") {
        let path = entry.expect("entry").path();
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("read source");
        for line in shipped(&text).lines() {
            if line.contains("Request::") {
                found.push(format!("{}: {}", path.display(), line.trim()));
            }
        }
    }
    assert!(
        found.is_empty(),
        "the daemon is transport only; dispatch belongs to bookmole-api:\n{}",
        found.join("\n")
    );
}

#[test]
fn shipped_drops_test_items_and_comments() {
    let src = "use a; // Request::X\n#[cfg(test)]\nmod t { fn f() { Request::Y; } }\nfn g() { Request::Z }\n";
    let kept = shipped(src);
    assert!(!kept.contains("Request::X"));
    assert!(!kept.contains("Request::Y"));
    assert!(kept.contains("Request::Z"));
}
