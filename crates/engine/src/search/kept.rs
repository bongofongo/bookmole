//! Reading kept records back into results, and the one record the
//! engine writes in its own words.
//!
//! A record kept as `doc` is the provider's own JSON and is read by that
//! provider's module (`openlibrary::decode_doc`), so a parser that improves
//! improves every kept record with it. A record kept as `book` is an answer
//! that took several requests to assemble — Open Library's ISBN lookup is the
//! edition, then its authors, then its work — where there is no one response to
//! keep; it is kept as [`KeptBook`], the engine's own reading of it.

use serde::{Deserialize, Serialize};

use crate::book::Book;
use crate::error::{EngineError, Result};
use crate::providers::{
    KeptRecord, ProviderBook, ProviderId, ProviderWork, RecordForm, RecordKind, openlibrary,
};
use crate::storage::KeptRow;

/// The fields of a [`Book`] a provider supplies, as kept for an ISBN lookup.
///
/// **Built and read by exhaustive destructuring**, never by `..`: a field added
/// to `Book` fails to compile here until somebody decides whether a provider
/// supplies it, which is the only way a second list of columns stays the same
/// list as the first.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct KeptBook {
    title: Option<String>,
    authors: Vec<String>,
    translators: Vec<String>,
    publisher: Option<String>,
    publish_year: Option<i64>,
    first_published: Option<i64>,
    language: Option<String>,
    isbn_10: Option<String>,
    isbn_13: Option<String>,
    openlibrary_key: Option<String>,
    googlebooks_id: Option<String>,
    cover_url: Option<String>,
    page_count: Option<i64>,
    description: Option<String>,
    first_sentence: Option<String>,
    subjects: Vec<String>,
    series: Option<String>,
    series_index: Option<f64>,
}

impl From<&Book> for KeptBook {
    fn from(b: &Book) -> Self {
        let Book {
            // What a provider says.
            title,
            authors,
            translators,
            publisher,
            publish_year,
            first_published,
            language,
            isbn_10,
            isbn_13,
            openlibrary_key,
            googlebooks_id,
            cover_url,
            page_count,
            description,
            first_sentence,
            subjects,
            series,
            series_index,
            // Ours: the row, its filing key, the stored jacket and what was
            // measured of it, and the reading state. No provider says these.
            id: _,
            sort_title: _,
            cover_path: _,
            cover_width: _,
            cover_height: _,
            cover_accent: _,
            cover_thumb_path: _,
            current_page: _,
            finished: _,
            reading_status: _,
            ko_percent: _,
            date_started: _,
            date_finished: _,
            created_at: _,
            last_modified: _,
        } = b.clone();
        KeptBook {
            title,
            authors,
            translators,
            publisher,
            publish_year,
            first_published,
            language,
            isbn_10,
            isbn_13,
            openlibrary_key,
            googlebooks_id,
            cover_url,
            page_count,
            description,
            first_sentence,
            subjects,
            series,
            series_index,
        }
    }
}

impl From<KeptBook> for Book {
    fn from(k: KeptBook) -> Self {
        let KeptBook {
            title,
            authors,
            translators,
            publisher,
            publish_year,
            first_published,
            language,
            isbn_10,
            isbn_13,
            openlibrary_key,
            googlebooks_id,
            cover_url,
            page_count,
            description,
            first_sentence,
            subjects,
            series,
            series_index,
        } = k;
        Book {
            title,
            authors,
            translators,
            publisher,
            publish_year,
            first_published,
            language,
            isbn_10,
            isbn_13,
            openlibrary_key,
            googlebooks_id,
            cover_url,
            page_count,
            description,
            first_sentence,
            subjects,
            series,
            series_index,
            ..Default::default()
        }
    }
}

/// The key an edition looked up by ISBN is kept under: the canonical ISBN-13,
/// so the same edition typed as its ISBN-10 is the same record.
pub(crate) fn isbn_key(book: &Book) -> Option<String> {
    book.canonical_isbn13().map(|i| format!("isbn:{i}"))
}

/// An ISBN lookup's answer, as the one record it is kept as.
pub(crate) fn book_record(book: &Book) -> Option<KeptRecord> {
    Some(KeptRecord {
        key: isbn_key(book)?,
        kind: RecordKind::Edition,
        form: RecordForm::Book,
        title: book.title.clone(),
        authors: book.authors.clone(),
        body: serde_json::to_string(&KeptBook::from(book)).ok()?,
    })
}

/// Kept rows back into the result they were. `Err` names a body that no
/// longer reads (a provider changed its shape, or a parser grew stricter);
/// the caller drops it and asks the provider again rather than failing.
pub(crate) fn decode(
    provider: ProviderId,
    rows: &[KeptRow],
    position: usize,
) -> Result<ProviderWork> {
    let unreadable = |why: &str| EngineError::Provider {
        provider,
        message: format!("a kept record {why}"),
    };
    let Some(first) = rows.first() else {
        return Err(unreadable("is missing"));
    };
    match first.form {
        RecordForm::Book => {
            let kept: KeptBook = serde_json::from_str(&first.body)?;
            Ok(ProviderWork::bare(ProviderBook {
                book: kept.into(),
                provider,
                position,
            }))
        }
        RecordForm::Doc => match provider {
            ProviderId::OpenLibrary => {
                let work = rows
                    .iter()
                    .find(|r| r.kind == RecordKind::Work)
                    .ok_or_else(|| unreadable("names no work"))?;
                let edition = rows.iter().find(|r| r.kind == RecordKind::Edition);
                openlibrary::decode_doc(&work.body, edition.map(|e| e.body.as_str()), position)
            }
            // Never kept, so never read back.
            ProviderId::GoogleBooks => Err(unreadable("is from a provider that is never kept")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything a provider supplies survives the round trip; nothing of ours
    /// is carried.
    #[test]
    fn a_kept_book_round_trips_what_a_provider_supplies() {
        let b = Book {
            id: Some(7),
            title: Some("Dune".into()),
            authors: vec!["Frank Herbert".into()],
            translators: vec!["T".into()],
            publisher: Some("Ace".into()),
            publish_year: Some(1990),
            first_published: Some(1965),
            language: Some("en".into()),
            isbn_10: Some("0441172717".into()),
            isbn_13: Some("9780441172719".into()),
            openlibrary_key: Some("/books/OL1M".into()),
            googlebooks_id: Some("g".into()),
            cover_url: Some("https://x/c.jpg".into()),
            cover_path: Some("/tmp/c.jpg".into()),
            page_count: Some(535),
            description: Some("d".into()),
            first_sentence: Some("s".into()),
            subjects: vec!["Fiction".into()],
            series: Some("Dune".into()),
            series_index: Some(1.0),
            current_page: Some(4),
            ..Default::default()
        };
        let rec = book_record(&b).unwrap();
        assert_eq!(rec.key, "isbn:9780441172719");
        let row = KeptRow {
            key: rec.key.clone(),
            kind: rec.kind,
            form: rec.form,
            body: rec.body.clone(),
            fetched_at: 0,
        };
        let back = decode(ProviderId::OpenLibrary, &[row], 2).unwrap();
        assert_eq!(back.found.position, 2);
        let mut want = b.clone();
        want.id = None;
        want.cover_path = None;
        want.current_page = None;
        assert_eq!(format!("{:?}", back.found.book), format!("{want:?}"));
        assert!(!rec.body.contains("/tmp/c.jpg"));
    }

    #[test]
    fn a_body_that_no_longer_reads_is_an_error() {
        let row = KeptRow {
            key: "isbn:9780441172719".into(),
            kind: RecordKind::Edition,
            form: RecordForm::Book,
            body: "{\"title\": 7}".into(),
            fetched_at: 0,
        };
        assert!(decode(ProviderId::OpenLibrary, &[row], 0).is_err());
        assert!(decode(ProviderId::OpenLibrary, &[], 0).is_err());
    }
}
