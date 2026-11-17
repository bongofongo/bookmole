//! A folder handed to *Add a book*, read as the book files in it.
//!
//! Every file request takes one path, and a webview frontend has no filesystem
//! of its own, so a folder chosen in a dialog or dropped on a window arrives as
//! one path with no extension. This turns a list of paths — files and folders
//! mixed — into the book files they hold, which a frontend then stages exactly
//! as it stages files picked one by one (`IdentifyFile`, then `ImportFile`). It
//! **reads directory listings and nothing else**: no file is opened, nothing is
//! written, and matching stays where it was.
//!
//! Four decisions, each a fact a frontend words rather than works out:
//!
//! - **A file passes through untouched**, whatever its extension. The caller
//!   named it; whether it is a book is the next step's question, and that step
//!   already answers *not a book file* about a `.docx`. Only what the walk
//!   *finds* is filtered by [`BOOK_EXTENSIONS`].
//! - **The walk is bounded**: folders [`MAX_DEPTH`] deep and no deeper, at most
//!   [`MAX_FILES`] files back, hidden files and folders skipped, a symlinked
//!   folder never followed (a link back up the tree is a loop, and a link out
//!   of it is somebody else's disk). What a bound cut off is said, typed:
//!   [`ExpandedPaths::cut_off_at`] and [`SkippedEntries`] — a silent cap is
//!   indistinguishable from a small folder.
//! - **A calibre library is not walked.** calibre owns the files under a
//!   folder holding its `metadata.db` (calibre is the origin of its files,
//!   and bookmole points at them rather than owning them), and bookmole
//!   reads them through calibre, which knows which file is which book. Walking
//!   one would point at calibre's files as loose copies and read every format
//!   of a book as a book of its own. So it is returned as a fact —
//!   [`ExpandedPaths::calibre_libraries`] — at any depth, and the frontend
//!   points at the calibre importer.
//! - **The order is the folder's**, deterministic: each input in the order
//!   given, and inside a folder a depth-first walk with every listing sorted by
//!   name, so the files come back in path order and the same folder gives the
//!   same list twice. A path reached twice is returned once.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::calibre::is_calibre_library;

/// The kinds of file a walk returns — the same four a frontend's file picker
/// should offer, so a folder and a hand-picked file accept the same kinds.
/// Compared case-insensitively.
pub const BOOK_EXTENSIONS: &[&str] = &["epub", "pdf", "mobi", "azw3"];

/// How many folders deep a walk reads. The folder chosen is depth 0, so
/// calibre's own `Author/Title/book.epub` shape (depth 2) and a folder of such
/// folders fit with room to spare. A folder below this is counted, not read.
pub const MAX_DEPTH: usize = 4;

/// The most files one expansion returns. A walk that finds more stops there
/// and says so; it does not guess at how many it left.
pub const MAX_FILES: usize = 5000;

/// What a list of paths came to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExpandedPaths {
    /// The files: every input that was not a folder, as given, and every book
    /// file found inside the folders, in walk order. No path twice.
    pub files: Vec<PathBuf>,
    /// Folders that are calibre libraries, chosen or found, and not walked.
    pub calibre_libraries: Vec<PathBuf>,
    /// What the walk passed over, by why.
    pub skipped: SkippedEntries,
    /// `Some(limit)` when the walk found more book files than it may return
    /// and stopped at `limit`; what lay beyond was not looked at.
    pub cut_off_at: Option<usize>,
}

/// Entries inside the chosen folders that are not in [`ExpandedPaths::files`],
/// counted by reason. Counts rather than paths: a folder of four hundred
/// cover images is one sentence, not four hundred rows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SkippedEntries {
    /// Files and folders whose name starts with `.`.
    pub hidden: usize,
    /// Files that are not one of [`BOOK_EXTENSIONS`].
    pub not_books: usize,
    /// Symbolic links to folders, not followed.
    pub linked_folders: usize,
    /// Folders deeper than [`MAX_DEPTH`], not read.
    pub too_deep: usize,
    /// Folders or entries that could not be read (permissions, a vanished
    /// volume).
    pub unreadable: usize,
}

/// The bounds, separate so a test can walk a small tree into them.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Bounds {
    pub depth: usize,
    pub files: usize,
}

impl Bounds {
    pub(crate) const SHIPPED: Bounds = Bounds {
        depth: MAX_DEPTH,
        files: MAX_FILES,
    };
}

/// Whether a path names a book file by its extension.
pub fn is_book_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| BOOK_EXTENSIONS.iter().any(|b| b.eq_ignore_ascii_case(e)))
}

/// Expand `paths` within `bounds`. Blocking; the facade runs it off the
/// runtime.
pub(crate) fn expand(paths: &[PathBuf], bounds: Bounds) -> ExpandedPaths {
    let mut walk = Walk {
        bounds,
        out: ExpandedPaths::default(),
        seen: HashSet::new(),
    };
    for path in paths {
        if walk.out.cut_off_at.is_some() {
            break;
        }
        // The caller named this path, so a link to a folder is followed here —
        // it is only links met *inside* a walk that are not.
        if fs::metadata(path).is_ok_and(|m| m.is_dir()) {
            walk.folder(path, 0);
        } else {
            walk.take(path.clone());
        }
    }
    walk.out
}

struct Walk {
    bounds: Bounds,
    out: ExpandedPaths,
    seen: HashSet<PathBuf>,
}

impl Walk {
    /// Add one file, unless it was added already or the limit is reached.
    fn take(&mut self, path: PathBuf) {
        if self.seen.contains(&path) {
            return;
        }
        if self.out.files.len() >= self.bounds.files {
            self.out.cut_off_at = Some(self.bounds.files);
            return;
        }
        self.seen.insert(path.clone());
        self.out.files.push(path);
    }

    fn folder(&mut self, dir: &Path, depth: usize) {
        if is_calibre_library(dir) {
            if !self.out.calibre_libraries.iter().any(|c| c == dir) {
                self.out.calibre_libraries.push(dir.to_path_buf());
            }
            return;
        }
        let Ok(listing) = fs::read_dir(dir) else {
            self.out.skipped.unreadable += 1;
            return;
        };
        let mut entries: Vec<fs::DirEntry> = Vec::new();
        for entry in listing {
            match entry {
                Ok(e) => entries.push(e),
                Err(_) => self.out.skipped.unreadable += 1,
            }
        }
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            if self.out.cut_off_at.is_some() {
                return;
            }
            let path = entry.path();
            if entry.file_name().to_string_lossy().starts_with('.') {
                self.out.skipped.hidden += 1;
                continue;
            }
            let Ok(kind) = entry.file_type() else {
                self.out.skipped.unreadable += 1;
                continue;
            };
            // A link is read for what it points at: a link to a file is that
            // file, a link to a folder is not followed.
            let (is_dir, is_file) = if kind.is_symlink() {
                match fs::metadata(&path) {
                    Ok(m) if m.is_dir() => {
                        self.out.skipped.linked_folders += 1;
                        continue;
                    }
                    Ok(m) => (false, m.is_file()),
                    Err(_) => {
                        self.out.skipped.unreadable += 1;
                        continue;
                    }
                }
            } else {
                (kind.is_dir(), kind.is_file())
            };
            if is_dir {
                if depth + 1 > self.bounds.depth {
                    self.out.skipped.too_deep += 1;
                } else {
                    self.folder(&path, depth + 1);
                }
            } else if is_file && is_book_file(&path) {
                self.take(path);
            } else {
                self.out.skipped.not_books += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(root: &Path, rel: &str) -> PathBuf {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, b"x").unwrap();
        p
    }

    fn rel(root: &Path, files: &[PathBuf]) -> Vec<String> {
        files
            .iter()
            .map(|p| {
                p.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect()
    }

    #[test]
    fn a_folder_of_folders_comes_back_as_its_book_files_in_path_order() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        touch(root, "b.epub");
        touch(root, "a.PDF");
        touch(root, "Austen/Emma/emma.mobi");
        touch(root, "Austen/Emma/cover.jpg");
        touch(root, "Austen/persuasion.azw3");
        touch(root, "notes.txt");
        let got = expand(&[root.to_path_buf()], Bounds::SHIPPED);
        assert_eq!(
            rel(root, &got.files),
            [
                "Austen/Emma/emma.mobi",
                "Austen/persuasion.azw3",
                "a.PDF",
                "b.epub"
            ]
        );
        assert_eq!(got.skipped.not_books, 2);
        assert_eq!(got.cut_off_at, None);
        assert!(got.calibre_libraries.is_empty());
        // The same folder twice is the same list.
        assert_eq!(expand(&[root.to_path_buf()], Bounds::SHIPPED), got);
    }

    #[test]
    fn a_file_passes_through_whatever_it_is_and_only_once() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let docx = touch(root, "list.docx");
        let epub = touch(root, "dir/one.epub");
        let missing = root.join("gone.epub");
        let got = expand(
            &[
                docx.clone(),
                root.join("dir"),
                epub.clone(),
                missing.clone(),
            ],
            Bounds::SHIPPED,
        );
        assert_eq!(got.files, [docx, epub, missing]);
    }

    #[test]
    fn hidden_entries_are_skipped_and_counted() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        touch(root, ".hidden.epub");
        touch(root, ".git/x.epub");
        touch(root, "._shadow.epub");
        touch(root, "seen.epub");
        let got = expand(&[root.to_path_buf()], Bounds::SHIPPED);
        assert_eq!(rel(root, &got.files), ["seen.epub"]);
        assert_eq!(got.skipped.hidden, 3);
    }

    #[test]
    fn the_walk_stops_at_its_depth_and_says_so() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        touch(root, "1/2/3/4/at-four.epub");
        touch(root, "1/2/3/4/5/at-five.epub");
        let got = expand(&[root.to_path_buf()], Bounds::SHIPPED);
        assert_eq!(rel(root, &got.files), ["1/2/3/4/at-four.epub"]);
        assert_eq!(got.skipped.too_deep, 1);
    }

    #[test]
    fn the_walk_stops_at_its_limit_and_says_where() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for i in 0..5 {
            touch(root, &format!("{i}.epub"));
        }
        let bounds = Bounds { depth: 4, files: 3 };
        let got = expand(&[root.to_path_buf()], bounds);
        assert_eq!(rel(root, &got.files), ["0.epub", "1.epub", "2.epub"]);
        assert_eq!(got.cut_off_at, Some(3));
        // Exactly at the limit is not cut off: nothing was left behind.
        let exact = expand(&[root.to_path_buf()], Bounds { depth: 4, files: 5 });
        assert_eq!(exact.files.len(), 5);
        assert_eq!(exact.cut_off_at, None);
    }

    #[test]
    fn a_calibre_library_is_named_and_not_walked_at_any_depth() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        touch(root, "Calibre Library/metadata.db");
        touch(root, "Calibre Library/Author/Title (1)/Title - Author.epub");
        touch(root, "loose.epub");
        let got = expand(&[root.to_path_buf()], Bounds::SHIPPED);
        assert_eq!(rel(root, &got.files), ["loose.epub"]);
        assert_eq!(got.calibre_libraries, [root.join("Calibre Library")]);
        // Chosen directly, it is the same fact and nothing else.
        let direct = expand(&[root.join("Calibre Library")], Bounds::SHIPPED);
        assert!(direct.files.is_empty());
        assert_eq!(direct.calibre_libraries, [root.join("Calibre Library")]);
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_folder_is_not_followed_and_a_linked_file_is_read() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let real = touch(root, "real/book.epub");
        std::os::unix::fs::symlink(root, root.join("real/loop")).unwrap();
        std::os::unix::fs::symlink(&real, root.join("real/alias.epub")).unwrap();
        let got = expand(&[root.join("real")], Bounds::SHIPPED);
        assert_eq!(rel(root, &got.files), ["real/alias.epub", "real/book.epub"]);
        assert_eq!(got.skipped.linked_folders, 1);
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(24))]

            /// Whatever the tree, the answer is within its bounds, in path
            /// order, without a repeat, and holds only book files.
            #[test]
            fn a_walk_is_bounded_sorted_and_only_books(
                files in proptest::collection::vec(
                    ("[ab]{1,2}(/[ab]{1,2}){0,5}", prop_oneof![Just("epub"), Just("txt"), Just("pdf")]),
                    0..24,
                ),
                limit in 1usize..12,
            ) {
                let tmp = tempfile::tempdir().unwrap();
                let root = tmp.path();
                for (stem, ext) in &files {
                    let p = root.join(format!("{stem}.{ext}"));
                    // A stem can name both a file and a folder; the second is
                    // simply not made.
                    if fs::create_dir_all(p.parent().unwrap()).is_ok() {
                        let _ = fs::write(&p, b"x");
                    }
                }
                let got = expand(&[root.to_path_buf()], Bounds { depth: 3, files: limit });
                prop_assert!(got.files.len() <= limit);
                prop_assert!(got.files.windows(2).all(|w| w[0] < w[1]), "{:?}", got.files);
                prop_assert!(got.files.iter().all(|p| is_book_file(p)));
                for p in &got.files {
                    let depth = p.strip_prefix(root).unwrap().components().count() - 1;
                    prop_assert!(depth <= 3);
                }
            }
        }
    }
}
