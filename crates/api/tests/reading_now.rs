use std::sync::Arc;

use bookmole::{Engine, EngineConfig};
use bookmole_api::{Api, BookDto, ErrorCode, ProgressDto, Request, Response};

async fn api() -> (Api, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config = EngineConfig {
        db_url: "sqlite::memory:".into(),
        images_dir: tmp.path().join("images"),
        files_dir: tmp.path().join("files"),
        vault_dir: tmp.path().join("vault"),
        log_dir: tmp.path().join("logs"),
        exports_dir: tmp.path().join("exports"),
        google_api_key: None,
        calibre_bin_dir: None,
    };
    let engine = Engine::open(config).await.expect("engine");
    (Api::new(Arc::new(engine)), tmp)
}

async fn seed(api: &Api) -> i64 {
    api.save_book(BookDto {
        title: Some("Station Eleven".into()),
        authors: vec!["Emily St. John Mandel".into()],
        ..Default::default()
    })
    .await
    .expect("save")
    .id
    .expect("saved book has an id")
}

fn book(r: Response) -> BookDto {
    match r {
        Response::Book(Some(b)) => b,
        other => panic!("expected a book, got {other:?}"),
    }
}

#[test]
fn a_finish_without_a_day_parses() {
    let r: Request =
        serde_json::from_str(r#"{"method":"finish_reading","params":{"book_id":3}}"#).unwrap();
    assert_eq!(
        r,
        Request::FinishReading {
            book_id: 3,
            finished_on: None
        }
    );
    let r: Request = serde_json::from_str(
        r#"{"method":"set_page_count","params":{"book_id":3,"page_count":320}}"#,
    )
    .unwrap();
    assert_eq!(
        r,
        Request::SetPageCount {
            book_id: 3,
            page_count: 320
        }
    );
}

#[tokio::test]
async fn finishing_through_dispatch_closes_the_read_and_refuses_a_second_time() {
    let (api, _tmp) = api().await;
    let id = seed(&api).await;
    api.update_progress(id, Some(40), None).await.unwrap();

    let b = book(
        api.dispatch(Request::FinishReading {
            book_id: id,
            finished_on: None,
        })
        .await
        .unwrap(),
    );
    assert!(b.finished);
    assert!(matches!(b.progress, ProgressDto::Finished));

    let again = api
        .dispatch(Request::FinishReading {
            book_id: id,
            finished_on: None,
        })
        .await
        .unwrap_err();
    assert_eq!(again.code, ErrorCode::InvalidInput, "{again:?}");
    assert_eq!(
        api.list_readings(id).await.unwrap().len(),
        1,
        "a refused finish opens nothing"
    );
}

#[tokio::test]
async fn the_page_count_crosses_and_gives_progress_its_length() {
    let (api, _tmp) = api().await;
    let id = seed(&api).await;
    api.update_progress(id, Some(80), None).await.unwrap();

    let typed = api.set_page_count(id, 320).await.unwrap();
    let dispatched = book(
        api.dispatch(Request::SetPageCount {
            book_id: id,
            page_count: 320,
        })
        .await
        .unwrap(),
    );
    assert_eq!(typed.page_count, Some(320));
    assert_eq!(dispatched.page_count, Some(320));
    match dispatched.progress {
        ProgressDto::Started {
            page, of, percent, ..
        } => {
            assert_eq!((page, of, percent), (Some(80), Some(320), Some(25)));
        }
        other => panic!("expected started, got {other:?}"),
    }

    let refused = api
        .dispatch(Request::SetPageCount {
            book_id: id,
            page_count: 0,
        })
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::InvalidInput);
}
