//! Sync across the seam: two libraries, each its own data directory, syncing through a server on
//! a loopback port, driven by typed methods and by dispatch as a frontend would.

use bookmole::test_server::TestServer;
use bookmole_api::{
    Api, ApiError, BookDto, BookQueryDto, Call, CopyFormDto, EntryShowDto, EntrySortDto, ErrorCode,
    NewCopyDto, NewNoteDto, Outcome, RecoveryCodeDto, Reply, Request, Response, SyncStatusDto,
    TenureDto,
};

struct Device {
    api: Api,
    _dir: tempfile::TempDir,
}

async fn device() -> Device {
    let dir = tempfile::tempdir().unwrap();
    let api = Api::open(dir.path()).await.unwrap();
    Device { api, _dir: dir }
}

/// A call as a transport carries it: one line of JSON each way.
async fn wire(d: &Device, request: Request) -> Result<Response, ApiError> {
    let line = serde_json::to_string(&Call { id: 1, request }).unwrap();
    let reply = d.api.call(serde_json::from_str(&line).unwrap()).await;
    let reply: Reply = serde_json::from_str(reply.to_line().trim_end()).unwrap();
    match reply.outcome {
        Outcome::Ok { response } => Ok(response),
        Outcome::Error { error } => Err(error),
    }
}

/// A book with a copy of it, so the library's listing shows it.
async fn save(d: &Device, title: &str, isbn_13: &str) -> i64 {
    let id = d
        .api
        .save_book(BookDto {
            title: Some(title.into()),
            authors: vec!["George Eliot".into()],
            isbn_13: Some(isbn_13.into()),
            ..Default::default()
        })
        .await
        .unwrap()
        .id
        .unwrap();
    d.api
        .add_copy(NewCopyDto {
            book_id: id,
            form: Some(CopyFormDto::Print),
            tenure: TenureDto::Own,
            have_now: None,
            from_whom: None,
            app: None,
            location: None,
            started_at: None,
            due_at: None,
            note: None,
        })
        .await
        .unwrap();
    id
}

async fn titles(d: &Device) -> Vec<String> {
    let books = d
        .api
        .list_books(BookQueryDto {
            sort: Default::default(),
            filter: None,
            limit: 100,
            offset: 0,
        })
        .await
        .unwrap();
    let mut titles: Vec<String> = books.into_iter().filter_map(|b| b.title).collect();
    titles.sort();
    titles
}

async fn page_count(d: &Device, title: &str) -> Option<i64> {
    let found = d.api.resolve_books(title).await.unwrap();
    assert_eq!(found.len(), 1, "{title}: {found:?}");
    found[0].page_count
}

async fn converge(devices: &[&Device]) {
    let held = converge_holding(devices).await;
    assert!(held.is_empty(), "held: {held:?}");
}

/// Every device syncs in turn until a whole round moves nothing; what the last round held.
async fn converge_holding(devices: &[&Device]) -> Vec<String> {
    for _ in 0..6 {
        let (mut moved, mut held) = (false, Vec::new());
        for d in devices {
            let outcome = d.api.sync_now().await.unwrap();
            held.extend(outcome.held.into_iter().map(|row| row.collection));
            moved |= outcome.sent || outcome.received;
        }
        if !moved {
            return held;
        }
    }
    panic!("the devices kept sending changes");
}

fn code_of(result: Result<Response, ApiError>) -> ErrorCode {
    result.unwrap_err().code
}

fn recovery_code(result: Result<Response, ApiError>) -> RecoveryCodeDto {
    match result.unwrap() {
        Response::RecoveryCode(code) => code,
        other => panic!("{other:?}"),
    }
}

async fn set_pages(d: &Device, title: &str, pages: i64) {
    let id = d.api.resolve_books(title).await.unwrap()[0].id.unwrap();
    d.api
        .set_book_fields(
            id,
            BookDto {
                page_count: Some(pages),
                ..Default::default()
            },
        )
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_libraries_sync_through_the_api_and_one_leaves_and_joins_again() {
    let server = TestServer::default();
    let http = server.serve().unwrap();
    let url = http.url();
    let (a, b) = (device().await, device().await);
    save(&a, "Middlemarch", "9780141439549").await;
    save(&b, "Romola", "9780140434705").await;

    assert_eq!(a.api.sync_status().await.unwrap(), SyncStatusDto::Off);
    assert_eq!(
        code_of(wire(&a, Request::SyncNow).await),
        ErrorCode::SyncOff
    );
    let code = recovery_code(
        wire(
            &a,
            Request::CreateSyncLibrary {
                server: format!("{url}/"),
                device_label: "Study".into(),
            },
        )
        .await,
    );
    let SyncStatusDto::On {
        server: origin,
        device_label,
        last_problem,
        ..
    } = a.api.sync_status().await.unwrap()
    else {
        panic!("not on");
    };
    assert_eq!(
        (origin.as_str(), device_label.as_str()),
        (url.as_str(), "Study")
    );
    assert_eq!(last_problem, None);
    assert_eq!(
        code_of(
            wire(
                &a,
                Request::CreateSyncLibrary {
                    server: url.clone(),
                    device_label: "Study".into(),
                },
            )
            .await
        ),
        ErrorCode::SyncAlreadyOn
    );

    let joined = wire(
        &b,
        Request::JoinSyncLibrary {
            server: url.clone(),
            recovery_code: code.clone(),
            device_label: "Reading chair".into(),
        },
    )
    .await;
    assert_eq!(joined.unwrap(), Response::Unit);
    converge(&[&a, &b]).await;
    for d in [&a, &b] {
        assert_eq!(titles(d).await, ["Middlemarch", "Romola"]);
    }

    set_pages(&a, "Middlemarch", 880).await;
    set_pages(&b, "Romola", 592).await;
    converge(&[&a, &b]).await;
    for d in [&a, &b] {
        assert_eq!(page_count(d, "Middlemarch").await, Some(880));
        assert_eq!(page_count(d, "Romola").await, Some(592));
    }

    let requests = server.requests();
    assert_eq!(
        wire(&b, Request::LeaveSyncLibrary).await.unwrap(),
        Response::Unit
    );
    assert_eq!(server.requests(), requests);
    assert_eq!(
        wire(&b, Request::SyncStatus).await.unwrap(),
        Response::SyncStatus(SyncStatusDto::Off)
    );
    assert_eq!(titles(&b).await, ["Middlemarch", "Romola"]);
    for refused in [Request::SyncNow, Request::SyncRecoveryCode] {
        assert_eq!(code_of(wire(&b, refused).await), ErrorCode::SyncOff);
    }
    assert_eq!(
        wire(&b, Request::LeaveSyncLibrary).await.unwrap(),
        Response::Unit
    );

    save(&a, "Silas Marner", "9780141439754").await;
    converge(&[&a]).await;
    assert_eq!(titles(&b).await, ["Middlemarch", "Romola"]);

    let again = recovery_code(wire(&a, Request::SyncRecoveryCode).await);
    assert_eq!(again, code);
    b.api
        .join_sync_library(&url, &again, "Reading chair")
        .await
        .unwrap();
    converge(&[&a, &b]).await;
    for d in [&a, &b] {
        assert_eq!(titles(d).await, ["Middlemarch", "Romola", "Silas Marner"]);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dispatch_and_the_typed_method_agree_on_sync_status() {
    let server = TestServer::default();
    let http = server.serve().unwrap();
    let a = device().await;
    a.api
        .create_sync_library(&http.url(), "Study")
        .await
        .unwrap();
    a.api.sync_now().await.unwrap();
    let typed = a.api.sync_status().await.unwrap();
    assert!(matches!(typed, SyncStatusDto::On { log_position, .. } if log_position > 0));
    assert_eq!(
        a.api.dispatch(Request::SyncStatus).await.unwrap(),
        Response::SyncStatus(typed)
    );
}

/// The code crosses the wire where it must, and no `Debug` of a request, a response or a reply
/// that carries it shows it, nor any error about it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recovery_code_is_on_the_wire_only_where_it_must_be_and_never_in_debug() {
    let server = TestServer::default();
    let http = server.serve().unwrap();
    let (a, b) = (device().await, device().await);
    let code = a
        .api
        .create_sync_library(&http.url(), "Study")
        .await
        .unwrap();
    let secret = code.expose().to_owned();
    let groups: Vec<&str> = secret.split('-').collect();
    assert!(groups.len() > 5, "{secret}");
    let shown = |text: &str| text.contains(&secret) || groups.iter().any(|g| text.contains(g));

    let join = Request::JoinSyncLibrary {
        server: http.url(),
        recovery_code: code.clone(),
        device_label: "Reading chair".into(),
    };
    let call = Call {
        id: 3,
        request: join.clone(),
    };
    let answer = Reply::ok(4, Response::RecoveryCode(code.clone()));
    for debug in [
        format!("{code:?}"),
        format!("{join:?}"),
        format!("{call:?}"),
        format!("{answer:?}"),
        format!("{:?}", a.api.sync_status().await.unwrap()),
    ] {
        assert!(!shown(&debug), "{debug}");
    }
    assert!(serde_json::to_string(&call).unwrap().contains(&secret));
    assert!(answer.to_line().contains(&secret));
    assert_eq!(
        serde_json::from_str::<RecoveryCodeDto>(&format!("\"{secret}\"")).unwrap(),
        code
    );

    let mistyped = RecoveryCodeDto::new(secret.replacen(groups[1], "UUUUU", 1));
    let refused = b
        .api
        .join_sync_library(&http.url(), &mistyped, "Reading chair")
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::SyncCodeInvalid);
    assert!(!shown(&refused.message), "{}", refused.message);
    assert!(!shown(&format!("{refused:?}")));
    assert_eq!(b.api.sync_status().await.unwrap(), SyncStatusDto::Off);
}

/// Each failure a reader can act on arrives as its own code, and no message names the server's
/// address, a library or a device.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sync_failure_arrives_as_a_code_a_frontend_can_act_on() {
    let a = device().await;

    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let nowhere = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    let unreachable = a
        .api
        .create_sync_library(&nowhere, "Study")
        .await
        .unwrap_err();
    assert_eq!(unreachable.code, ErrorCode::SyncUnreachable);
    assert!(
        !unreachable.message.contains("127.0.0.1"),
        "{unreachable:?}"
    );

    let with_password = "https://reader:hunter2@sync.example.test";
    let refused = a
        .api
        .create_sync_library(with_password, "Study")
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::InvalidInput);
    assert!(!refused.message.contains("hunter2"), "{refused:?}");

    let elsewhere = TestServer::default();
    let elsewhere = elsewhere.serve().unwrap();
    let here = TestServer::default();
    let here = here.serve().unwrap();
    let b = device().await;
    let code = b
        .api
        .create_sync_library(&elsewhere.url(), "Study")
        .await
        .unwrap();
    let SyncStatusDto::On { library_id, .. } = b.api.sync_status().await.unwrap() else {
        panic!("not on");
    };
    let unknown = a
        .api
        .join_sync_library(&here.url(), &code, "Reading chair")
        .await
        .unwrap_err();
    assert_eq!(unknown.code, ErrorCode::SyncRefused);
    for named in [library_id.as_str(), code.expose(), "127.0.0.1"] {
        assert!(!unknown.message.contains(named), "{unknown:?}");
    }
    assert_eq!(a.api.sync_status().await.unwrap(), SyncStatusDto::Off);

    drop(elsewhere);
    let gone = b.api.sync_now().await.unwrap_err();
    assert_eq!(gone.code, ErrorCode::SyncUnreachable);
    let SyncStatusDto::On { last_problem, .. } = b.api.sync_status().await.unwrap() else {
        panic!("not on");
    };
    assert_eq!(
        last_problem.map(|p| p.code),
        Some(ErrorCode::SyncUnreachable)
    );

    b.api.leave_sync_library().await.unwrap();
    b.api
        .create_sync_library(&here.url(), "Study")
        .await
        .unwrap();
    let SyncStatusDto::On { last_problem, .. } = b.api.sync_status().await.unwrap() else {
        panic!("not on");
    };
    assert_eq!(last_problem, None, "a problem outlived leaving");
}

/// What one library holds, by kind of row.
#[derive(Debug, PartialEq, Eq)]
struct Counts {
    books: usize,
    copies: usize,
    readings: usize,
    highlights: usize,
    lists: usize,
    entries: usize,
    notes: usize,
    scales: usize,
}

async fn counts(d: &Device) -> Counts {
    let books = d.api.resolve_books("").await.unwrap();
    let (mut copies, mut readings, mut highlights) = (0, 0, 0);
    for book in &books {
        let id = book.id.unwrap();
        copies += d.api.copies_for_book(id).await.unwrap().len();
        readings += d.api.list_readings(id).await.unwrap().len();
        highlights += d.api.list_highlights(id).await.unwrap().len();
    }
    let lists = d.api.list_lists(Default::default(), 0).await.unwrap();
    let mut entries = 0;
    for list in &lists {
        entries += d
            .api
            .list_entries(list.id, EntryShowDto::All, EntrySortDto::Yours)
            .await
            .unwrap()
            .len();
    }
    Counts {
        books: books.len(),
        copies,
        readings,
        highlights,
        lists: lists.len(),
        entries,
        notes: d.api.list_notes(None, None, None).await.unwrap().len(),
        scales: d.api.rating_scales().await.unwrap().len(),
    }
}

/// Recorded behaviour, not wanted behaviour: a device that leaves and joins the same library
/// again is a new device whose rows the others have never seen, so every row with no natural
/// identity arrives twice, on every device. Books with an ISBN, their highlights, notes and
/// rating scales match up. A fix flips these counts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn known_rejoining_the_same_library_duplicates_rows_without_identity() {
    let server = TestServer::default();
    let http = server.serve().unwrap();
    let (a, b) = (device().await, device().await);
    let middlemarch = save(&a, "Middlemarch", "9780141439549").await;
    let felix = a
        .api
        .save_book(BookDto {
            title: Some("Felix Holt".into()),
            authors: vec!["George Eliot".into()],
            ..Default::default()
        })
        .await
        .unwrap()
        .id
        .unwrap();
    for (book, text) in [
        (
            middlemarch,
            "Who can quit young lives after being long in company?",
        ),
        (
            felix,
            "There is no private life which has not been determined.",
        ),
    ] {
        a.api
            .create_highlight(book, text, None, None)
            .await
            .unwrap();
    }
    a.api
        .update_progress(middlemarch, Some(40), None)
        .await
        .unwrap();
    let list = a.api.create_list("To read", None).await.unwrap();
    a.api.add_to_list(list.id, middlemarch, None).await.unwrap();
    a.api.add_to_list(list.id, felix, None).await.unwrap();
    a.api
        .create_note(NewNoteDto {
            book_id: Some(middlemarch),
            body: "Dorothea and the key to all mythologies.".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let romola = save(&b, "Romola", "9780140434705").await;
    b.api.update_progress(romola, Some(12), None).await.unwrap();

    let code = a
        .api
        .create_sync_library(&http.url(), "Study")
        .await
        .unwrap();
    b.api
        .join_sync_library(&http.url(), &code, "Reading chair")
        .await
        .unwrap();
    converge(&[&a, &b]).await;
    let before = counts(&a).await;
    assert_eq!(counts(&b).await, before);

    b.api.leave_sync_library().await.unwrap();
    b.api
        .join_sync_library(&http.url(), &code, "Reading chair")
        .await
        .unwrap();
    converge(&[&a, &b]).await;
    let after = counts(&a).await;
    assert_eq!(counts(&b).await, after);
    assert_eq!(
        before,
        Counts {
            books: 3,
            copies: 2,
            readings: 2,
            highlights: 2,
            lists: 1,
            entries: 2,
            notes: 1,
            scales: 2,
        }
    );
    assert_eq!(
        after,
        Counts {
            books: 4,
            copies: 4,
            readings: 4,
            highlights: 3,
            lists: 2,
            entries: 4,
            notes: 1,
            scales: 2,
        }
    );
}
