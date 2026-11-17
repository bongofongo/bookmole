//! The testing corpus's hostile replay: hand-written hostile inputs, each with
//! the class of outcome it must have, applied straight to the engine.
//!
//! Every case in `crates/corpus/hostile.json` whose `fields` name a text field
//! the engine API takes directly is applied to an in-memory engine, and the
//! case's declared `expect` class is asserted:
//!
//! | field                    | door                                              |
//! |--------------------------|---------------------------------------------------|
//! | `note-body`, `note-title`| `Engine::create_note`, read back via `note_body`  |
//! | `title`, `author`, `isbn`| `Engine::set_book_fields` on a seeded book        |
//! | `page`                   | `i64` parse (the CLI's boundary), then `update_progress` |
//! | `highlight`              | `Storage::insert_highlight` (the `internals` feature) |
//! | `query`                  | `Engine::search_marks` must answer, not error     |
//!
//! One test per class; a failure names the case id and the field. The bank is
//! **not** edited to match what the engine does: where the engine contradicts a
//! declared class, the pair is listed in [`KNOWN_DIVERGENCES`] with what was
//! observed, so the suite stays green on today's behaviour, goes red on any
//! *new* contradiction, and goes red again when a listed one is fixed (so the
//! entry is removed and the case keeps its class). Each listed pair is reported
//! as a suspected engine bug, which is the bank's whole purpose.
//!
//! The bank is hand-edited and committed, so this suite needs no fixture and
//! never skips. It runs on every PR.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bookmole::{Book, Engine, NewNoteInput, NoteKind};
use serde_json::Value;

mod common;
use common::{engine, highlight, seed_book};

/// `(case id, field, what the engine was observed to do)`. Every entry is a
/// declared class the engine contradicts today; see the module comment.
///
/// Observed when the bank was first replayed. Two engine findings account
/// for all of them:
///
/// - **`Engine::set_book_fields` stores an ISBN without `normalize_isbn`.** The
///   CLI's `set` normalises before calling it, so `rb set --isbn` is safe; the
///   facade is not, and `crates/api`'s `set_book_fields` (the door every
///   API client uses) passes a `BookDto` straight through — so the rule "every
///   ISBN entering the system goes through `normalize_isbn`, no exceptions" has
///   an exception at the API seam.
/// - **`update_progress` accepts a negative page** and stores it as the
///   reading's `current_page` (`rb progress <book> -- -12` prints `p.-12`).
const KNOWN_DIVERGENCES: &[(&str, &str, &str)] = &[
    (
        "isbn-excel-armour",
        "isbn",
        "set_book_fields stored the armour verbatim",
    ),
    (
        "isbn-10-x-check",
        "isbn",
        "set_book_fields stored the hyphenated ISBN-10 verbatim",
    ),
    (
        "isbn-with-spaces",
        "isbn",
        "set_book_fields stored the spaced ISBN verbatim",
    ),
    (
        "isbn-bad-checksum",
        "isbn",
        "set_book_fields stored a bad check digit",
    ),
    (
        "isbn-not-applicable",
        "isbn",
        "set_book_fields stored \"N/A\" as an ISBN",
    ),
    (
        "page-negative",
        "page",
        "update_progress stored current_page = -12",
    ),
];

const APPLIED: &[&str] = &[
    "note-body",
    "note-title",
    "title",
    "author",
    "isbn",
    "page",
    "highlight",
    "query",
];

#[derive(Clone, Debug)]
struct Case {
    id: String,
    fields: Vec<String>,
    value: String,
    expect: String,
    normal: Option<String>,
}

fn bank_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/hostile.json")
}

fn bank() -> Vec<Case> {
    let text = std::fs::read_to_string(bank_path()).expect("hostile.json is committed");
    let v: Value = serde_json::from_str(&text).expect("hostile.json parses");
    v["cases"]
        .as_array()
        .expect("cases")
        .iter()
        .map(|c| Case {
            id: c["id"].as_str().unwrap().to_string(),
            fields: c["fields"]
                .as_array()
                .unwrap()
                .iter()
                .map(|f| f.as_str().unwrap().to_string())
                .collect(),
            value: c["value"].as_str().unwrap().to_string(),
            expect: c["expect"].as_str().unwrap().to_string(),
            normal: c["normal"].as_str().map(str::to_string),
        })
        .collect()
}

/// The bank's two templates, expanded as its comment says a generator must
/// (the same rule as `crates/corpus/src/reader.rs::expand`, restated here
/// because this crate may not link the generator, nor it this one).
fn expand(value: &str, self_title: &str) -> String {
    let v = value.replace("{self_title}", self_title);
    if let Some(body) = v.strip_prefix("{repeat:").and_then(|r| r.strip_suffix('}'))
        && let Some((s, n)) = body.rsplit_once(':')
        && let Ok(n) = n.parse::<usize>()
    {
        return s.repeat(n);
    }
    v
}

/// What one application produced.
#[derive(Debug)]
enum Observed {
    /// Written; this is what reads back. `path` is the vault-relative file a
    /// note landed in, for the slug checks.
    Stored { value: String, path: Option<String> },
    /// Refused with an error and nothing written (checked by the door). The
    /// message is read only through `Debug`, in a failure line.
    #[allow(dead_code)]
    Refused(String),
    /// Accepted without error but nothing was written — refusal by omission.
    Dropped,
    /// The door panicked.
    Panicked(String),
}

/// Apply `value` through the door for `field`. Runs in a spawned task, so a
/// panic is observed and attributed to its case instead of aborting the test.
async fn apply(engine: &Arc<Engine>, case: &Case, field: &str) -> Observed {
    let engine = Arc::clone(engine);
    let case = case.clone();
    let field = field.to_string();
    match tokio::spawn(async move { door(&engine, &case, &field).await }).await {
        Ok(o) => o,
        Err(e) if e.is_panic() => Observed::Panicked(format!("{e:?}")),
        Err(e) => Observed::Panicked(format!("join error: {e}")),
    }
}

async fn door(engine: &Engine, case: &Case, field: &str) -> Observed {
    match field {
        "note-body" => {
            let title = format!("hostile body {}", case.id);
            let body = expand(&case.value, &title);
            let created = match engine
                .create_note(NewNoteInput {
                    kind: NoteKind::Note,
                    title: Some(title),
                    body,
                    ..Default::default()
                })
                .await
            {
                Ok(c) => c,
                Err(e) => return Observed::Refused(e.to_string()),
            };
            let rec = engine.get_note(created.id).await.unwrap().unwrap();
            Observed::Stored {
                value: engine.note_body(&rec).unwrap(),
                path: Some(rec.file_path),
            }
        }
        "note-title" => {
            let title = expand(&case.value, "");
            match engine
                .create_note(NewNoteInput {
                    kind: NoteKind::Note,
                    title: Some(title),
                    body: format!("body for the hostile title {}", case.id),
                    ..Default::default()
                })
                .await
            {
                Ok(c) => {
                    let rec = engine.get_note(c.id).await.unwrap().unwrap();
                    Observed::Stored {
                        value: rec.title,
                        path: Some(rec.file_path),
                    }
                }
                Err(e) => Observed::Refused(e.to_string()),
            }
        }
        "title" | "author" | "isbn" => {
            let id = seed_book(engine, &format!("seed for {}", case.id)).await;
            let value = expand(&case.value, "");
            let fields = match field {
                "title" => Book {
                    title: Some(value),
                    ..Default::default()
                },
                "author" => Book {
                    authors: vec![value],
                    ..Default::default()
                },
                _ => Book {
                    isbn_13: Some(value),
                    ..Default::default()
                },
            };
            if let Err(e) = engine.set_book_fields(id, &fields).await {
                return Observed::Refused(e.to_string());
            }
            let b = engine.get_book(id).await.unwrap().unwrap();
            match field {
                "title" => Observed::Stored {
                    value: b.title.unwrap_or_default(),
                    path: None,
                },
                "author" => Observed::Stored {
                    value: b.authors.join("\u{1f}"),
                    path: None,
                },
                _ => match b.isbn_13 {
                    Some(v) => Observed::Stored {
                        value: v,
                        path: None,
                    },
                    None => Observed::Dropped,
                },
            }
        }
        "page" => {
            // The CLI's `progress <book> <page>` parses an i64 before the engine
            // sees anything; that parse is the boundary for typed text.
            let page = match case.value.trim().parse::<i64>() {
                Ok(p) => p,
                Err(e) => return Observed::Refused(format!("not an i64: {e}")),
            };
            let id = seed_book(engine, &format!("seed for {}", case.id)).await;
            if let Err(e) = engine.update_progress(id, Some(page), None).await {
                return Observed::Refused(e.to_string());
            }
            match engine
                .active_reading(id)
                .await
                .unwrap()
                .and_then(|r| r.current_page)
            {
                Some(p) => Observed::Stored {
                    value: p.to_string(),
                    path: None,
                },
                None => Observed::Dropped,
            }
        }
        "highlight" => {
            let id = seed_book(engine, &format!("seed for {}", case.id)).await;
            let text = expand(&case.value, "");
            let h = highlight(&text, "2025-03-01 21:15:00");
            match engine.storage().insert_highlight(id, &h).await {
                Err(e) => Observed::Refused(e.to_string()),
                Ok(None) => Observed::Dropped,
                Ok(Some(hid)) => {
                    let all = engine.list_highlights(id).await.unwrap();
                    match all.into_iter().find(|x| x.id == hid) {
                        Some(x) => Observed::Stored {
                            value: x.text,
                            path: None,
                        },
                        None => Observed::Dropped,
                    }
                }
            }
        }
        "query" => match engine.search_marks(&case.value, None, None, 20).await {
            Ok(hits) => Observed::Stored {
                value: format!("{} hits", hits.len()),
                path: None,
            },
            Err(e) => Observed::Refused(e.to_string()),
        },
        other => unreachable!("{other} is not an applied field"),
    }
}

/// Does `observed` satisfy `case`'s class for `field`? `Err` says why not.
fn verdict(case: &Case, field: &str, observed: &Observed) -> Result<(), String> {
    if let Observed::Panicked(p) = observed {
        return Err(format!("panicked: {p}"));
    }
    // A query is checked the same way whatever its class: the search answers.
    if field == "query" {
        return match observed {
            Observed::Stored { .. } => Ok(()),
            other => Err(format!("search did not answer: {other:?}")),
        };
    }
    let input = expand(&case.value, &format!("hostile body {}", case.id));
    match case.expect.as_str() {
        "verbatim" => match observed {
            Observed::Stored { value, .. } => {
                // `note_body` returns the body minus trailing whitespace — the
                // vault's stated rule (`update_note_body` trims the same way).
                let want = if field == "note-body" {
                    input.trim_end().to_string()
                } else {
                    input.clone()
                };
                if *value == want {
                    Ok(())
                } else {
                    Err(format!("read back {value:?}, wanted {want:?}"))
                }
            }
            other => Err(format!("not stored: {other:?}")),
        },
        "normalised" => {
            let normal = case.normal.clone().unwrap_or_default();
            match (field, observed) {
                // A title's normal form is the slug the vault makes of it, and
                // it must stay inside the vault.
                ("note-title", Observed::Stored { path: Some(p), .. }) => {
                    let stem = Path::new(p)
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    if p.contains("..") || p.starts_with('/') {
                        Err(format!("vault path escapes: {p:?}"))
                    } else if stem == normal || stem.ends_with(&format!("-{normal}")) {
                        Ok(())
                    } else {
                        Err(format!("slug {stem:?}, wanted {normal:?}"))
                    }
                }
                ("title", Observed::Stored { value, .. }) => {
                    let slug = bookmole::notes::slugify(value);
                    if slug == normal {
                        Ok(())
                    } else {
                        Err(format!(
                            "stored {value:?} slugs to {slug:?}, wanted {normal:?}"
                        ))
                    }
                }
                (_, Observed::Stored { value, .. }) if *value == normal => Ok(()),
                (_, other) => Err(format!("observed {other:?}, wanted {normal:?}")),
            }
        }
        "rejected" => match observed {
            Observed::Refused(_) | Observed::Dropped => Ok(()),
            Observed::Stored { value, .. } => Err(format!("accepted and stored {value:?}")),
            Observed::Panicked(_) => unreachable!(),
        },
        // Not panicking was checked above; the engine-still-answers half is
        // checked once per run by `still_answers`.
        "no-crash" => Ok(()),
        other => Err(format!("unknown class {other}")),
    }
}

/// After every application: the engine still answers a search and finds the
/// sentinel note written before the first case.
async fn still_answers(engine: &Engine) -> Result<(), String> {
    let hits = engine
        .search_marks("quillsentinel", None, None, 5)
        .await
        .map_err(|e| format!("search_marks errored: {e}"))?;
    if hits.is_empty() {
        return Err("the sentinel note is no longer found".into());
    }
    engine
        .resolve_books("seed for")
        .await
        .map_err(|e| format!("resolve_books errored: {e}"))?;
    Ok(())
}

/// Run every applicable `(case, field)` of one class; collect failures,
/// excusing (and re-checking) the listed divergences.
async fn run_class(class: &str) {
    let (_tmp, engine) = engine().await;
    let engine = Arc::new(engine);
    engine
        .create_note(NewNoteInput {
            kind: NoteKind::Note,
            title: Some("sentinel".into()),
            body: "quillsentinel marks the engine as still answering".into(),
            ..Default::default()
        })
        .await
        .unwrap();

    let cases: Vec<Case> = bank().into_iter().filter(|c| c.expect == class).collect();
    assert!(!cases.is_empty(), "no {class} cases in the bank");
    let mut failures = Vec::new();
    let mut fixed = Vec::new();
    let mut applied = 0;
    for case in &cases {
        for field in case.fields.iter().filter(|f| APPLIED.contains(&f.as_str())) {
            applied += 1;
            let observed = apply(&engine, case, field).await;
            let outcome = verdict(case, field, &observed);
            let known = KNOWN_DIVERGENCES
                .iter()
                .any(|(id, f, _)| *id == case.id && *f == field.as_str());
            match (outcome, known) {
                (Err(why), false) => failures.push(format!("{} [{field}]: {why}", case.id)),
                (Ok(()), true) => fixed.push(format!(
                    "{} [{field}]: now meets `{class}` — remove it from KNOWN_DIVERGENCES",
                    case.id
                )),
                _ => {}
            }
            if let Err(why) = still_answers(&engine).await {
                failures.push(format!("{} [{field}]: after applying, {why}", case.id));
            }
        }
    }
    assert!(applied > 0, "no {class} case names an applied field");
    failures.extend(fixed);
    assert!(
        failures.is_empty(),
        "{} of {applied} `{class}` applications failed:\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verbatim_cases_read_back_exactly() {
    run_class("verbatim").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn normalised_cases_read_back_as_their_normal_form() {
    run_class("normalised").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejected_cases_are_refused_and_write_nothing() {
    run_class("rejected").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_crash_cases_do_not_panic_and_search_still_answers() {
    run_class("no-crash").await;
}

/// The declaration is the contract; this keeps it well-formed.
#[test]
fn the_bank_is_well_formed() {
    let cases = bank();
    let mut ids = std::collections::BTreeSet::new();
    for c in &cases {
        assert!(ids.insert(c.id.clone()), "duplicate id {}", c.id);
        assert!(
            ["verbatim", "normalised", "rejected", "no-crash"].contains(&c.expect.as_str()),
            "{}: unknown class {}",
            c.id,
            c.expect
        );
        assert_eq!(
            c.expect == "normalised",
            c.normal.is_some(),
            "{}: `normal` goes with `normalised` and only with it",
            c.id
        );
        assert!(!c.fields.is_empty(), "{} names no field", c.id);
    }
    for (id, field, _) in KNOWN_DIVERGENCES {
        let c = cases
            .iter()
            .find(|c| c.id == *id)
            .unwrap_or_else(|| panic!("KNOWN_DIVERGENCES names unknown case {id}"));
        assert!(
            c.fields.iter().any(|f| f == field),
            "{id} is not declared for {field}"
        );
    }
}
