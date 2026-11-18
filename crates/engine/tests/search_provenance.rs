//! Saving a search result records which provider supplied each field; a plain save records nothing.
//! The providers are scripted, so nothing here leaves the machine.

mod common;

use std::collections::BTreeMap;

use bookmole::providers::{MetadataProvider, ProviderBook, ProviderId, SearchRequest};
use bookmole::{Book, EngineError, FieldClaim, RankedResult};

use common::engine;

const ISBN: &str = "9781900000178";

struct Scripted {
    id: ProviderId,
    book: Book,
}

#[async_trait::async_trait]
impl MetadataProvider for Scripted {
    fn id(&self) -> ProviderId {
        self.id
    }
    async fn search(&self, _: &SearchRequest) -> bookmole::Result<Vec<ProviderBook>> {
        Ok(vec![ProviderBook {
            book: self.book.clone(),
            provider: self.id,
            position: 0,
        }])
    }
    async fn by_isbn(&self, isbn: &str) -> bookmole::Result<Option<ProviderBook>> {
        Ok(
            (self.book.isbn_13.as_deref() == Some(isbn)).then(|| ProviderBook {
                book: self.book.clone(),
                provider: self.id,
                position: 0,
            }),
        )
    }
}

/// Two providers describing one edition, each with fields the other lacks and one they both give.
fn providers() -> Vec<Box<dyn MetadataProvider>> {
    let open_library = Book {
        title: Some("Middlemarch".into()),
        authors: vec!["George Eliot".into()],
        isbn_13: Some(ISBN.into()),
        page_count: Some(880),
        publisher: Some("Blackwood".into()),
        ..Default::default()
    };
    let google_books = Book {
        title: Some("Middlemarch".into()),
        authors: vec!["George Eliot".into()],
        isbn_13: Some(ISBN.into()),
        page_count: Some(912),
        language: Some("en".into()),
        description: Some("A study of provincial life.".into()),
        ..Default::default()
    };
    vec![
        Box::new(Scripted {
            id: ProviderId::OpenLibrary,
            book: open_library,
        }),
        Box::new(Scripted {
            id: ProviderId::GoogleBooks,
            book: google_books,
        }),
    ]
}

fn fielded() -> SearchRequest {
    SearchRequest {
        title: Some("Middlemarch".into()),
        ..Default::default()
    }
}

fn free_text() -> SearchRequest {
    SearchRequest {
        query: Some("Middlemarch".into()),
        ..Default::default()
    }
}

async fn found(engine: &bookmole::Engine, req: &SearchRequest) -> RankedResult {
    let mut outcome = engine.search(req).await.expect("search");
    assert_eq!(outcome.results.len(), 1, "{:?}", outcome.results);
    outcome.results.remove(0)
}

async fn provenance(engine: &bookmole::Engine, id: i64) -> BTreeMap<String, String> {
    engine
        .field_provenance(id)
        .await
        .unwrap()
        .into_iter()
        .map(|f| (f.field, f.source))
        .collect()
}

fn claimed(claims: &[FieldClaim]) -> BTreeMap<String, String> {
    claims
        .iter()
        .map(|c| (c.field.clone(), c.provider.to_string()))
        .collect()
}

#[tokio::test]
async fn a_saved_search_result_names_the_provider_of_every_field() {
    for req in [fielded(), free_text()] {
        let (_tmp, engine) = engine().await;
        engine.replace_providers(providers());
        let result = found(&engine, &req).await;

        let by = claimed(&result.claims);
        assert_eq!(by.get("language").map(String::as_str), Some("googlebooks"));
        assert_eq!(by.get("publisher").map(String::as_str), Some("openlibrary"));
        assert!(by.contains_key("page_count") && by.contains_key("title"));

        let saved = engine
            .save_search_result(&result.book, &result.claims)
            .await
            .unwrap();
        let id = saved.id.unwrap();
        assert_eq!(
            provenance(&engine, id).await,
            by,
            "{req:?}: every field the result carried is stamped with the provider the merge took it from"
        );
    }
}

#[tokio::test]
async fn a_plain_save_still_records_nothing() {
    let (_tmp, engine) = engine().await;
    engine.replace_providers(providers());
    let result = found(&engine, &fielded()).await;
    let saved = engine.save_book(&result.book).await.unwrap();
    assert!(provenance(&engine, saved.id.unwrap()).await.is_empty());
}

#[tokio::test]
async fn a_field_the_user_owns_keeps_its_value_and_its_claim() {
    let (_tmp, engine) = engine().await;
    engine.replace_providers(providers());
    let result = found(&engine, &fielded()).await;
    let id = engine
        .save_search_result(&result.book, &result.claims)
        .await
        .unwrap()
        .id
        .unwrap();
    engine.set_page_count(id, 700).await.unwrap();

    let again = engine
        .save_search_result(&result.book, &result.claims)
        .await
        .unwrap();
    assert_eq!(again.id, Some(id), "the ISBN finds the same book");
    assert_eq!(again.page_count, Some(700));
    let now = provenance(&engine, id).await;
    assert_eq!(now.get("page_count").map(String::as_str), Some("user"));
    assert_eq!(now.get("language").map(String::as_str), Some("googlebooks"));
}

#[tokio::test]
async fn a_search_result_put_on_a_list_is_created_with_its_provenance() {
    let (_tmp, engine) = engine().await;
    engine.replace_providers(providers());
    let result = found(&engine, &fielded()).await;
    let list = engine.create_list("Victorians", None).await.unwrap();

    let entry = engine
        .add_search_result_to_list(list.id, &result.book, &result.claims, None)
        .await
        .unwrap();
    assert_eq!(
        provenance(&engine, entry.book.id.unwrap()).await,
        claimed(&result.claims)
    );

    let other = engine.create_list("Doorstops", None).await.unwrap();
    let mut changed = result.claims.clone();
    for c in &mut changed {
        c.provider = ProviderId::OpenLibrary;
    }
    let again = engine
        .add_search_result_to_list(other.id, &result.book, &changed, None)
        .await
        .unwrap();
    assert_eq!(again.book.id, entry.book.id, "a known book is reused");
    assert_eq!(
        provenance(&engine, entry.book.id.unwrap()).await,
        claimed(&result.claims),
        "and not written to"
    );
}

#[tokio::test]
async fn a_claim_naming_no_book_field_is_refused() {
    let (_tmp, engine) = engine().await;
    let book = common::book("Middlemarch");
    for claims in [
        vec![FieldClaim {
            field: "favourite_colour".into(),
            provider: ProviderId::OpenLibrary,
        }],
        vec![
            FieldClaim {
                field: "title".into(),
                provider: ProviderId::OpenLibrary,
            },
            FieldClaim {
                field: "title".into(),
                provider: ProviderId::GoogleBooks,
            },
        ],
    ] {
        assert!(matches!(
            engine.save_search_result(&book, &claims).await,
            Err(EngineError::InvalidInput(_))
        ));
    }
    assert!(
        engine
            .resolve_books("Middlemarch")
            .await
            .unwrap()
            .is_empty(),
        "a refused save writes nothing"
    );
}

#[tokio::test]
async fn a_claim_on_a_column_no_provider_supplies_is_refused() {
    let (_tmp, engine) = engine().await;
    let book = Book {
        cover_path: Some("database/images/middlemarch.jpg".into()),
        ..common::book("Middlemarch")
    };
    let claims = [FieldClaim {
        field: "cover_path".into(),
        provider: ProviderId::OpenLibrary,
    }];
    assert!(matches!(
        engine.save_search_result(&book, &claims).await,
        Err(EngineError::InvalidInput(_))
    ));
    let list = engine.create_list("Victorians", None).await.unwrap();
    assert!(matches!(
        engine
            .add_search_result_to_list(list.id, &book, &claims, None)
            .await,
        Err(EngineError::InvalidInput(_))
    ));
}

#[tokio::test]
async fn a_book_saved_from_an_isbn_lookup_names_the_provider_of_every_field() {
    let (_tmp, engine) = engine().await;
    engine.replace_providers(providers());
    let mut outcome = engine
        .lookup_isbn_result("978-1-900000-17-8")
        .await
        .unwrap();
    assert_eq!(outcome.results.len(), 1, "{:?}", outcome.results);
    let result = outcome.results.remove(0);
    assert_eq!(
        format!("{:?}", Some(&result.book)),
        format!("{:?}", engine.lookup_isbn(ISBN).await.unwrap().as_ref()),
        "the result is the book the plain lookup answers"
    );
    assert_eq!(result.shelf_id, None);

    let by = claimed(&result.claims);
    assert_eq!(by.get("language").map(String::as_str), Some("googlebooks"));
    assert_eq!(
        by.get("description").map(String::as_str),
        Some("googlebooks")
    );
    assert_eq!(by.get("publisher").map(String::as_str), Some("openlibrary"));
    assert!(by.contains_key("page_count") && by.contains_key("title"));

    let saved = engine
        .save_search_result(&result.book, &result.claims)
        .await
        .unwrap();
    let id = saved.id.unwrap();
    assert_eq!(provenance(&engine, id).await, by);

    let again = engine.lookup_isbn_result(ISBN).await.unwrap();
    assert_eq!(again.results[0].shelf_id, Some(id), "now on the shelf");
}

#[tokio::test]
async fn an_isbn_no_provider_knows_is_an_empty_answer() {
    let (_tmp, engine) = engine().await;
    engine.replace_providers(providers());
    let outcome = engine.lookup_isbn_result("9780141439549").await.unwrap();
    assert!(outcome.results.is_empty() && outcome.warnings.is_empty());
    assert!(matches!(
        engine.lookup_isbn_result("not an isbn").await,
        Err(EngineError::InvalidIsbn(_))
    ));
}

#[tokio::test]
async fn an_unrecorded_save_of_a_result_keeps_its_provenance_and_its_copy() {
    let (_tmp, engine) = engine().await;
    engine.replace_providers(providers());
    let result = engine
        .lookup_isbn_result(ISBN)
        .await
        .unwrap()
        .results
        .remove(0);
    let saved = engine
        .save_search_result_unrecorded(&result.book, &result.claims)
        .await
        .unwrap();
    let id = saved.id.unwrap();
    assert_eq!(provenance(&engine, id).await, claimed(&result.claims));
    assert_eq!(engine.copies_for_book(id).await.unwrap().len(), 1);

    engine
        .save_search_result_unrecorded(&result.book, &result.claims)
        .await
        .unwrap();
    assert_eq!(
        engine.copies_for_book(id).await.unwrap().len(),
        1,
        "saving a known book again adds no copy"
    );
}
