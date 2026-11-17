//! `set` writes through `Source::User`, so corrected fields are held against later enrich runs.

use anyhow::{Result, bail};
use bookmole::{
    Book, Engine, EnrichCandidate, EnrichMatch, EnrichOutcome, FieldChange, HeldField, Source,
    series_index_text,
};

use super::resolve_one;

pub async fn enrich(engine: &Engine, selector: &str) -> Result<()> {
    let book = resolve_one(engine, selector).await?;
    let id = book.id.expect("a stored book has an id");
    let report = engine.enrich_book_from_providers(id).await?;

    for w in &report.warnings {
        println!("warning: {w}");
    }

    let title = book.display_title();
    match &report.outcome {
        EnrichOutcome::Enriched(how) => {
            println!("{title} — looked up again ({})", matched_by(*how));
            if !report.wrote_anything() {
                println!("    everything they know, you already had.");
            }
            for c in &report.filled {
                println!("{}", filled_line(c));
            }
            if let Some(path) = &report.cover {
                println!("    {:<16}{}", "cover", path.display());
            }
        }
        EnrichOutcome::Refused { candidates } => {
            println!("{title}: nothing written — nothing came back that is certainly this book.");
            for c in candidates {
                print_candidate(c);
            }
            print_next_moves(selector, candidates);
        }
        EnrichOutcome::NothingFound => {
            println!("{title}: no provider knows this book.");
            if book.isbn_13.is_some() || book.isbn_10.is_some() {
                // An ISBN names an edition; no fall-through to a title search, which would write another
                // edition's data over this one.
                println!("    that is about this ISBN, not about the book.");
                println!("    wrong ISBN? bookmole set {selector} --isbn <isbn>");
            }
        }
        EnrichOutcome::NoAnswer => {
            // Not "no provider knows this book": a provider that never answered says nothing about it.
            println!("{title}: no answer — the providers above did not come back.");
            println!("    nothing was written. try again in a moment.");
        }
        EnrichOutcome::Unaskable => {
            println!("{title}: nothing to ask with — no ISBN, and no title to match on.");
            println!("    bookmole set {selector} --title \"...\" --author \"...\"");
        }
    }

    for h in &report.held {
        print_held(selector, h);
    }
    Ok(())
}

fn matched_by(how: EnrichMatch) -> String {
    match how {
        EnrichMatch::Isbn => "by isbn".to_string(),
        EnrichMatch::Title(score) => format!("by title, {:.0}%", score * 100.0),
    }
}

fn filled_line(c: &FieldChange) -> String {
    let source = c.source.map(source_name).unwrap_or("unattributed");
    let value = clip(&c.after);
    match &c.before {
        Some(before) => format!(
            "    {:<16}{value}  [{source}, was {}]",
            c.field,
            clip(before)
        ),
        None => format!("    {:<16}{value}  [{source}]", c.field),
    }
}

fn print_held(selector: &str, h: &HeldField) {
    let by = h.offered_by.map(source_name).unwrap_or("a provider");
    println!(
        "    held back: {} — {by} says {}, yours says {}",
        h.field,
        clip(&h.offered),
        h.kept.as_deref().map(clip).unwrap_or_else(|| "—".into()),
    );
    match flag_of(h.field) {
        Some(flag) => {
            println!("               yours stays. bookmole set {selector} --{flag} …")
        }
        // No `set` flag reaches this column, so no move is offered: a named flag would not exist.
        None => println!("               yours stays."),
    }
}

fn print_candidate(c: &EnrichCandidate) {
    let mut detail: Vec<String> = Vec::new();
    if !c.book.authors.is_empty() {
        detail.push(c.book.authors.join(", "));
    }
    if let Some(year) = c.book.publish_year {
        detail.push(year.to_string());
    }
    if let Some(publisher) = &c.book.publisher {
        detail.push(publisher.clone());
    }
    let sources: Vec<String> = c.sources.iter().map(|s| s.to_string()).collect();
    println!(
        "    maybe: {} ({}) {:.0}% [{}]",
        c.book.display_title(),
        detail.join(", "),
        c.score * 100.0,
        sources.join(", ")
    );
    match c.book.isbn_13.as_deref().or(c.book.isbn_10.as_deref()) {
        Some(isbn) => println!("           isbn {isbn}"),
        None => println!("           no isbn — nothing here can name this edition"),
    }
}

/// Next moves name durable things, never an index into a candidate list a later run cannot
/// reproduce.
fn print_next_moves(selector: &str, candidates: &[EnrichCandidate]) {
    if let Some(isbn) = candidates
        .iter()
        .find_map(|c| c.book.isbn_13.as_deref().or(c.book.isbn_10.as_deref()))
    {
        println!("    it is one of these : bookmole set {selector} --isbn {isbn}");
        println!("                         then run enrich again");
    }
    println!("    or it is described wrongly here:");
    println!("                         bookmole set {selector} --title \"...\" --author \"...\"");
}

fn source_name(s: Source) -> &'static str {
    s.as_str()
}

/// Exhaustive, `None` for columns without a flag: a guessed flag name (`--translators`) would be a
/// clap error.
fn flag_of(field: &str) -> Option<&'static str> {
    Some(match field {
        "title" => "title",
        "authors" => "author",
        "translators" => "translator",
        "publisher" => "publisher",
        "publish_year" => "year",
        "language" => "language",
        "page_count" => "page-count",
        "description" => "description",
        "isbn_10" | "isbn_13" => "isbn",
        "subjects" => "subject",
        "series" => "series",
        "series_index" => "series-index",
        _ => return None,
    })
}

fn clip(s: &str) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= 58 {
        return flat;
    }
    flat.chars().take(57).collect::<String>() + "…"
}

/// What `bookmole set` can write: fields a provider also writes, not a general row editor.
#[derive(clap::Args, Debug, Default)]
pub struct SetArgs {
    /// Book selector: id, ISBN, or title fragment
    pub book: String,
    #[arg(long)]
    pub title: Option<String>,
    /// Repeatable; replaces the author list
    #[arg(long = "author")]
    pub authors: Vec<String>,
    /// Repeatable; replaces the translator list
    #[arg(long = "translator")]
    pub translators: Vec<String>,
    #[arg(long)]
    pub publisher: Option<String>,
    #[arg(long)]
    pub year: Option<i64>,
    /// BCP-47 or MARC code, as the providers give it
    #[arg(long)]
    pub language: Option<String>,
    #[arg(long)]
    pub isbn: Option<String>,
    #[arg(long = "page-count")]
    pub page_count: Option<i64>,
    #[arg(long)]
    pub description: Option<String>,
    /// Repeatable; replaces the subject set.
    #[arg(long = "subject")]
    pub subjects: Vec<String>,
    #[arg(long)]
    pub series: Option<String>,
    /// Fractional is fine. Refused when the book is in no series and `--series` is not given.
    #[arg(long = "series-index")]
    pub series_index: Option<f64>,
}

/// Fields written here are stamped `user` and are never overwritten by provider merges.
pub async fn set(engine: &Engine, args: &SetArgs) -> Result<()> {
    let book = resolve_one(engine, &args.book).await?;
    let id = book.id.expect("a stored book has an id");

    let mut fields = Book {
        title: args.title.clone(),
        authors: args.authors.clone(),
        translators: args.translators.clone(),
        publisher: args.publisher.clone(),
        publish_year: args.year,
        language: args.language.clone(),
        page_count: args.page_count,
        description: args.description.clone(),
        subjects: args.subjects.clone(),
        series: args.series.clone(),
        series_index: args.series_index,
        ..Default::default()
    };
    if let Some(raw) = &args.isbn {
        let isbn = bookmole::normalize_isbn(raw)
            .ok_or_else(|| anyhow::anyhow!("{raw:?} is not a valid ISBN"))?;
        if isbn.len() == 13 {
            fields.isbn_13 = Some(isbn);
        } else {
            fields.isbn_10 = Some(isbn);
        }
    }

    if is_empty(&fields) {
        bail!("nothing to set — name at least one field (--help lists them)");
    }
    if let Some(orphan) = orphan_index(&fields, &book) {
        bail!(
            "{} is in no series — name it too: --series \"…\" --series-index {}",
            book.display_title(),
            series_index_text(orphan)
        );
    }

    let after = engine.set_book_fields(id, &fields).await?;
    println!("{} — set, and recorded as yours:", after.display_title());
    for (field, value) in changed(&fields) {
        println!("    {field:<16}{}", clip(&value));
    }
    println!("    no provider merge will overwrite these.");
    // `set_book_fields` cannot clear, so a renamed series keeps its old number; say so.
    if fields.series.is_some() && fields.series_index.is_none() && after.series_index.is_some() {
        println!(
            "    note: still numbered {} — --series-index changes it.",
            after
                .series_index
                .map(series_index_text)
                .expect("checked just above")
        );
    }
    Ok(())
}

/// Read off the request, not the stored row: a field set to its existing value is still claimed.
fn changed(fields: &Book) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    let mut push = |name, value: Option<String>| {
        if let Some(v) = value {
            out.push((name, v));
        }
    };
    push("title", fields.title.clone());
    push(
        "authors",
        (!fields.authors.is_empty()).then(|| fields.authors.join(", ")),
    );
    push(
        "translators",
        (!fields.translators.is_empty()).then(|| fields.translators.join(", ")),
    );
    push("publisher", fields.publisher.clone());
    push("publish_year", fields.publish_year.map(|y| y.to_string()));
    push("language", fields.language.clone());
    push("isbn_10", fields.isbn_10.clone());
    push("isbn_13", fields.isbn_13.clone());
    push("page_count", fields.page_count.map(|n| n.to_string()));
    push("description", fields.description.clone());
    push(
        "subjects",
        (!fields.subjects.is_empty()).then(|| fields.subjects.join(", ")),
    );
    push("series", fields.series.clone());
    push("series_index", fields.series_index.map(series_index_text));
    out
}

fn is_empty(fields: &Book) -> bool {
    changed(fields).is_empty()
}

/// Checked against the stored series too: numbering a book already in a series needs no `--series`.
fn orphan_index(fields: &Book, stored: &Book) -> Option<f64> {
    fields
        .series_index
        .filter(|_| fields.series.is_none() && stored.series.is_none())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(field: &'static str, before: Option<&str>, after: &str) -> FieldChange {
        FieldChange {
            field,
            source: Some(Source::OpenLibrary),
            before: before.map(str::to_string),
            after: after.to_string(),
        }
    }

    #[test]
    fn a_filled_field_says_who_supplied_it() {
        let line = filled_line(&change("page_count", None, "490"));
        assert!(line.contains("page_count"), "{line}");
        assert!(line.contains("490"), "{line}");
        assert!(line.contains("openlibrary"), "{line}");
        assert!(!line.contains("was"), "no old value to report: {line}");
    }

    #[test]
    fn a_replaced_field_shows_what_it_replaced() {
        let line = filled_line(&change("page_count", Some("100"), "490"));
        assert!(line.contains("was 100"), "{line}");
    }

    #[test]
    fn a_description_is_flattened_and_clipped() {
        let prose = "A sweeping saga\nof four generations\n".to_string() + &"x".repeat(200);
        let out = clip(&prose);
        assert!(!out.contains('\n'));
        assert_eq!(out.chars().count(), 58);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn short_text_is_left_alone() {
        assert_eq!(clip("Middlemarch"), "Middlemarch");
    }

    #[test]
    fn every_held_field_names_a_real_flag() {
        use clap::CommandFactory;
        #[derive(clap::Parser)]
        struct Wrapper {
            #[command(flatten)]
            set: SetArgs,
        }
        let cmd = Wrapper::command();
        let flags: Vec<String> = cmd
            .get_arguments()
            .filter_map(|a| a.get_long().map(str::to_string))
            .collect();
        for field in [
            "title",
            "authors",
            "publisher",
            "publish_year",
            "language",
            "page_count",
            "description",
            "isbn_10",
            "isbn_13",
            "translators",
            "subjects",
            "series",
            "series_index",
        ] {
            let flag = flag_of(field).unwrap_or_else(|| panic!("{field} has no `set` flag"));
            assert!(
                flags.contains(&flag.to_string()),
                "held field {field} points at --{flag}, which `set` does not take: {flags:?}"
            );
        }
        assert_eq!(flag_of("cover_url"), None);
    }

    #[test]
    fn an_index_is_refused_only_when_no_series_names_it() {
        let numbered = |n: f64| Book {
            series_index: Some(n),
            ..Default::default()
        };
        let in_dune = Book {
            series: Some("Dune".into()),
            ..Default::default()
        };
        assert_eq!(
            orphan_index(&numbered(2.0), &Book::default()),
            Some(2.0),
            "nothing names this number"
        );
        assert_eq!(orphan_index(&numbered(2.5), &in_dune), None);
        assert_eq!(
            orphan_index(
                &Book {
                    series: Some("Dune".into()),
                    ..numbered(2.0)
                },
                &Book::default()
            ),
            None
        );
        assert_eq!(orphan_index(&in_dune, &Book::default()), None);
    }

    #[test]
    fn the_echo_names_the_pair_and_the_subjects() {
        let fields = Book {
            subjects: vec!["Fiction / Literary".into()],
            series: Some("Dune".into()),
            series_index: Some(2.0),
            ..Default::default()
        };
        let named: Vec<&str> = changed(&fields).iter().map(|(f, _)| *f).collect();
        assert_eq!(named, ["subjects", "series", "series_index"]);
        assert_eq!(changed(&fields)[2].1, "2");
    }

    #[test]
    fn set_with_nothing_named_is_refused() {
        assert!(is_empty(&Book::default()));
        assert!(!is_empty(&Book {
            page_count: Some(1),
            ..Default::default()
        }));
    }
}
