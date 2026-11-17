//! Archive paths: an `href` is joined textually to the package directory, `/`-separated.
//! `.` and `..` are not folded here; [`normalize`] is only a lookup fallback.

use std::path::Path;

/// The directory a package document sits in, as an archive path (`""` for a document at the
/// archive root).
pub(crate) fn parent_dir(path: &str) -> String {
    Path::new(path)
        .parent()
        .map(|p| slashes(&p.to_string_lossy()))
        .unwrap_or_default()
}

/// `base` joined with `href`, as `Path::join` does it: an absolute href replaces the base, an
/// empty base yields the href unchanged.
pub(crate) fn join(base: &str, href: &str) -> String {
    slashes(&Path::new(base).join(href).to_string_lossy())
}

/// Whether two archive paths name the same entry by `std::path`'s component comparison —
/// which ignores repeated separators and interior `.`, but not `..`.
#[cfg(test)]
pub(crate) fn same(a: &str, b: &str) -> bool {
    Path::new(a) == Path::new(b)
}

/// The path percent-decoded, then [`normalize`]d: the loosest key two spellings of one
/// archive entry share.
pub(crate) fn folded(path: &str) -> Option<String> {
    let decoded = percent_encoding::percent_decode_str(path)
        .decode_utf8()
        .ok()?;
    normalize(&decoded)
}

/// Whether an href is an absolute URL (`https://…`, `mailto:…`) rather than a path inside the
/// book: it starts with a URI scheme — a letter, then letters, digits, `+`, `-` or `.`, then
/// `:`.
pub(crate) fn has_scheme(href: &str) -> bool {
    let Some((scheme, _)) = href.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// The path with `.` and `..` segments folded and empty segments dropped, or `None` when a
/// `..` would climb out of the archive root.
pub(crate) fn normalize(path: &str) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop()?;
            }
            s => out.push(s),
        }
    }
    Some(out.join("/"))
}

#[cfg(windows)]
fn slashes(s: &str) -> String {
    s.replace('\\', "/")
}

#[cfg(not(windows))]
fn slashes(s: &str) -> String {
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_like_the_old_crate() {
        assert_eq!(join("OEBPS", "Text/ch1.xhtml"), "OEBPS/Text/ch1.xhtml");
        assert_eq!(join("", "ch1.xhtml"), "ch1.xhtml");
        assert_eq!(join("OEBPS", "/abs.xhtml"), "/abs.xhtml");
        assert_eq!(join("OEBPS", "../img.png"), "OEBPS/../img.png");
        assert_eq!(parent_dir("OEBPS/content.opf"), "OEBPS");
        assert_eq!(parent_dir("content.opf"), "");
        assert_eq!(parent_dir(""), "");
    }

    #[test]
    fn normalizes_dot_segments() {
        assert_eq!(normalize("OEBPS/../img.png").as_deref(), Some("img.png"));
        assert_eq!(normalize("a/./b//c").as_deref(), Some("a/b/c"));
        assert_eq!(normalize("../x"), None);
    }

    #[test]
    fn folds_and_decodes() {
        assert_eq!(
            folded("OEBPS/nav/../my%20ch.xhtml").as_deref(),
            Some("OEBPS/my ch.xhtml")
        );
        assert_eq!(folded("../x"), None);
    }

    #[test]
    fn recognises_absolute_urls() {
        for url in [
            "https://example.com/a",
            "mailto:x@y",
            "urn:isbn:1",
            "x-a.b+c:rest",
        ] {
            assert!(has_scheme(url), "{url}");
        }
        for rel in [
            "ch1.xhtml",
            "ch1.xhtml#a:b",
            "../x",
            "1http:x",
            ":x",
            "a b:c",
        ] {
            assert!(!has_scheme(rel), "{rel}");
        }
    }

    #[test]
    fn compares_by_component() {
        assert!(same("OEBPS/./ch1.xhtml", "OEBPS/ch1.xhtml"));
        assert!(same("OEBPS//ch1.xhtml", "OEBPS/ch1.xhtml"));
        assert!(!same("OEBPS/../ch1.xhtml", "ch1.xhtml"));
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// Folding never yields a path that still carries a `.`/`..` segment or an empty one, and
        /// folding twice changes nothing.
        #[test]
        fn normalize_is_idempotent_and_clean(segs in proptest::collection::vec("(\\.|\\.\\.|[a-z]{1,4}|)", 0..8)) {
            let p = segs.join("/");
            if let Some(n) = normalize(&p) {
                prop_assert!(n.split('/').all(|s| s != "." && s != ".."));
                prop_assert!(!n.contains("//"));
                prop_assert_eq!(normalize(&n), Some(n.clone()));
            }
        }

        /// A relative href with no dot segments joins to `dir/href` exactly, and the result is the
        /// same entry as its normalized form.
        #[test]
        fn plain_relative_hrefs_join_textually(
            dir in proptest::collection::vec("[A-Za-z0-9_-]{1,6}", 0..3),
            href in proptest::collection::vec("[A-Za-z0-9_ %.-]{1,6}", 1..4),
        ) {
            let dir = dir.join("/");
            let href = href.join("/");
            prop_assume!(href.split('/').all(|s| s != "." && s != ".."));
            let joined = join(&dir, &href);
            let expect = if dir.is_empty() { href.clone() } else { format!("{dir}/{href}") };
            prop_assert_eq!(&joined, &expect);
            if let Some(n) = normalize(&joined) {
                prop_assert!(same(&n, &joined));
            }
        }
    }
}
