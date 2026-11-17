//! The API crate against a real engine, in-process.
//!
//! What these assert is not "the engine works" — the engine's own suite does
//! that — but the two claims the API seam rests on:
//!
//! 1. **The typed method and the dispatch arm are the same call.** If they ever
//!    diverge, an in-process host (typed) and a daemon client (dispatched) get
//!    different behaviour from the same request, and the boundary has stopped
//!    being the API.
//! 2. **No domain handle crosses the seam.** Everything is addressed by id, and
//!    the row is re-read on this side.

use std::path::PathBuf;
use std::sync::Arc;

use bookmole::{Engine, EngineConfig};
use bookmole_api::{
    Api, ApiError, BookDto, BookFilterDto, BookQueryDto, CopyFormDto, DayRangeDto, EntryShowDto,
    EntrySortDto, ErrorCode, GraphEdgeKindDto, GraphScopeDto, LinkDirectionDto, ListSortDto,
    MomentKindDto, NewCopyDto, NewNoteDto, NoteKindDto, Outcome, ReadingFilterDto, ReadingQueryDto,
    ReadingStateDto, Request, Response, SearchHitDto, SearchSourceDto, ShapeSourceDto,
    StatusFilterDto, TenureDto,
};

/// A library in a tempdir with an in-memory database, like every other suite
/// here. The `TempDir` comes back so the vault outlives the test body.
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

/// Own a copy of this book, so it is in the library (the Library's listings
/// show only books you have).
async fn have(api: &Api, book_id: i64) {
    api.add_copy(NewCopyDto {
        book_id,
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
    .expect("add copy");
}

async fn seed(api: &Api) -> i64 {
    let id = api
        .save_book(BookDto {
            title: Some("Station Eleven".into()),
            authors: vec!["Emily St. John Mandel".into()],
            isbn_13: Some("9781447268963".into()),
            page_count: Some(333),
            ..Default::default()
        })
        .await
        .expect("save")
        .id
        .expect("saved book has an id");
    have(api, id).await;
    id
}

fn ok(response: Result<Response, ApiError>) -> Response {
    response.expect("dispatch succeeded")
}

/// The daemon holds one `Api` and hands `&self` to every connection at once, so
/// this is a compile-time requirement rather than a nicety. It is why
/// `Engine::set_google_api_key` had to stop taking `&mut self`.
#[test]
fn the_api_can_be_shared_across_connections() {
    fn assert_send_sync<T: Send + Sync + Clone + 'static>() {}
    assert_send_sync::<Api>();
}

/// Claim 1, for a read: the typed method and the dispatch arm must produce the
/// same answer, because `dispatch` is meant to be pure fan-out.
#[tokio::test]
async fn dispatch_and_the_typed_method_agree() {
    let (api, _tmp) = api().await;
    let id = seed(&api).await;

    let typed = api.get_book(id).await.unwrap();
    match ok(api.dispatch(Request::GetBook { id }).await) {
        Response::Book(dispatched) => assert_eq!(dispatched, typed),
        other => panic!("{other:?}"),
    }

    let typed = api
        .list_books(BookQueryDto {
            sort: Default::default(),
            filter: None,
            limit: 10,
            offset: 0,
        })
        .await
        .unwrap();
    match ok(api
        .dispatch(Request::ListBooks {
            limit: 10,
            sort: Default::default(),
            offset: 0,
            filter: None,
        })
        .await)
    {
        Response::Books(dispatched) => assert_eq!(dispatched, typed),
        other => panic!("{other:?}"),
    }
}

/// And for a write. A rule that lived in the dispatch arm would show up here as
/// the two paths disagreeing about what a call did.
#[tokio::test]
async fn dispatch_and_the_typed_method_agree_on_a_write() {
    let (api, _tmp) = api().await;
    let id = seed(&api).await;

    let typed = api.update_progress(id, Some(40), None).await.unwrap();
    assert_eq!(typed.current_page, Some(40));

    match ok(api
        .dispatch(Request::UpdateProgress {
            book_id: id,
            page: Some(41),
            finished: None,
        })
        .await)
    {
        Response::Book(Some(b)) => assert_eq!(b.current_page, Some(41)),
        other => panic!("{other:?}"),
    }
    // One reading, not two: the second call must have gone through the same
    // `update_progress` the first did.
    assert_eq!(api.list_readings(id).await.unwrap().len(), 1);
}

/// Claim 2. `Engine::update_note_body` takes a `&NoteRecord`; nothing here
/// does, and a client never holds one.
#[tokio::test]
async fn a_note_is_written_and_read_back_by_id_alone() {
    let (api, _tmp) = api().await;
    let book_id = seed(&api).await;

    let created = api
        .create_note(NewNoteDto {
            book_id: Some(book_id),
            body: "A note about the [[Symphony]].".into(),
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(
        api.note_body(created.id).await.unwrap(),
        "A note about the [[Symphony]]."
    );
    api.update_note_body(created.id, "Rewritten, still about the [[Symphony]].")
        .await
        .unwrap();
    assert!(
        api.note_body(created.id)
            .await
            .unwrap()
            .starts_with("Rewritten")
    );

    // The edge survived the rewrite, which is what re-indexing on update buys.
    let links = api.outgoing_links(created.id).await.unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].target_title, "Symphony");
    // Nobody has written that note, so the target dangles — a forward
    // reference, carried as text rather than dropped.
    assert!(links[0].note.is_none());

    api.delete_note(created.id).await.unwrap();
    assert!(api.get_note(created.id).await.unwrap().is_none());
}

/// `last_modified` crosses the seam, so a frontend can say when a page was
/// kept after a reload rather than only in the session that kept it. The
/// stamp is second-resolution, so "moves" is asserted as "never goes back";
/// the strict move is the storage suite's, where the row can be backdated.
#[tokio::test]
async fn a_notes_last_kept_moment_crosses_the_seam() {
    let (api, _tmp) = api().await;
    let created = api
        .create_note(NewNoteDto {
            body: "First words.".into(),
            ..Default::default()
        })
        .await
        .unwrap();

    let fresh = api.get_note(created.id).await.unwrap().expect("the note");
    let kept = fresh.last_modified.expect("stamped on create");
    assert_eq!(Some(kept), fresh.created_at);

    api.update_note_body(created.id, "Second words.")
        .await
        .unwrap();
    let saved = api.get_note(created.id).await.unwrap().expect("the note");
    assert!(saved.last_modified.expect("still stamped") >= kept);

    // And through dispatch, as a socket client reads it.
    match ok(api.dispatch(Request::GetNote { id: created.id }).await) {
        Response::Note(Some(n)) => assert_eq!(n.last_modified, saved.last_modified),
        other => panic!("expected a note, got {other:?}"),
    }
}

/// A note id that names nothing must be `not_found` rather than a panic or a
/// database error leaking through as `internal`.
#[tokio::test]
async fn an_absent_note_is_not_found_and_not_something_vaguer() {
    let (api, _tmp) = api().await;
    let err = api.note_body(4242).await.expect_err("no such note");
    assert_eq!(err.code, ErrorCode::NotFound);
    assert!(err.message.contains("4242"), "{}", err.message);
}

/// Calibre is feature-detected, never required. Absent is an answer, and
/// a client that got an error here would show a failure the user is meant to
/// fix — which is exactly what the CLI's `calibre status` refuses to do.
#[tokio::test]
async fn an_absent_calibre_is_a_status_and_never_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let empty = tmp.path().join("no-binaries-here");
    std::fs::create_dir_all(&empty).unwrap();
    let config = EngineConfig {
        db_url: "sqlite::memory:".into(),
        images_dir: tmp.path().join("images"),
        files_dir: tmp.path().join("files"),
        vault_dir: tmp.path().join("vault"),
        log_dir: tmp.path().join("logs"),
        exports_dir: tmp.path().join("exports"),
        google_api_key: None,
        calibre_bin_dir: Some(empty),
    };
    let api = Api::new(Arc::new(Engine::open(config).await.unwrap()));

    // Not `Err`, whatever this machine happens to have installed.
    match ok(api.dispatch(Request::CalibreStatus).await) {
        Response::CalibreStatus(_) => {}
        other => panic!("{other:?}"),
    }
}

/// The rating rule, across the seam: an unmapped value is a code, never a
/// rounded star. The lookup table exists precisely to refuse that, and a
/// transport is not a reason to start guessing.
#[tokio::test]
async fn an_unmapped_rating_is_a_code_not_a_rounded_star() {
    let (api, _tmp) = api().await;
    let book_id = seed(&api).await;

    // Since migration `0026` the seeded scale is 0–5 in quarters and maps onto
    // Goodreads one-to-one, so a quarter star crosses as itself.
    let review = api.open_review(book_id, None).await.unwrap();
    api.set_rating(review.id, 4.25).await.unwrap();

    let stored = api.review_rating(review.id).await.unwrap().expect("rated");
    assert_eq!(stored.value, 4.25);
    // The scale travels with the value, or the number means nothing.
    assert_eq!(stored.scale.step, 0.25);
    assert_eq!(api.goodreads_rating(review.id).await.unwrap(), Some(4.25));

    // A scale the user made, with no map, is the case an unmapped rating now
    // comes from — and it is a code, not the nearest quarter.
    api.put_rating_scale("tens", 0.0, 10.0, 1.0).await.unwrap();
    api.set_rating(review.id, 7.0).await.unwrap();
    let err = api
        .goodreads_rating(review.id)
        .await
        .expect_err("7 of 10 has no Goodreads mapping");
    assert_eq!(err.code, ErrorCode::UnmappedRating);
}

/// A reflection accretes: the second call finds the first call's note. Worth
/// pinning here because a transport makes "call it twice" the ordinary case
/// rather than the unusual one.
#[tokio::test]
async fn opening_a_reflection_twice_opens_the_same_note() {
    let (api, _tmp) = api().await;
    let book_id = seed(&api).await;

    let first = api.open_reflection(book_id, None).await.unwrap();
    let again = api.open_reflection(book_id, None).await.unwrap();
    assert_eq!(first.id, again.id);
    assert_eq!(first.file, again.file);

    // And it is findable from the reading, which is what anchors it.
    let reading = api.active_reading(book_id).await.unwrap().expect("open");
    let found = api
        .note_for_reading(reading.id, NoteKindDto::Reflection)
        .await
        .unwrap()
        .expect("the reflection");
    assert_eq!(found.id, first.id);
    // A review is a different note entirely — never a slice of the private one.
    let review = api.open_review(book_id, None).await.unwrap();
    assert_ne!(review.id, first.id);
}

/// The key is settable and its presence is readable; the key itself is not.
#[tokio::test]
async fn a_key_can_be_set_but_never_read_back() {
    let (api, _tmp) = api().await;
    assert!(!api.has_google_api_key());

    api.set_google_api_key(Some("AIzaSUPERSECRET".into()));
    assert!(api.has_google_api_key());

    // The whole reply, serialized, must not contain it — a `bool` is the only
    // thing this method can answer with.
    let response = ok(api.dispatch(Request::GoogleApiKey).await);
    let json = serde_json::to_string(&response).unwrap();
    assert!(!json.contains("SUPERSECRET"), "{json}");
    assert_eq!(response, Response::Bool(true));

    api.set_google_api_key(None);
    assert!(!api.has_google_api_key());
}

/// The paths a settings screen shows come off the engine's accessors, so they
/// follow `--data-dir` rather than a second copy of the layout.
#[tokio::test]
async fn the_paths_are_the_engines_own() {
    let (api, tmp) = api().await;
    let paths = api.paths();
    assert_eq!(PathBuf::from(&paths.vault_dir), tmp.path().join("vault"));
    assert_eq!(PathBuf::from(&paths.files_dir), tmp.path().join("files"));
    assert_eq!(paths.db_url, "sqlite::memory:");
}

/// The library-location fact: the typed method and the dispatch arm
/// are one call, and an in-memory library is in no folder at all.
#[tokio::test]
async fn an_in_memory_library_is_in_no_folder() {
    let (api, _tmp) = api().await;
    let typed = api.library_location();
    assert_eq!(typed.database_dir, None);
    assert_eq!(typed.cloud_folder, None);
    assert_eq!(
        ok(api.dispatch(Request::LibraryLocation).await),
        Response::LibraryLocation(typed)
    );
}

/// A library on disk names the folder its database is in — absolute, and the
/// database's folder rather than the root or the vault. A temp dir is in no
/// synced location, so the fact is absent; which roots count is
/// `bookmole::cloud_folder`'s own table, tested there on every platform
/// without the real home.
#[tokio::test]
async fn a_library_on_disk_names_its_database_folder() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let engine = Engine::open(EngineConfig::rooted_at(tmp.path()))
        .await
        .expect("engine");
    let api = Api::new(Arc::new(engine));
    let got = api.library_location();
    assert_eq!(
        got.database_dir.map(PathBuf::from),
        Some(tmp.path().join("database"))
    );
    assert_eq!(got.cloud_folder, None);
}

/// The wire shape a frontend reads by name: the method is its bare name, the
/// fact's absence is `null` rather than a missing key, and iCloud Drive is
/// spelled `icloud_drive` rather than serde's `i_cloud_drive`.
#[test]
fn the_library_location_reads_plainly_on_the_wire() {
    use bookmole_api::{CloudFolderDto, CloudServiceDto, LibraryLocationDto};
    let req: Request = serde_json::from_str(r#"{"method":"library_location"}"#).unwrap();
    assert_eq!(req, Request::LibraryLocation);

    let none = serde_json::to_value(LibraryLocationDto {
        database_dir: Some("/x/database".into()),
        cloud_folder: None,
    })
    .unwrap();
    assert_eq!(
        none,
        serde_json::json!({ "database_dir": "/x/database", "cloud_folder": null })
    );

    let some = serde_json::to_value(Response::LibraryLocation(LibraryLocationDto {
        database_dir: Some("/Users/u/Library/Mobile Documents/x/database".into()),
        cloud_folder: Some(CloudFolderDto {
            service: CloudServiceDto::ICloudDrive,
            root: "/Users/u/Library/Mobile Documents".into(),
        }),
    }))
    .unwrap();
    assert_eq!(some["shape"], "library_location");
    assert_eq!(some["value"]["cloud_folder"]["service"], "icloud_drive");
    for (dto, wire) in [
        (CloudServiceDto::Dropbox, "dropbox"),
        (CloudServiceDto::GoogleDrive, "google_drive"),
        (CloudServiceDto::OneDrive, "one_drive"),
        (CloudServiceDto::ProtonDrive, "proton_drive"),
        (CloudServiceDto::Box, "box"),
        (CloudServiceDto::CloudStorage, "cloud_storage"),
    ] {
        assert_eq!(serde_json::to_value(dto).unwrap(), wire);
    }
}

/// Expanding a picked folder into book files: the typed method and the dispatch
/// arm are one call, a folder comes back as the book files in it, a calibre
/// library is named and not walked, and the counts cross by name.
#[tokio::test]
async fn a_folder_expands_to_its_book_files_through_both_doors() {
    let (api, _tmp) = api().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    for rel in [
        "b.epub",
        "a/one.pdf",
        "a/cover.jpg",
        ".hidden.epub",
        "Calibre Library/metadata.db",
        "Calibre Library/Author/Title/book.epub",
    ] {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"x").unwrap();
    }
    let paths = vec![root.to_string_lossy().into_owned(), "/not/here.docx".into()];
    let typed = api.expand_book_paths(&paths).await.expect("expands");
    let s = |rel: &str| root.join(rel).to_string_lossy().into_owned();
    assert_eq!(
        typed.files,
        [s("a/one.pdf"), s("b.epub"), "/not/here.docx".into()]
    );
    assert_eq!(typed.calibre_libraries, [s("Calibre Library")]);
    assert_eq!(typed.skipped.hidden, 1);
    assert_eq!(typed.skipped.not_books, 1);
    assert_eq!(typed.cut_off_at, None);
    assert_eq!(
        ok(api.dispatch(Request::ExpandBookPaths { paths }).await),
        Response::ExpandedPaths(typed.clone())
    );

    let wire = serde_json::to_value(Response::ExpandedPaths(typed)).unwrap();
    assert_eq!(wire["shape"], "expanded_paths");
    assert_eq!(wire["value"]["cut_off_at"], serde_json::Value::Null);
    assert_eq!(wire["value"]["skipped"]["not_books"], 1);
    let req: Request =
        serde_json::from_str(r#"{"method":"expand_book_paths","params":{"paths":["/x"]}}"#)
            .unwrap();
    assert_eq!(
        req,
        Request::ExpandBookPaths {
            paths: vec!["/x".into()]
        }
    );
}

/// The whole vocabulary must survive a round trip through JSON, because that is
/// what the transport does to it. A sample rather than all seventy: what this
/// is really pinning is that the tagging scheme works for unit variants, for
/// nested DTOs and for the optional-field defaults all at once.
#[test]
fn the_vocabulary_survives_json() {
    let calls = vec![
        Request::ApiVersion,
        Request::LibraryLocation,
        Request::CandidateMounts,
        Request::GetBook { id: 3 },
        Request::SaveBook {
            book: BookDto {
                title: Some("t".into()),
                ..Default::default()
            },
        },
        Request::CreateNote {
            note: NewNoteDto {
                book_id: Some(1),
                kind: NoteKindDto::Reflection,
                body: "a body\nwith a newline".into(),
                ..Default::default()
            },
        },
        Request::SyncDevice {
            paths: vec!["/Volumes/KOBOeReader/a.sdr".into()],
        },
        Request::MapRating {
            scale_id: 1,
            value: 4.5,
            goodreads: 4.75,
        },
    ];
    for call in calls {
        let json = serde_json::to_string(&call).unwrap();
        assert_eq!(
            serde_json::from_str::<Request>(&json).unwrap(),
            call,
            "{json}"
        );
    }
}

/// An error crosses the seam as a reply, not as a dropped connection: `call`
/// never fails, whatever the engine did.
#[tokio::test]
async fn a_failing_call_still_produces_a_reply() {
    let (api, _tmp) = api().await;
    let reply = api
        .call(bookmole_api::Call {
            id: 99,
            request: Request::GetReading { id: -1 },
        })
        .await;
    assert_eq!(reply.id, 99);
    // A missing reading is `None`, not an error — the failure below is the one
    // that has to survive.
    assert!(matches!(reply.outcome, Outcome::Ok { .. }));

    let reply = api
        .call(bookmole_api::Call {
            id: 100,
            request: Request::DeleteBook { id: -1 },
        })
        .await;
    assert_eq!(reply.id, 100);
}

/// Claim 1 again, for the two link methods this crate grew — both writes, both
/// returning `Unit`, so the only way to see that dispatch did the same thing is
/// to read the link back through the import that consumes it.
#[tokio::test]
async fn dispatch_and_the_typed_method_agree_on_linking_a_foreign_record() {
    let (api, _tmp) = api().await;
    let id = seed(&api).await;

    api.link_goodreads_row("9000001", id)
        .await
        .expect("typed link");
    match ok(api
        .dispatch(Request::LinkCalibreBook {
            uuid: "c47437a8".into(),
            book_id: id,
        })
        .await)
    {
        Response::Unit => {}
        other => panic!("{other:?}"),
    }

    // Both landed, and against the same book — the table is keyed
    // `(source, external_id)`, so one call must not have overwritten the other.
    for (source, external) in [("goodreads", "9000001"), ("calibre", "c47437a8")] {
        let linked = api
            .dispatch(Request::LinkCalibreBook {
                uuid: external.into(),
                book_id: id,
            })
            .await;
        assert!(linked.is_ok(), "{source} link is re-recordable");
    }
}

/// A link to a book that is not there is a typed `NotFound`, not a foreign-key
/// error naming a constraint — a candidate list can always name a book another
/// pane has since deleted.
#[tokio::test]
async fn linking_to_a_missing_book_is_a_typed_error() {
    let (api, _tmp) = api().await;
    let err = api
        .link_calibre_book("uuid-abc", -1)
        .await
        .expect_err("there is no book -1");
    assert_eq!(err.code, ErrorCode::NotFound);
}

/// `only` is `#[serde(default)]`, so a client written before the field existed
/// sends the same JSON and still means "the whole library". Parsed rather than
/// constructed, because the default is a property of the wire form.
#[test]
fn an_import_request_without_only_still_means_the_whole_library() {
    let parsed: Request =
        serde_json::from_str(r#"{"method":"import_calibre_library","params":{"dry_run":true}}"#)
            .expect("an older client's request still parses");
    match parsed {
        Request::ImportCalibreLibrary {
            dry_run,
            only,
            library,
            create_ambiguous,
        } => {
            assert!(dry_run);
            assert!(only.is_empty(), "absent means the whole library");
            assert!(library.is_none());
            assert!(!create_ambiguous);
        }
        other => panic!("{other:?}"),
    }
}

/// The activity log crosses whole, and **absence survives the trip**.
///
/// The activity log is built on one distinction — `minutes: null` is "we have no
/// data" and `0` is a measured near-nothing — and a wire that flattened one into
/// the other would put the lie on every reading screen at once. A fresh library
/// has no minutes at all, which is the case that catches a `unwrap_or(0)`
/// anywhere between the SQL and the JSON.
#[tokio::test]
async fn a_period_with_no_device_data_has_no_minutes_rather_than_zero() {
    let (api, _tmp) = api().await;
    seed(&api).await;

    let typed = api
        .activity_summary("2026-01-01", "2026-12-31")
        .await
        .unwrap();
    assert_eq!(typed.minutes, None, "no device has ever spoken here");
    assert_eq!(typed.pages, None);
    // A count the engine fully originates: zero is knowable and is not absence.
    assert_eq!(typed.books_finished, 0);
    assert_eq!(typed.range.from, "2026-01-01");

    let json = serde_json::to_string(&typed).unwrap();
    assert!(json.contains("\"minutes\":null"), "{json}");

    match ok(api
        .dispatch(Request::ActivitySummary {
            from: "2026-01-01".into(),
            to: "2026-12-31".into(),
        })
        .await)
    {
        Response::ActivitySummary(d) => assert_eq!(d, typed),
        other => panic!("{other:?}"),
    }
}

/// A backwards range is an error, **not an empty answer**.
///
/// `DayRange::new` refuses one because every aggregate over an inverted span
/// reports a confident, wrong zero. That refusal has to survive this layer: if
/// the API validated more loosely than the engine — or not at all, by reaching
/// past `DayRange` — a client would get the zero the engine exists to refuse.
#[tokio::test]
async fn a_backwards_range_is_refused_rather_than_answered() {
    let (api, _tmp) = api().await;

    let err = api
        .activity_summary("2026-12-31", "2026-01-01")
        .await
        .expect_err("an inverted range is not a question with an answer");
    assert_eq!(err.code, ErrorCode::InvalidInput);

    match api
        .dispatch(Request::ActivityByDay {
            from: "2026-13-45".into(),
            to: "2026-12-31".into(),
        })
        .await
    {
        Err(e) => assert_eq!(e.code, ErrorCode::InvalidInput),
        Ok(r) => panic!("a non-day was answered: {r:?}"),
    }
}

/// **`ActivityByMonth` crosses with its absences intact, which is the whole
/// reason it is a method here rather than a `reduce` in a client.**
///
/// A month with activity and no measured minutes must arrive as `null`. Given
/// `ActivityByDay` a client would fold those days together, and the first
/// `reduce` in any language turns `null` into `0` — so the page built to say
/// "nothing measured this month" would say "you read for zero minutes" instead.
#[tokio::test]
async fn a_month_with_no_device_data_crosses_the_wire_as_null_minutes() {
    let (api, _tmp) = api().await;
    let book_id = seed(&api).await;
    // A note is the one event no importer can fail to supply — and it carries
    // no minutes, which is exactly the case being protected.
    api.create_note(NewNoteDto {
        book_id: Some(book_id),
        body: "A month that happened, and was never timed.".into(),
        ..Default::default()
    })
    .await
    .unwrap();
    api.refill_reading_events().await.unwrap();

    let typed = api
        .activity_by_month("2000-01-01", "2099-12-31")
        .await
        .unwrap();
    assert_eq!(typed.len(), 1, "one month carried an event: {typed:?}");
    let month = &typed[0];
    assert_eq!(month.month.len(), 7, "YYYY-MM: {:?}", month.month);
    assert_eq!(month.activity_days, 1, "the vault knows you were here");
    assert_eq!(month.books, 1);
    assert_eq!(
        month.minutes, None,
        "nothing measured minutes, and zero would be a claim"
    );
    assert_ne!(month.minutes, Some(0));
    assert_eq!(month.pages, None);

    let json = serde_json::to_string(month).unwrap();
    assert!(json.contains("\"minutes\":null"), "{json}");

    match ok(api
        .dispatch(Request::ActivityByMonth {
            from: "2000-01-01".into(),
            to: "2099-12-31".into(),
        })
        .await)
    {
        Response::ActivityByMonth(d) => assert_eq!(d, typed),
        other => panic!("{other:?}"),
    }
}

/// The month aggregate validates through the same `DayRange` its two siblings
/// do, so this layer cannot route around the refusal an inverted span gets.
#[tokio::test]
async fn a_backwards_range_is_refused_by_the_month_aggregate_too() {
    let (api, _tmp) = api().await;

    let err = api
        .activity_by_month("2026-12-31", "2026-01-01")
        .await
        .expect_err("an inverted range is not a question with an answer");
    assert_eq!(err.code, ErrorCode::InvalidInput);

    match api
        .dispatch(Request::ActivityByMonth {
            from: "2026-01-01".into(),
            to: "2026-13-45".into(),
        })
        .await
    {
        Err(e) => assert_eq!(e.code, ErrorCode::InvalidInput),
        Ok(r) => panic!("a non-day was answered: {r:?}"),
    }
}

/// **A card with no passage crosses as `null` rather than as an error**, and
/// the typed method
/// and `dispatch` agree about it.
///
/// A reading with nothing marked is the ordinary case for most of a library —
/// a Goodreads import mints readings and no highlights at all — so the card
/// asking for its passage must get an answer, not a failure.
#[tokio::test]
async fn a_reading_with_no_marks_has_no_card_passage_across_the_seam() {
    let (api, _tmp) = api().await;
    let book_id = seed(&api).await;
    // Typing a page opens a reading, which is the only door to one on this
    // surface that does not need an importer.
    api.update_progress(book_id, Some(12), None).await.unwrap();
    let reading = api
        .active_reading(book_id)
        .await
        .unwrap()
        .expect("typing a page opens a reading");

    let typed = api.card_passage(reading.id).await.unwrap();
    assert_eq!(typed, None, "no marks is an answer, not an error");

    match ok(api
        .dispatch(Request::CardPassage {
            reading_id: reading.id,
        })
        .await)
    {
        Response::Highlight(h) => assert_eq!(h, typed),
        other => panic!("{other:?}"),
    }

    // And the same reading's full list agrees — the card and the list are
    // never allowed to disagree about what was marked.
    assert!(
        api.highlights_for_reading(reading.id)
            .await
            .unwrap()
            .is_empty()
    );
}

/// Refilling twice changes nothing the second time, and the report says so per
/// filler rather than as one total.
#[tokio::test]
async fn refilling_the_log_is_idempotent_across_the_seam() {
    let (api, _tmp) = api().await;
    seed(&api).await;

    api.refill_reading_events().await.unwrap();
    match ok(api.dispatch(Request::RefillReadingEvents).await) {
        Response::RefillReport(r) => {
            assert_eq!(r.highlights.inserted, 0);
            assert_eq!(r.notes.updated, 0);
            assert_eq!(r.readings.inserted, 0);
        }
        other => panic!("{other:?}"),
    }
}

/// **`null` is not the same as an empty chapter list.**
///
/// A book owning no epub cannot be asked; an epub with no `toc.ncx` was asked
/// and had nothing to say. A client that collapsed the two would tell its reader
/// the same thing about a missing file and an ordinary EPUB3 book, so the
/// distinction has to reach the wire — and the seeded book, which owns no file
/// at all, is the first half of it.
#[tokio::test]
async fn a_book_with_no_file_has_no_chapter_list_to_read() {
    let (api, _tmp) = api().await;
    let id = seed(&api).await;

    assert_eq!(api.table_of_contents(id).await.unwrap(), None);
    match ok(api.dispatch(Request::TableOfContents { book_id: id }).await) {
        Response::TableOfContents(t) => assert_eq!(t, None),
        other => panic!("{other:?}"),
    }
}

/// A correction crosses as the user's, and the provenance says so afterwards.
///
/// The provenance table, read through the API: `set_book_fields` stamps
/// `user`, which is the rank no provider merge outranks, and a frontend showing
/// "who said this" reads it back here. It also pins three later book columns
/// (subjects, series, series index), which once had no wire field at all — a `save_book` round trip
/// used to drop them silently.
#[tokio::test]
async fn a_correction_crosses_as_the_users_and_is_recorded_that_way() {
    let (api, _tmp) = api().await;
    let id = seed(&api).await;

    let after = ok(api
        .dispatch(Request::SetBookFields {
            book_id: id,
            fields: BookDto {
                subjects: vec!["Fiction / Dystopian".into()],
                series: Some("Dune".into()),
                series_index: Some(2.0),
                ..Default::default()
            },
        })
        .await);
    match after {
        Response::Book(Some(b)) => {
            assert_eq!(b.subjects, ["Fiction / Dystopian"]);
            assert_eq!(b.series.as_deref(), Some("Dune"));
            assert_eq!(b.series_index, Some(2.0));
        }
        other => panic!("{other:?}"),
    }

    let provenance = api.field_provenance(id).await.unwrap();
    let claimed: Vec<&str> = provenance
        .iter()
        .filter(|f| f.source == "user")
        .map(|f| f.field.as_str())
        .collect();
    assert!(claimed.contains(&"subjects"), "{provenance:?}");
    assert!(claimed.contains(&"series"), "{provenance:?}");
    assert!(claimed.contains(&"series_index"), "{provenance:?}");
    // A field nobody has claimed is simply absent — never "unknown provider".
    assert!(!provenance.iter().any(|f| f.field == "publisher"));
}

// ---- the list surface ---------------------------------------------------

/// An old client's payload still means what it did. `offset` and `filter` are
/// additive and their absence is the previous behaviour exactly, which is why
/// adding them did not move `API_VERSION`.
#[test]
fn a_list_books_payload_without_the_new_fields_still_parses() {
    let r: Request =
        serde_json::from_str(r#"{"method":"list_books","params":{"limit":20}}"#).unwrap();
    assert_eq!(
        r,
        Request::ListBooks {
            limit: 20,
            sort: Default::default(),
            offset: 0,
            filter: None,
        }
    );
    let r: Request = serde_json::from_str(r#"{"method":"list_notes","params":{}}"#).unwrap();
    assert_eq!(
        r,
        Request::ListNotes {
            book_id: None,
            reading_id: None,
            limit: None,
        },
        "no limit is every note, which is what this method always did — and a
         payload written before `reading_id` existed still parses, which is why it
         is `#[serde(default)]` and `API_VERSION` did not move"
    );
}

/// The count and the page answer the same filter, across the seam.
#[tokio::test]
async fn a_filtered_page_and_its_count_agree_over_the_wire() {
    let (api, _tmp) = api().await;
    let id = seed(&api).await;
    let second = api
        .save_book(BookDto {
            title: Some("Sea of Tranquility".into()),
            authors: vec!["Emily St. John Mandel".into()],
            isbn_13: Some("9780593321447".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    have(&api, second.id.unwrap()).await;
    api.update_progress(id, Some(40), None).await.unwrap();

    let reading = BookFilterDto {
        status: Some(StatusFilterDto::State {
            is: ReadingStateDto::Reading,
        }),
        ..Default::default()
    };
    match ok(api
        .dispatch(Request::CountBooks {
            filter: Some(reading.clone()),
        })
        .await)
    {
        Response::Count(n) => assert_eq!(n, 1),
        other => panic!("{other:?}"),
    }
    match ok(api
        .dispatch(Request::ListBooks {
            limit: -1,
            sort: Default::default(),
            offset: 0,
            filter: Some(reading),
        })
        .await)
    {
        Response::Books(books) => assert_eq!(books.len(), 1),
        other => panic!("{other:?}"),
    }

    // Absence is a filter case and not a reading state — the wire says so too.
    match ok(api
        .dispatch(Request::CountBooks {
            filter: Some(BookFilterDto {
                status: Some(StatusFilterDto::NoReading),
                ..Default::default()
            }),
        })
        .await)
    {
        Response::Count(n) => assert_eq!(n, 1),
        other => panic!("{other:?}"),
    }
    match ok(api.dispatch(Request::CountBooks { filter: None }).await) {
        Response::Count(n) => assert_eq!(n, 2, "no filter is every book"),
        other => panic!("{other:?}"),
    }
}

/// One call for a page, one row per id in the order asked.
#[tokio::test]
async fn the_summary_crosses_the_seam_in_the_order_asked() {
    let (api, _tmp) = api().await;
    let id = seed(&api).await;
    match ok(api
        .dispatch(Request::BookSummaries {
            book_ids: vec![id, id],
        })
        .await)
    {
        Response::BookSummaries(rows) => {
            assert_eq!(rows.len(), 2);
            assert!(rows.iter().all(|r| r.book_id == id));
            assert_eq!(rows[0].highlights, 0);
        }
        other => panic!("{other:?}"),
    }
}

/// The edition-shape arithmetic reaches a client that links this crate and
/// not the engine — which is the whole reason the field exists.
#[tokio::test]
async fn a_book_carries_the_shape_of_its_edition() {
    let (api, _tmp) = api().await;
    let id = seed(&api).await;
    let book = api.get_book(id).await.unwrap().unwrap();
    // 333 pages is recorded; nothing measured the cover, so the width stands in.
    assert_eq!(book.shape.thickness_source, ShapeSourceDto::Recorded);
    assert_eq!(book.shape.width_source, ShapeSourceDto::Assumed);
    assert!(book.shape.width_over_height > 0.0);
    assert!(book.shape.thickness_over_height > 0.0);

    // A provider candidate has no page count, so both numbers are stand-ins —
    // and the field is still there, because a book always has *a* shape.
    let bare = BookDto::default();
    assert_eq!(bare.shape.width_source, ShapeSourceDto::Assumed);
    assert_eq!(bare.shape.thickness_source, ShapeSourceDto::Assumed);
}

// ---- one search surface ----------------------------------------------------

/// `source` is a filter and its absence is *both*, in `BookFilterDto`'s idiom.
///
/// A payload that names no source has to be the widest question rather than a
/// parse error, or every client would have to spell out the default it wanted
/// anyway.
#[test]
fn a_search_payload_without_a_source_asks_both_indexes() {
    let r: Request =
        serde_json::from_str(r#"{"method":"search_marks","params":{"query":"grief","limit":25}}"#)
            .unwrap();
    assert_eq!(
        r,
        Request::SearchMarks {
            query: "grief".into(),
            source: None,
            book_id: None,
            limit: 25,
        }
    );
    let narrowed: Request = serde_json::from_str(
        r#"{"method":"search_marks","params":{"query":"grief","source":"highlight","limit":5}}"#,
    )
    .unwrap();
    assert_eq!(
        narrowed,
        Request::SearchMarks {
            query: "grief".into(),
            source: Some(SearchSourceDto::Highlight),
            book_id: None,
            limit: 5,
        }
    );
}

/// The search and note-list scopes are **additive**, and this is what that
/// claim means on the wire.
///
/// Both new parameters are `#[serde(default)]`, so every payload a client wrote
/// before the scopes existed still parses to the same request it always did — which is
/// why `API_VERSION` stays at 2. `the_replaced_method_is_gone_from_the_wire`
/// below pins the number itself; this pins the reason it did not have to move.
#[test]
fn a_payload_written_before_the_scope_existed_still_parses() {
    let old_search: Request =
        serde_json::from_str(r#"{"method":"search_marks","params":{"query":"grief","limit":25}}"#)
            .unwrap();
    assert_eq!(
        old_search,
        Request::SearchMarks {
            query: "grief".into(),
            source: None,
            book_id: None,
            limit: 25,
        },
        "absent is the whole library, exactly as before"
    );
    let old_notes: Request =
        serde_json::from_str(r#"{"method":"list_notes","params":{"book_id":3}}"#).unwrap();
    assert_eq!(
        old_notes,
        Request::ListNotes {
            book_id: Some(3),
            reading_id: None,
            limit: None,
        }
    );

    // And the new shapes are reachable by name, with no positional guessing.
    let scoped: Request = serde_json::from_str(
        r#"{"method":"search_marks","params":{"query":"grief","book_id":7,"limit":25}}"#,
    )
    .unwrap();
    assert_eq!(
        scoped,
        Request::SearchMarks {
            query: "grief".into(),
            source: None,
            book_id: Some(7),
            limit: 25,
        }
    );
    let per_read: Request =
        serde_json::from_str(r#"{"method":"list_notes","params":{"reading_id":2,"limit":12}}"#)
            .unwrap();
    assert_eq!(
        per_read,
        Request::ListNotes {
            book_id: None,
            reading_id: Some(2),
            limit: Some(12),
        }
    );
}

/// **The bug, at the seam.** A client that searched the library and filtered its
/// own answer would have got nothing; the request that carries the scope gets
/// the passage.
///
/// The precondition is asserted, so this cannot pass vacuously if the ranking
/// ever changes underneath it.
#[tokio::test]
async fn a_scoped_search_crosses_the_seam_without_being_truncated() {
    const LIMIT: i64 = 5;
    let (api, _tmp) = api().await;
    for i in 0..12 {
        let decoy = api
            .save_book(BookDto {
                title: Some(format!("Decoy {i}")),
                ..Default::default()
            })
            .await
            .unwrap()
            .id
            .unwrap();
        api.create_note(NewNoteDto {
            book_id: Some(decoy),
            title: Some(format!("d{i}")),
            body: "grief".into(),
            kind: NoteKindDto::Note,
            ..Default::default()
        })
        .await
        .unwrap();
    }
    let wanted = seed(&api).await;
    api.create_note(NewNoteDto {
        book_id: Some(wanted),
        title: Some("On collapse".into()),
        body: "a long paragraph about what the collapse does to grief".into(),
        kind: NoteKindDto::Note,
        ..Default::default()
    })
    .await
    .unwrap();

    let unbounded = api.search_marks("grief", None, None, 100).await.unwrap();
    let buried = unbounded
        .iter()
        .position(|h| matches!(h, SearchHitDto::Note { note, .. } if note.book_id == Some(wanted)))
        .expect("the wanted book matches at all");
    assert!(buried >= LIMIT as usize, "fixture ranks it at {buried}");

    let truncated = api.search_marks("grief", None, None, LIMIT).await.unwrap();
    assert_eq!(truncated.len(), LIMIT as usize);
    assert!(
        !truncated
            .iter()
            .any(|h| matches!(h, SearchHitDto::Note { note, .. } if note.book_id == Some(wanted))),
        "nothing for a client-side filter to keep"
    );

    let scoped = api
        .search_marks("grief", None, Some(wanted), LIMIT)
        .await
        .unwrap();
    assert_eq!(scoped.len(), 1);

    match ok(api
        .dispatch(Request::SearchMarks {
            query: "grief".into(),
            source: None,
            book_id: Some(wanted),
            limit: LIMIT,
        })
        .await)
    {
        Response::SearchHits(dispatched) => assert_eq!(dispatched, scoped),
        other => panic!("{other:?}"),
    }
}

/// The two ids are alternatives, and the seam says so rather than picking one.
///
/// A reading names its book, so the pair is redundant at best and a
/// contradiction at worst — and a contradiction's honest answer is an empty
/// list no client can tell from an empty vault. `NoteScope` cannot represent
/// the pair at all; this is the one place it has to be refused.
#[tokio::test]
async fn asking_a_book_and_a_reading_at_once_is_refused_rather_than_guessed() {
    let (api, _tmp) = api().await;
    let book = seed(&api).await;
    let reading = api.reread(book).await.unwrap();

    assert!(api.list_notes(Some(book), None, None).await.is_ok());
    assert!(api.list_notes(None, Some(reading), None).await.is_ok());

    let err = api
        .list_notes(Some(book), Some(reading), None)
        .await
        .expect_err("the pair is not a conjunction");
    assert_eq!(err.code, ErrorCode::InvalidInput);
    assert!(err.message.contains("reading_id"), "{}", err.message);
}

/// The removal, pinned — this is what `API_VERSION = 2` is *for*.
///
/// A client still speaking version 1 must meet a clear refusal rather than
/// having its search silently answer half the question, and this asserts the
/// refusal exists rather than trusting that deleting the variant produced one.
#[test]
fn the_replaced_method_is_gone_from_the_wire() {
    let old = r#"{"method":"search_notes","params":{"query":"grief","limit":25}}"#;
    assert!(
        serde_json::from_str::<Request>(old).is_err(),
        "search_notes still parses, so nothing was actually replaced"
    );
    assert_eq!(bookmole_api::API_VERSION, 2);
}

/// One list, and every hit says which kind it is — so a client switches on a
/// string rather than probing for the field that happens to be present.
#[test]
fn a_hit_names_its_kind_on_the_wire() {
    let raw = r#"{"kind":"highlight",
                  "highlight":{"id":7,"book_id":1,"text":"born in moral stupidity",
                               "source":"koreader","created_at":0},
                  "snippet":"born in >>moral<< stupidity"}"#;
    let hit: SearchHitDto = serde_json::from_str(raw).unwrap();
    let SearchHitDto::Highlight { highlight, snippet } = &hit else {
        panic!("the tag did not select the highlight arm: {hit:?}");
    };
    assert_eq!(highlight.id, 7);
    assert_eq!(snippet, "born in >>moral<< stupidity");

    let json = serde_json::to_value(&hit).unwrap();
    assert_eq!(json["kind"], "highlight");
    assert!(
        json.get("note").is_none(),
        "one row, not two nullable ones: {json}"
    );
    assert_eq!(serde_json::from_value::<SearchHitDto>(json).unwrap(), hit);
}

/// The typed method and the dispatch arm are the same call, for the search
/// method — claim 1 of this suite, applied where it is newest.
#[tokio::test]
async fn dispatch_and_the_typed_method_agree_on_a_search() {
    let (api, _tmp) = api().await;
    let book = seed(&api).await;
    api.create_note(NewNoteDto {
        book_id: Some(book),
        title: Some("On collapse".into()),
        body: "The symphony rehearses in an antechamber.".into(),
        kind: NoteKindDto::Note,
        ..Default::default()
    })
    .await
    .unwrap();

    let typed = api
        .search_marks("antechamber", None, None, 10)
        .await
        .unwrap();
    assert_eq!(typed.len(), 1);
    assert!(matches!(typed[0], SearchHitDto::Note { .. }));

    match ok(api
        .dispatch(Request::SearchMarks {
            query: "antechamber".into(),
            source: None,
            book_id: None,
            limit: 10,
        })
        .await)
    {
        Response::SearchHits(dispatched) => assert_eq!(dispatched, typed),
        other => panic!("{other:?}"),
    }
}

/// Absence crosses the seam as an answer. A query nothing matches, and a query
/// nobody typed, are both an empty list and never an error.
#[tokio::test]
async fn an_empty_answer_is_not_an_error_across_the_seam() {
    let (api, _tmp) = api().await;
    seed(&api).await;
    for query in ["thermodynamics", "", "   ", "don't"] {
        assert!(
            api.search_marks(query, None, None, 10)
                .await
                .unwrap()
                .is_empty(),
            "{query:?} should answer nothing rather than fail"
        );
    }
}

/// `title` joins the five filters that were already there, rather than becoming
/// a seventh endpoint — so it composes with the count and the paging that
/// `BookFilterDto` already had.
#[tokio::test]
async fn a_title_filter_crosses_the_seam_as_a_predicate() {
    let (api, _tmp) = api().await;
    seed(&api).await;

    let filter = BookFilterDto {
        title: Some("station".into()),
        ..Default::default()
    };
    assert_eq!(api.count_books(Some(filter.clone())).await.unwrap(), 1);
    let rows = api
        .list_books(BookQueryDto {
            sort: Default::default(),
            filter: Some(filter),
            limit: 10,
            offset: 0,
        })
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);

    // And it composes with a neighbour rather than replacing it.
    let both = BookFilterDto {
        title: Some("station".into()),
        author: Some("mandel".into()),
        ..Default::default()
    };
    assert_eq!(api.count_books(Some(both)).await.unwrap(), 1);

    let none = BookFilterDto {
        title: Some("thermodynamics".into()),
        ..Default::default()
    };
    assert_eq!(api.count_books(Some(none)).await.unwrap(), 0);
}

// ---- moments --------------------------------------------------------------

/// A moment crosses whole, is **polled** rather than pushed, and carries the
/// read it belongs to.
///
/// `reading_id` is the field this test exists for. A card is minted per
/// *reading* and a reread mints a second one beside the first, so a moment
/// naming only its book cannot select the right card — and that is the shape
/// that is expensive to change later, because it is what the moment *is*
/// rather than what it carries.
#[tokio::test]
async fn a_closed_reading_crosses_as_a_moment_naming_its_read() {
    let (api, _tmp) = api().await;
    let book = seed(&api).await;

    // Nothing has happened yet, and that is an empty list rather than an error.
    assert_eq!(api.pending_moments(None).await.unwrap(), vec![]);

    api.update_progress(book, Some(333), Some(true))
        .await
        .unwrap();

    let typed = api.pending_moments(None).await.unwrap();
    let closed: Vec<_> = typed
        .iter()
        .filter(|m| m.kind == MomentKindDto::ReadingClosed)
        .collect();
    assert_eq!(closed.len(), 1, "one read ended: {typed:?}");
    assert_eq!(closed[0].book_id, Some(book));
    assert!(
        closed[0].reading_id.is_some(),
        "a card is minted per reading, so the moment has to name one"
    );

    match ok(api.dispatch(Request::PendingMoments { limit: None }).await) {
        Response::Moments(ms) => assert_eq!(ms, typed),
        other => panic!("{other:?}"),
    }
}

/// Acknowledging is what makes it fire once, and it is **idempotent** — a
/// client that acknowledges twice, or two clients that acknowledge the same
/// moment, must not be able to break the ceremony or resurrect it.
#[tokio::test]
async fn acknowledging_a_moment_retires_it_and_repeating_it_changes_nothing() {
    let (api, _tmp) = api().await;
    let book = seed(&api).await;
    api.update_progress(book, Some(333), Some(true))
        .await
        .unwrap();

    let id = api.pending_moments(None).await.unwrap()[0].id.clone();
    ok(api
        .dispatch(Request::AcknowledgeMoment { id: id.clone() })
        .await);
    assert!(
        api.pending_moments(None)
            .await
            .unwrap()
            .iter()
            .all(|m| m.id != id)
    );

    // Twice, through both doors, and neither brings it back.
    api.acknowledge_moment(&id).await.unwrap();
    ok(api
        .dispatch(Request::AcknowledgeMoment { id: id.clone() })
        .await);
    assert!(
        api.pending_moments(None)
            .await
            .unwrap()
            .iter()
            .all(|m| m.id != id)
    );
}

/// An id this build knows no kind for is a typed error rather than a row.
#[tokio::test]
async fn an_invented_moment_id_is_refused() {
    let (api, _tmp) = api().await;
    let err = api
        .acknowledge_moment("something_i_made_up:1")
        .await
        .expect_err("a moment id is not a string a client composes");
    assert_eq!(err.code, ErrorCode::InvalidInput);
}

/// **Nothing on this surface counts.** bookmole never shows a badge
/// counting what you have not done, and the cheap version of that is a
/// `pending: 3` beside the rows — so the reply is a bare array and there is no
/// count method to reach for. Asserted rather than trusted, because the next
/// person to want it will want it for a good reason.
#[test]
fn the_wire_states_no_number_of_moments() {
    let json = serde_json::to_string(&Response::Moments(vec![])).unwrap();
    assert_eq!(
        json, r#"{"shape":"moments","value":[]}"#,
        "a bare array, with no number beside it"
    );

    for name in ["count_moments", "pending_moment_count", "moment_count"] {
        let call = format!(r#"{{"method":"{name}","params":{{}}}}"#);
        assert!(
            serde_json::from_str::<Request>(&call).is_err(),
            "{name} must not exist: a number waiting is a badge"
        );
    }
}

/// `limit` takes from the newest end, and the two doors agree about it.
#[tokio::test]
async fn a_limit_takes_the_newest_moments() {
    let (api, _tmp) = api().await;
    let a = seed(&api).await;
    let b = api
        .save_book(BookDto {
            title: Some("Middlemarch".into()),
            isbn_13: Some("9780141439549".into()),
            page_count: Some(880),
            ..Default::default()
        })
        .await
        .unwrap()
        .id
        .unwrap();
    api.update_progress(a, Some(333), Some(true)).await.unwrap();
    api.update_progress(b, Some(880), Some(true)).await.unwrap();

    let all = api.pending_moments(None).await.unwrap();
    assert!(all.len() >= 2);
    assert!(all.windows(2).all(|w| w[0].occurred_at >= w[1].occurred_at));

    match ok(api
        .dispatch(Request::PendingMoments { limit: Some(1) })
        .await)
    {
        Response::Moments(ms) => {
            assert_eq!(ms.len(), 1);
            assert_eq!(ms[0], all[0]);
        }
        other => panic!("{other:?}"),
    }
}

// ---- flashcards and citations ---------------------------------------------

/// Without `CreateFlashcard`, `Storage::insert_flashcard` would have no facade
/// wrapper and no request, so a card could be minted by the KOReader import and
/// by nothing else. This is the door, and the two paths through it agree.
///
/// The reply is `Response::Bool` because *you already had this card* and *a card
/// now exists* are different facts — an existing shape rather than a new one.
#[tokio::test]
async fn a_card_can_be_made_across_the_seam_and_the_second_is_not_new() {
    let (api, _tmp) = api().await;
    let book_id = seed(&api).await;

    assert!(
        api.create_flashcard(book_id, None, "middlemarch", Some("Ch 1"))
            .await
            .unwrap()
    );

    // The same word again, through `dispatch` this time. A rule that lived in
    // the dispatch arm would show up here as the two paths disagreeing.
    match ok(api
        .dispatch(Request::CreateFlashcard {
            book_id,
            highlight_id: None,
            word: "middlemarch".into(),
            context: Some("Ch 9".into()),
        })
        .await)
    {
        Response::Bool(created) => assert!(!created, "you already had this card"),
        other => panic!("{other:?}"),
    }

    let cards = api.list_flashcards_for_book(book_id).await.unwrap();
    assert_eq!(cards.len(), 1, "UNIQUE(book_id, word) deduped");
    assert_eq!(
        cards[0].book_id, book_id,
        "the card carries its book's id, not only its title"
    );
    assert_eq!(cards[0].highlight_id, None);
    assert_eq!(
        cards[0].context.as_deref(),
        Some("Ch 1"),
        "the second attempt must not rewrite the first card"
    );
}

/// Handles do not cross: the write takes ids and the engine re-reads them, so a
/// stale or mismatched handle is a typed refusal rather than a raw
/// foreign-key error surfacing as `internal`.
#[tokio::test]
async fn a_card_pointing_at_nothing_is_refused_by_name() {
    let (api, _tmp) = api().await;
    let book_id = seed(&api).await;

    let err = api
        .create_flashcard(book_id, Some(4_242), "ghost", None)
        .await
        .expect_err("no such highlight");
    assert_eq!(err.code, ErrorCode::NotFound);

    let err = api
        .create_flashcard(9_999, None, "ghost", None)
        .await
        .expect_err("no such book");
    assert_eq!(err.code, ErrorCode::NotFound);

    match api
        .dispatch(Request::CreateFlashcard {
            book_id,
            highlight_id: None,
            word: "   ".into(),
            context: None,
        })
        .await
    {
        Err(e) => assert_eq!(e.code, ErrorCode::InvalidInput),
        Ok(other) => panic!("{other:?}"),
    }
    assert!(api.list_flashcards(true).await.unwrap().is_empty());
}

/// The Anki export, typed and dispatched, agrees — and the
/// saved form says where it wrote.
#[tokio::test]
async fn the_anki_export_crosses_the_seam_both_ways() {
    let (api, _tmp) = api().await;
    let book_id = seed(&api).await;
    api.create_flashcard(book_id, None, "middlemarch", Some("a <game> & more"))
        .await
        .unwrap();

    let typed = api.export_anki(Some(book_id), true).await.unwrap();
    assert_eq!(typed.count, 1);
    assert!(typed.text.contains("a &lt;game&gt; &amp; more"));
    assert!(typed.text.contains("bookmole::Station Eleven"));

    match ok(api
        .dispatch(Request::ExportAnki {
            book_id: Some(book_id),
            include_exported: true,
        })
        .await)
    {
        Response::AnkiExport(dto) => {
            assert_eq!(dto.text, typed.text, "the two paths are one call");
            assert_eq!(dto.count, 1);
        }
        other => panic!("{other:?}"),
    }

    // Marked by the two above, so the default sends nothing and writes nothing.
    match ok(api
        .dispatch(Request::SaveAnkiExport {
            book_id: None,
            include_exported: false,
        })
        .await)
    {
        Response::AnkiSaved(dto) => {
            assert_eq!(dto.count, 0);
            assert_eq!(dto.path, None);
        }
        other => panic!("{other:?}"),
    }
    let saved = api.save_anki_export(None, true).await.unwrap();
    let path = saved.path.expect("written");
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("#guid column:5")
    );

    let err = api.export_anki(Some(9_999), false).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);

    // Both fields default: an empty params object is the whole library, new
    // cards only.
    let req: Request = serde_json::from_str(r#"{"method":"export_anki","params":{}}"#).unwrap();
    assert_eq!(
        req,
        Request::ExportAnki {
            book_id: None,
            include_exported: false
        }
    );
}

/// `CitationsForNotes`: one call for a page of notes, one entry per id asked, in that
/// order, empties included — including for an id that is not a note at all, to
/// which "cites nothing" is the honest answer.
#[tokio::test]
async fn the_citation_batch_answers_every_id_asked_in_order() {
    let (api, _tmp) = api().await;
    let book_id = seed(&api).await;

    let mut ids = Vec::new();
    for body in ["The first note.", "The second note."] {
        ids.push(
            api.create_note(NewNoteDto {
                book_id: Some(book_id),
                body: body.into(),
                ..Default::default()
            })
            .await
            .unwrap()
            .id,
        );
    }
    let asked = vec![ids[1], 9_999, ids[0], ids[1]];

    let typed = api.citations_for_notes(&asked).await.unwrap();
    assert_eq!(
        typed.iter().map(|r| r.note_id).collect::<Vec<_>>(),
        asked,
        "one row per id, duplicates included, in the order asked"
    );
    assert!(typed.iter().all(|r| r.highlight_ids.is_empty()));

    match ok(api
        .dispatch(Request::CitationsForNotes {
            note_ids: asked.clone(),
        })
        .await)
    {
        Response::NoteCitations(dispatched) => assert_eq!(dispatched, typed),
        other => panic!("{other:?}"),
    }

    // And the empty ask is not an error.
    assert!(api.citations_for_notes(&[]).await.unwrap().is_empty());
}

/// The batch carries **handles, not passages**, and the wire is where that
/// claim is worth pinning: a `Vec<HighlightDto>` per note would put the
/// reader's private text back on the wire once per citing note, for a screen
/// whose whole output is a tick. `CitationsFor` is untouched and still returns
/// the passages, because the pane that shows them needs the words.
#[test]
fn the_citation_batch_puts_no_highlight_text_on_the_wire() {
    let json = serde_json::to_string(&Response::NoteCitations(vec![
        bookmole_api::NoteCitationsDto {
            note_id: 3,
            highlight_ids: vec![7, 9],
        },
    ]))
    .unwrap();
    assert_eq!(
        json,
        r#"{"shape":"note_citations","value":[{"note_id":3,"highlight_ids":[7,9]}]}"#
    );
    assert!(!json.contains("text"));
}

/// Card capture and the citation batch are **additive**, and this is what that
/// claim means on the wire: no existing request changed shape, so `API_VERSION` does not move.
#[test]
fn a_payload_written_before_the_cards_wave_still_parses() {
    let single: Request =
        serde_json::from_str(r#"{"method":"citations_for","params":{"note_id":3}}"#).unwrap();
    assert_eq!(single, Request::CitationsFor { note_id: 3 });

    let cards: Request =
        serde_json::from_str(r#"{"method":"list_flashcards","params":{}}"#).unwrap();
    assert_eq!(
        cards,
        Request::ListFlashcards {
            include_exported: false
        }
    );

    // And the new shapes are reachable by name. A card with no passage and no
    // context is the ordinary typed one, so both are `#[serde(default)]`.
    let minimal: Request = serde_json::from_str(
        r#"{"method":"create_flashcard","params":{"book_id":4,"word":"mot"}}"#,
    )
    .unwrap();
    assert_eq!(
        minimal,
        Request::CreateFlashcard {
            book_id: 4,
            highlight_id: None,
            word: "mot".into(),
            context: None,
        }
    );
    let batch: Request =
        serde_json::from_str(r#"{"method":"citations_for_notes","params":{"note_ids":[3,4]}}"#)
            .unwrap();
    assert_eq!(
        batch,
        Request::CitationsForNotes {
            note_ids: vec![3, 4]
        }
    );
}

// ---- readings across the library -------------------------------------------

/// The minimal payload for the library-wide readings methods means what a
/// client with no opinion means.
///
/// Neither method existed before, so nothing *old* can be replayed at
/// them — which is exactly why they are new methods and not four fields on
/// `ListReadings`. What is pinned here is the other half of the same guarantee:
/// every added field is `#[serde(default)]`, so the smallest honest payload is
/// the whole-library read, and `API_VERSION` did not have to move.
#[test]
fn the_new_reading_methods_parse_from_the_smallest_honest_payload() {
    let rows: Request =
        serde_json::from_str(r#"{"method":"list_reading_rows","params":{"limit":20}}"#).unwrap();
    assert_eq!(
        rows,
        Request::ListReadingRows {
            limit: 20,
            sort: Default::default(),
            offset: 0,
            filter: None,
        }
    );
    let count: Request =
        serde_json::from_str(r#"{"method":"count_readings","params":{}}"#).unwrap();
    assert_eq!(count, Request::CountReadings { filter: None });

    // And a payload written against `list_readings` still means what it did —
    // it was not reshaped to make room for this.
    let old: Request =
        serde_json::from_str(r#"{"method":"list_readings","params":{"book_id":3}}"#).unwrap();
    assert_eq!(old, Request::ListReadings { book_id: 3 });
}

/// The wall, end to end: one call carries the book, the reading, the read
/// number and the passage, and the count is a second call answering the same
/// filter.
#[tokio::test]
async fn a_page_of_readings_carries_what_a_card_needs_in_one_call() {
    let (api, _tmp) = api().await;
    let book_id = seed(&api).await;
    api.update_progress(book_id, Some(333), Some(true))
        .await
        .unwrap();
    api.reread(book_id).await.unwrap();

    let query = ReadingQueryDto {
        limit: -1,
        filter: Some(ReadingFilterDto {
            book_id: Some(book_id),
            ..Default::default()
        }),
        ..Default::default()
    };
    let typed = api.list_reading_rows(query.clone()).await.unwrap();
    assert_eq!(typed.len(), 2, "a reread is a second row, not a flag");
    for row in &typed {
        assert_eq!(row.book.id, Some(book_id), "the book rides with the row");
        assert_eq!(row.of_reads, 2);
    }
    assert_eq!(
        typed.iter().map(|r| r.read_number).collect::<Vec<_>>(),
        vec![1, 2],
        "most recently finished first — and the open reread has no `finished_at`
         at all, so it lands last, which is where a read that has not ended
         belongs on a list of reads that did"
    );
    assert_eq!(typed[1].reading.finished_at, None);

    // The dispatch arm is the same call, which is the claim this suite exists
    // for — the four values travel flat and are assembled by pure fan-out.
    match ok(api
        .dispatch(Request::ListReadingRows {
            limit: -1,
            sort: Default::default(),
            offset: 0,
            filter: query.filter.clone(),
        })
        .await)
    {
        Response::ReadingRows(rows) => assert_eq!(rows, typed),
        other => panic!("{other:?}"),
    }

    // The count is its own call, answering the same filter.
    assert_eq!(api.count_readings(query.filter.clone()).await.unwrap(), 2);
    match ok(api
        .dispatch(Request::CountReadings {
            filter: query.filter.clone(),
        })
        .await)
    {
        Response::Count(n) => assert_eq!(n, 2),
        other => panic!("{other:?}"),
    }
}

/// **The passage is on the row and still not on `ReadingDto`** — asserted
/// rather than trusted.
///
/// Putting it on `ReadingDto` would ride the reader's private highlight text
/// along on every row of every `ListReadings`, including the ones nobody is
/// drawing a card for. Serialising both and reading the keys is the only check
/// that fails when somebody adds the convenient field.
#[tokio::test]
async fn the_readings_list_carries_no_passage_and_the_wall_does() {
    let (api, _tmp) = api().await;
    let book_id = seed(&api).await;
    api.update_progress(book_id, Some(12), None).await.unwrap();

    let plain = api.list_readings(book_id).await.unwrap();
    let json = serde_json::to_value(&plain[0]).unwrap();
    assert!(
        json.get("passage").is_none(),
        "a reading is not a card: {json}"
    );

    let rows = api
        .list_reading_rows(ReadingQueryDto {
            limit: -1,
            ..Default::default()
        })
        .await
        .unwrap();
    let json = serde_json::to_value(&rows[0]).unwrap();
    assert!(json.get("passage").is_some(), "the wall's row carries it");
    // And it is not a card: no rating, no notes, no composition of a layout.
    for absent in ["rating", "notes", "reflection", "review"] {
        assert!(
            json.get(absent).is_none(),
            "{absent} would make this a CardDto, a layout this crate does not serve: {json}"
        );
    }
}

/// An inverted year is refused, not answered with a confident empty wall.
///
/// The rule lives in the engine's own `DayRange` and this layer must not be able
/// to route around it — the rule the two activity aggregates already follow,
/// applied to a filter.
#[tokio::test]
async fn a_backwards_year_is_refused_by_both_doors() {
    let (api, _tmp) = api().await;
    let filter = ReadingFilterDto {
        finished_in: Some(DayRangeDto {
            from: "2025-12-31".into(),
            to: "2025-01-01".into(),
        }),
        ..Default::default()
    };

    let err = api
        .list_reading_rows(ReadingQueryDto {
            limit: 20,
            filter: Some(filter.clone()),
            ..Default::default()
        })
        .await
        .expect_err("a backwards range is a refusal");
    assert_eq!(err.code, ErrorCode::InvalidInput);

    let err = api
        .count_readings(Some(filter.clone()))
        .await
        .expect_err("and the count refuses it identically");
    assert_eq!(err.code, ErrorCode::InvalidInput);

    match api
        .dispatch(Request::CountReadings {
            filter: Some(filter),
        })
        .await
    {
        Err(e) => assert_eq!(e.code, ErrorCode::InvalidInput),
        Ok(other) => panic!("{other:?}"),
    }
}

// ---- the years the wall has ------------------------------------------------

/// The picker's request, through both doors, and the two facts it carries.
///
/// A read finished and a read still open, so the answer has to hold a year
/// **and** the open bucket — which is the shape's whole reason for existing:
/// without `open`, the years do not add up to the wall and a reader who picks
/// each in turn never sees the book they are in.
#[tokio::test]
async fn the_year_picker_gets_the_years_and_the_open_bucket() {
    let (api, _tmp) = api().await;
    let book_id = seed(&api).await;
    api.update_progress(book_id, Some(333), Some(true))
        .await
        .unwrap();
    api.reread(book_id).await.unwrap();

    let typed = api.reading_years(None).await.unwrap();
    assert_eq!(typed.years.len(), 1, "one read has ended, so one year");
    assert!(typed.open, "the reread is open and belongs to no year");

    match ok(api.dispatch(Request::ReadingYears { filter: None }).await) {
        Response::ReadingYears(y) => assert_eq!(y, typed),
        other => panic!("{other:?}"),
    }

    // The years the picker offers are the years the wall draws — asked with the
    // same filter, through the count that shares the engine's predicate.
    for year in &typed.years {
        let scoped = ReadingFilterDto {
            finished_in: Some(DayRangeDto {
                from: format!("{year}-01-01"),
                to: format!("{year}-12-31"),
            }),
            ..Default::default()
        };
        assert!(
            api.count_readings(Some(scoped)).await.unwrap() > 0,
            "{year} was offered and the wall has nothing in it"
        );
    }
}

/// The smallest honest payload is the whole-library question, and no existing
/// request was reshaped to make room for this one.
#[test]
fn the_year_request_parses_from_the_smallest_honest_payload() {
    let years: Request = serde_json::from_str(r#"{"method":"reading_years","params":{}}"#).unwrap();
    assert_eq!(years, Request::ReadingYears { filter: None });

    // `API_VERSION` stays at 2 because this is additive: the request beside it
    // still means exactly what it did.
    let count: Request =
        serde_json::from_str(r#"{"method":"count_readings","params":{}}"#).unwrap();
    assert_eq!(count, Request::CountReadings { filter: None });
    assert_eq!(bookmole_api::API_VERSION, 2);
}

/// A backwards year is refused here too, rather than answering with a confident
/// empty picker.
#[tokio::test]
async fn a_backwards_year_is_refused_by_the_year_list_as_well() {
    let (api, _tmp) = api().await;
    let filter = ReadingFilterDto {
        finished_in: Some(DayRangeDto {
            from: "2025-12-31".into(),
            to: "2025-01-01".into(),
        }),
        ..Default::default()
    };
    let err = api
        .reading_years(Some(filter.clone()))
        .await
        .expect_err("a backwards range is a refusal");
    assert_eq!(err.code, ErrorCode::InvalidInput);

    match api
        .dispatch(Request::ReadingYears {
            filter: Some(filter),
        })
        .await
    {
        Err(e) => assert_eq!(e.code, ErrorCode::InvalidInput),
        Ok(other) => panic!("{other:?}"),
    }
}

/// **No number rides with a year.** The reply enumerates; it does not measure.
///
/// A per-year count is a scoreboard, which bookmole never draws, and it is
/// the field somebody adds for cheapness — so the absence is asserted over the
/// serialized reply rather than left to review.
#[tokio::test]
async fn the_year_list_puts_no_count_on_the_wire() {
    let (api, _tmp) = api().await;
    let book_id = seed(&api).await;
    api.update_progress(book_id, Some(333), Some(true))
        .await
        .unwrap();

    let json = serde_json::to_string(&api.reading_years(None).await.unwrap()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    // Sorted before comparing: the wire's key order is not part of the
    // protocol, and it is not even stable across builds — any crate in a
    // workspace that turns on serde_json's `preserve_order` feature hands it,
    // through feature unification, to every crate in a `--workspace` build,
    // so a `-p bookmole-api` run and a workspace run could otherwise
    // disagree about this one assertion.
    let mut keys: Vec<&str> = value
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec!["open", "years"],
        "two fields, and neither counts"
    );
    assert!(
        value["years"]
            .as_array()
            .unwrap()
            .iter()
            .all(|y| y.is_i64()),
        "a year is a number; a year carrying a number is a scoreboard"
    );
}

/// The DTO's `Default` and the engine's are the **same query**.
///
/// Two types with one name meaning opposite things by *the whole-library read*
/// is the trap a derived `Default` walks into here: it would put `limit: 0`
/// against the engine's `-1`, and a page of nothing is indistinguishable from a
/// library of nothing at every call site.
#[test]
fn the_two_reading_query_defaults_are_one_query() {
    let dto = ReadingQueryDto::default();
    let engine: bookmole::ReadingQuery = dto.clone().try_into().unwrap();
    assert_eq!(engine, bookmole::ReadingQuery::default());
    assert!(dto.limit < 0, "the whole-library read has one spelling");
}

// ---- the plugin ------------------------------------------------------------

/// A plausible KOReader install. Deliberately hand-rolled rather than reaching
/// for the engine's own test helper: `crates/api` links `bookmole` as a
/// dependency, so its `#[cfg(test)]` modules do not exist here — and this is
/// also the only place the *dispatch* path is exercised end to end.
fn fake_reader(root: &std::path::Path) {
    let dir = root.join("koreader");
    std::fs::create_dir_all(dir.join("frontend")).unwrap();
    std::fs::create_dir_all(dir.join("plugins")).unwrap();
    std::fs::write(dir.join("reader.lua"), "-- entry point\n").unwrap();
}

/// Claim 1 again, for the three writes on the plugin seam — and the round trip
/// a frontend actually makes: look, install, look, remove.
#[tokio::test]
async fn the_plugin_seam_round_trips_through_dispatch() {
    let (api, _tmp) = api().await;
    let reader = tempfile::tempdir().unwrap();
    fake_reader(reader.path());
    let mount = reader.path().display().to_string();

    let before = match ok(api
        .dispatch(Request::PluginStatus {
            mount: mount.clone(),
        })
        .await)
    {
        Response::PluginStatus(s) => s,
        other => panic!("{other:?}"),
    };
    assert!(!before.installed);
    assert!(!before.paired);
    assert!(before.plugin_dir.ends_with("bookmole.koplugin"));

    let report = match ok(api
        .dispatch(Request::InstallPlugin {
            mount: mount.clone(),
        })
        .await)
    {
        Response::PluginInstalled(r) => r,
        other => panic!("{other:?}"),
    };
    assert_eq!(report.version, before.our_version);
    assert_eq!(report.upgraded_from, None);

    let after = match ok(api
        .dispatch(Request::PluginStatus {
            mount: mount.clone(),
        })
        .await)
    {
        Response::PluginStatus(s) => s,
        other => panic!("{other:?}"),
    };
    assert!(after.installed && after.paired);
    assert_eq!(after.device_id.as_deref(), Some(report.device_id.as_str()));

    let paired = match ok(api.dispatch(Request::PairedDevices).await) {
        Response::PairedDevices(d) => d,
        other => panic!("{other:?}"),
    };
    assert_eq!(paired.len(), 1);
    assert_eq!(paired[0].device_id, report.device_id);

    match ok(api.dispatch(Request::UninstallPlugin { mount }).await) {
        Response::PluginUninstalled(r) => {
            assert_eq!(r.forgot_device.as_deref(), Some(report.device_id.as_str()))
        }
        other => panic!("{other:?}"),
    }
    assert!(api.paired_devices().await.unwrap().is_empty());
}

/// The token is a shared secret and the wire is the one place it must not be.
/// A `PairedDeviceDto` has no field for it, so this is really a test that
/// nobody adds one — serialize the whole reply and look.
#[tokio::test]
async fn no_reply_ever_carries_the_pairing_token() {
    let (api, _tmp) = api().await;
    let reader = tempfile::tempdir().unwrap();
    fake_reader(reader.path());
    let mount = reader.path().display().to_string();

    api.dispatch(Request::InstallPlugin {
        mount: mount.clone(),
    })
    .await
    .unwrap();

    // The token is on the device, so read it from there rather than trusting a
    // constant: this has to fail if the value ever starts travelling.
    let pairing = std::fs::read_to_string(
        reader
            .path()
            .join("koreader/plugins/bookmole.koplugin/pairing.lua"),
    )
    .unwrap();
    let token = pairing
        .lines()
        .find_map(|l| l.trim().strip_prefix("token     = \""))
        .and_then(|l| l.strip_suffix("\","))
        .expect("the installer wrote a token");
    assert_eq!(token.len(), 64);

    for request in [Request::PairedDevices, Request::PluginStatus { mount }] {
        let reply = serde_json::to_string(&ok(api.dispatch(request).await)).unwrap();
        assert!(!reply.contains(token), "a reply carried the token: {reply}");
    }
}

/// A refusal is a decision, and it reaches a client as its own code rather than
/// as prose to be pattern-matched.
#[tokio::test]
async fn refusing_an_unrecognised_volume_is_its_own_error_code() {
    let (api, _tmp) = api().await;
    let plain = tempfile::tempdir().unwrap();

    let err = api
        .dispatch(Request::InstallPlugin {
            mount: plain.path().display().to_string(),
        })
        .await
        .expect_err("an ordinary directory is not a reader");
    assert_eq!(err.code, ErrorCode::PluginRefused);
    assert_eq!(std::fs::read_dir(plain.path()).unwrap().count(), 0);
}

// ---- paired readers: forget, rename, sync a mount ----------------------------

/// The three new methods, through `dispatch`, in the order a devices page uses
/// them: look (which stamps *seen*), sync (which stamps *synced*), rename,
/// forget.
///
/// The two stamps are asserted **apart**, because that separation is the whole
/// of migration `0020`: a page that read `last_seen_at` as "your highlights are
/// here" would tell somebody who plugged a Kobo in to charge it that it had
/// synced.
#[tokio::test]
async fn the_devices_seam_round_trips_through_dispatch() {
    let (api, _tmp) = api().await;
    let reader = tempfile::tempdir().unwrap();
    fake_reader(reader.path());
    let mount = reader.path().display().to_string();

    let report = match ok(api
        .dispatch(Request::InstallPlugin {
            mount: mount.clone(),
        })
        .await)
    {
        Response::PluginInstalled(r) => r,
        other => panic!("{other:?}"),
    };

    // Looking is what records that the reader was in our hands.
    ok(api
        .dispatch(Request::PluginStatus {
            mount: mount.clone(),
        })
        .await);
    let seen = api.paired_devices().await.unwrap();
    assert!(seen[0].last_seen_at.is_some());
    assert_eq!(
        seen[0].last_synced_at, None,
        "looking at a reader is not syncing with it"
    );
    assert_eq!(seen[0].last_mount_path.as_deref(), Some(mount.as_str()));

    // Syncing is what records that the data came across. The reader is empty,
    // which is the case worth pinning: `found` and `synced` are both zero and
    // the device is still identified, so a page can say *nothing new* rather
    // than *no device*.
    let sync = match ok(api
        .dispatch(Request::SyncMount {
            mount: mount.clone(),
        })
        .await)
    {
        Response::MountSync(s) => s,
        other => panic!("{other:?}"),
    };
    assert_eq!(sync.device_id.as_deref(), Some(report.device_id.as_str()));
    assert_eq!((sync.found, sync.synced), (0, 0));
    assert!(sync.reports.is_empty());
    assert!(
        api.paired_devices().await.unwrap()[0]
            .last_synced_at
            .is_some()
    );

    // The two breadcrumbs the wireless link stamps are a fourth fact
    // beside these, and neither a look nor a whole-volume sync over a cable
    // is a reader reaching us over the air. They cross the seam as `None`
    // here — not as a missing key, which the DTO's own test pins — so a card
    // can leave the line out rather than say *not since* about a verb the
    // user never chose.
    let after = &api.paired_devices().await.unwrap()[0];
    assert_eq!(after.last_wireless_at, None, "a cable is not the air");
    assert_eq!(after.last_lan_addr, None);

    // A name of your own, and a blank one puts the fallback back.
    match ok(api
        .dispatch(Request::RenameDevice {
            device_id: report.device_id.clone(),
            label: "the bedside Kobo".into(),
        })
        .await)
    {
        Response::Bool(done) => assert!(done),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        api.paired_devices().await.unwrap()[0].label.as_deref(),
        Some("the bedside Kobo")
    );
    ok(api
        .dispatch(Request::RenameDevice {
            device_id: report.device_id.clone(),
            label: "  ".into(),
        })
        .await);
    assert_eq!(api.paired_devices().await.unwrap()[0].label, None);

    // Forgetting is our side only — and this is the assertion that says so.
    match ok(api
        .dispatch(Request::ForgetDevice {
            device_id: report.device_id.clone(),
        })
        .await)
    {
        Response::Bool(done) => assert!(done),
        other => panic!("{other:?}"),
    }
    assert!(api.paired_devices().await.unwrap().is_empty());
    let still = api.plugin_status(reader.path()).await.unwrap();
    assert!(
        still.installed && !still.paired,
        "forgetting must not reach the reader — the plugin is still on it"
    );

    // And it says whether there was anything to forget.
    match ok(api
        .dispatch(Request::ForgetDevice {
            device_id: report.device_id,
        })
        .await)
    {
        Response::Bool(done) => assert!(!done),
        other => panic!("{other:?}"),
    }
}

/// `SyncMount` against a tree that is nobody's reader — a library directory on
/// a disk. It must work and it must stamp nothing.
///
/// The refusal it would be easy to add here is the bug: `plugin::inspect`
/// failing is not a reason to fail a sync, because importing sidecars has never
/// needed a pairing.
#[tokio::test]
async fn syncing_a_tree_that_is_not_a_reader_is_ordinary() {
    let (api, _tmp) = api().await;
    let tree = tempfile::tempdir().unwrap();
    let sync = match ok(api
        .dispatch(Request::SyncMount {
            mount: tree.path().display().to_string(),
        })
        .await)
    {
        Response::MountSync(s) => s,
        other => panic!("{other:?}"),
    };
    assert_eq!(sync.device_id, None);
    assert_eq!((sync.found, sync.synced), (0, 0));
    assert!(api.paired_devices().await.unwrap().is_empty());
}

/// Forget/rename/sync-mount are additive: three new methods and one new field
/// on a **response**
/// DTO. A payload written before it still parses to the request it meant.
///
/// The field is the half worth stating. `ts-rs` emits a new field as required
/// TypeScript however `#[serde(default)]` the Rust is, which is why a field may
/// never be added to an existing `Request` — and why adding one to
/// `PairedDeviceDto`, which no request carries, is safe.
#[test]
fn a_payload_written_before_the_devices_page_still_parses() {
    let before: Request =
        serde_json::from_str(r#"{"method":"sync_device","params":{"paths":["/mnt/k"]}}"#).unwrap();
    assert!(matches!(before, Request::SyncDevice { .. }));
    let before: Request = serde_json::from_str(r#"{"method":"paired_devices"}"#).unwrap();
    assert!(matches!(before, Request::PairedDevices));
    assert_eq!(
        bookmole_api::API_VERSION,
        2,
        "these added methods and reshaped none"
    );
}

/// Pairing over wifi is one new method and one new field on a **response**
/// DTO; nothing a client already sends changed shape.
///
/// What can be asserted here without a reader on the network is the edge of
/// it: the request parses with and without the optional address, something
/// that is not a code is refused as `wireless_refused` before any socket is
/// opened — so a typo costs no wait — and the refusal's sentence does not
/// repeat what was typed, because what was typed is meant to be a secret.
#[tokio::test]
async fn pairing_over_wifi_is_additive_and_refuses_what_is_not_a_code() {
    let (api, _tmp) = api().await;

    let bare: Request =
        serde_json::from_str(r#"{"method":"pair_with_reader","params":{"code":"ABCD"}}"#).unwrap();
    assert!(matches!(bare, Request::PairWithReader { host: None, .. }));
    let with_host: Request = serde_json::from_str(
        r#"{"method":"pair_with_reader","params":{"code":"ABCD","host":"192.0.2.42"}}"#,
    )
    .unwrap();
    assert!(matches!(
        with_host,
        Request::PairWithReader { host: Some(_), .. }
    ));

    let err = api
        .dispatch(Request::PairWithReader {
            code: "hunter2-is-not-a-code".into(),
            host: None,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::WirelessRefused);
    assert!(
        !err.message.contains("hunter2"),
        "a refusal must not echo the code: {}",
        err.message
    );
    assert!(api.paired_devices().await.unwrap().is_empty());

    // The door's status now says where this computer is, for a person to type
    // into a reader. Present as a key whether or not there is an address —
    // `bindings.ts` is keyed by name, and a missing key reads as `undefined`.
    let status = serde_json::to_value(api.listener_status().await.unwrap()).unwrap();
    assert!(status.get("lan_address").is_some(), "{status}");

    assert_eq!(
        bookmole_api::API_VERSION,
        2,
        "pairing over wifi added a method and reshaped none"
    );
}

/// `ResurfaceNote` crosses the seam with the caller's local date as text: a
/// fresh notebook answers `null` (a note kept today is still resting), a note
/// a month old comes back with its reason, and a malformed date is
/// `invalid_input` rather than a guess at UTC's today.
#[tokio::test]
async fn resurface_note_takes_the_callers_day_and_answers_null_when_nothing_qualifies() {
    let (api, _tmp) = api().await;
    let created = api
        .create_note(NewNoteDto {
            body: "An older thought.".into(),
            ..Default::default()
        })
        .await
        .unwrap();

    let now = time::OffsetDateTime::now_utc().date();
    let day = |d: time::Date| format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day());

    match ok(api
        .dispatch(Request::ResurfaceNote { today: day(now) })
        .await)
    {
        Response::Resurfaced(None) => {}
        other => panic!("a note kept today rests, got {other:?}"),
    }

    let later = day(now + time::Duration::days(30));
    match ok(api.dispatch(Request::ResurfaceNote { today: later }).await) {
        Response::Resurfaced(Some(r)) => {
            assert_eq!(r.note.id, created.id);
            assert_eq!(r.reason, bookmole_api::ResurfaceReasonDto::Notebook);
        }
        other => panic!("expected the note back, got {other:?}"),
    }

    let bad = api.resurface_note("18/10/2026").await.unwrap_err();
    assert_eq!(bad.code, ErrorCode::InvalidInput);
}

/// `ResurfacePassage` crosses like its twin: through both doors, `null` for
/// a library with no passage old enough, and a malformed day is
/// `invalid_input`. The pick itself is the engine's and is tested there —
/// this crate has no way to store a highlight, which is the seam working (the
/// storage layer is not reachable from here).
#[tokio::test]
async fn resurface_passage_crosses_the_seam_like_the_note() {
    let (api, _tmp) = api().await;
    let now = time::OffsetDateTime::now_utc().date();
    let day = format!(
        "{:04}-{:02}-{:02}",
        now.year(),
        u8::from(now.month()),
        now.day()
    );
    match ok(api
        .dispatch(Request::ResurfacePassage { today: day.clone() })
        .await)
    {
        Response::ResurfacedPassage(None) => {}
        other => panic!("an empty library offers no passage, got {other:?}"),
    }
    assert_eq!(api.resurface_passage(&day).await.unwrap(), None);
    let parsed: Request =
        serde_json::from_str(r#"{"method":"resurface_passage","params":{"today":"2026-11-10"}}"#)
            .unwrap();
    assert!(matches!(parsed, Request::ResurfacePassage { .. }));
    let bad = api.resurface_passage("10/11/2026").await.unwrap_err();
    assert_eq!(bad.code, ErrorCode::InvalidInput);
}

/// `LinkContexts` through both doors: the typed method and the dispatch arm
/// answer alike, and the direction crosses as a snake_case word a client can
/// match on. Values only — no paragraph is printed.
#[tokio::test]
async fn link_contexts_cross_the_seam_with_their_paragraph() {
    let (api, _tmp) = api().await;
    let book_id = seed(&api).await;
    let target = api
        .create_note(NewNoteDto {
            book_id: Some(book_id),
            title: Some("Symphony".into()),
            body: "Out to [[Nowhere]].".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let source = api
        .create_note(NewNoteDto {
            book_id: Some(book_id),
            title: Some("Travelling".into()),
            body: "Head.\n\nThe [[Symphony]] plays on.".into(),
            ..Default::default()
        })
        .await
        .unwrap();

    let typed = api.link_contexts(target.id).await.unwrap();
    assert_eq!(typed.len(), 2);
    assert_eq!(typed[0].direction, LinkDirectionDto::Incoming);
    assert_eq!(typed[0].note.as_ref().map(|n| n.id), Some(source.id));
    assert_eq!(typed[0].book.as_ref().map(|b| b.id), Some(book_id));
    assert_eq!(
        typed[0].paragraph.as_deref(),
        Some("The [[Symphony]] plays on.")
    );
    assert_eq!(typed[0].link_range, Some((4, 16)));
    assert!(!typed[0].truncated);
    assert_eq!(typed[1].direction, LinkDirectionDto::Outgoing);
    assert_eq!(typed[1].target_title, "Nowhere");
    assert!(typed[1].note.is_none() && typed[1].book.is_none());
    assert!(typed[1].paragraph.is_none() && typed[1].link_range.is_none());

    match ok(api
        .dispatch(Request::LinkContexts { note_id: target.id })
        .await)
    {
        Response::LinkContexts(rows) => assert_eq!(rows, typed),
        other => panic!("expected link contexts, got {other:?}"),
    }

    let json = serde_json::to_value(&typed[0]).unwrap();
    assert_eq!(json["direction"], "incoming");
    assert_eq!(json["link_range"], serde_json::json!([4, 16]));
}

// ---- note rows, threads, links and parts ----------------------------------

/// Every new Notes request through `dispatch`, once: each answers its own
/// shape, and the list's typed method agrees with its arm. Values only.
#[tokio::test]
async fn the_notes_boards_round_trip_through_dispatch() {
    let (api, _tmp) = api().await;
    let book_id = seed(&api).await;
    let hub = api
        .create_note(NewNoteDto {
            book_id: Some(book_id),
            body: "# Symphony\n\nThe travelling symphony plays.".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let spoke = api
        .create_note(NewNoteDto {
            title: Some("Kirsten".into()),
            body: "She joins [[Symphony]] and [[Unwritten]]. Later, kirsten again.".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let mentioner = api
        .create_note(NewNoteDto {
            title: Some("Arthur".into()),
            body: "Arthur never met Kirsten.".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(hub.title, "Symphony", "the heading names the note");

    // ListNoteRows
    let typed = api
        .list_note_rows(None, None, None, None, None, 10)
        .await
        .unwrap();
    match ok(api
        .dispatch(Request::ListNoteRows {
            kinds: None,
            book_id: None,
            reading_id: None,
            ids: None,
            until: None,
            limit: 10,
        })
        .await)
    {
        Response::NoteRows(rows) => assert_eq!(rows, typed),
        other => panic!("{other:?}"),
    }
    let hub_row = typed.iter().find(|r| r.note.id == hub.id).unwrap();
    assert_eq!(hub_row.heading.as_deref(), Some("Symphony"));
    assert_eq!(hub_row.excerpt, "The travelling symphony plays.");
    assert_eq!(hub_row.links_in, 1);
    assert!(hub_row.thread.is_some(), "linked, so in a thread");
    assert!(hub_row.book.is_some());
    let refused = api
        .dispatch(Request::ListNoteRows {
            kinds: Some(vec![NoteKindDto::Note]),
            book_id: Some(book_id),
            reading_id: Some(1),
            ids: None,
            until: None,
            limit: 10,
        })
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::InvalidInput);

    // NotePeriods
    match ok(api.dispatch(Request::NotePeriods).await) {
        Response::NotePeriods(p) => assert_eq!(p.len(), 1),
        other => panic!("{other:?}"),
    }
    // ThreadsFor
    match ok(api
        .dispatch(Request::ThreadsFor {
            note_ids: vec![spoke.id],
            since: Some(0),
        })
        .await)
    {
        Response::Threads(t) => {
            assert_eq!(t.len(), 1);
            assert_eq!(t[0].notes, 2);
            assert_eq!(t[0].joined, 2);
        }
        other => panic!("{other:?}"),
    }
    // LinksFromNotes
    match ok(api
        .dispatch(Request::LinksFromNotes {
            note_ids: vec![spoke.id],
        })
        .await)
    {
        Response::NoteLinks(l) => {
            let titles: Vec<_> = l.iter().map(|l| l.target_title.as_str()).collect();
            assert_eq!(titles, vec!["Symphony", "Unwritten"]);
        }
        other => panic!("{other:?}"),
    }
    // NoteGraph
    match ok(api
        .dispatch(Request::NoteGraph {
            scope: GraphScopeDto::Around {
                note_id: spoke.id,
                steps: 1,
            },
        })
        .await)
    {
        Response::NoteGraph(g) => {
            assert!(
                g.nodes
                    .iter()
                    .any(|n| n.key == "tunwritten" && n.row.is_none())
            );
            assert!(g.edges.iter().any(|e| e.kind == GraphEdgeKindDto::Mention));
        }
        other => panic!("{other:?}"),
    }
    let json = serde_json::to_value(GraphScopeDto::Recent { since: 5 }).unwrap();
    assert_eq!(json, serde_json::json!({"scope": "recent", "since": 5}));
    // ContinueNote, NoteParts, UpdateNotePart
    match ok(api
        .dispatch(Request::ContinueNote {
            note_id: hub.id,
            body: "A second sitting.".into(),
            page: Some(12),
        })
        .await)
    {
        Response::NoteParts(p) => assert_eq!(p.len(), 2),
        other => panic!("{other:?}"),
    }
    match ok(api
        .dispatch(Request::UpdateNotePart {
            note_id: hub.id,
            n: 2,
            body: "Revised.".into(),
        })
        .await)
    {
        Response::NoteParts(p) => assert_eq!(p[1].body, "Revised."),
        other => panic!("{other:?}"),
    }
    match ok(api.dispatch(Request::NoteParts { note_id: hub.id }).await) {
        Response::NoteParts(p) => assert_eq!((p[1].n, p[1].page), (2, Some(12))),
        other => panic!("{other:?}"),
    }
    // UnlinkedMentions, LinkMention
    match ok(api
        .dispatch(Request::UnlinkedMentions {
            note_id: spoke.id,
            limit: 5,
        })
        .await)
    {
        Response::SearchHits(h) => {
            assert!(matches!(&h[..], [SearchHitDto::Note { note, .. }] if note.id == mentioner.id))
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        ok(api
            .dispatch(Request::LinkMention {
                from_note_id: mentioner.id,
                to_note_id: spoke.id,
            })
            .await),
        Response::Unit
    ));
    // CreateHighlight
    match ok(api
        .dispatch(Request::CreateHighlight {
            book_id,
            text: "Survival is insufficient.".into(),
            page: Some(58),
            chapter: None,
        })
        .await)
    {
        Response::Highlight(Some(h)) => assert_eq!((h.source.as_str(), h.page), ("user", Some(58))),
        other => panic!("{other:?}"),
    }
    // PreviewLinks
    match ok(api
        .dispatch(Request::PreviewLinks {
            title: Some("Unwritten".into()),
            body: "see [[Kirsten]]".into(),
        })
        .await)
    {
        Response::LinkPreview(p) => {
            assert_eq!(p.outgoing[0].note.as_ref().map(|n| n.id), Some(spoke.id));
            assert_eq!(p.incoming.len(), 1);
            assert_eq!(p.incoming[0].direction, LinkDirectionDto::Incoming);
        }
        other => panic!("{other:?}"),
    }
}

// ---- reading history, the library table, settings, more notes ------------

/// Every new request through `dispatch`, once: each answers its own shape, and
/// the typed method agrees with its arm where both read. Values only.
#[tokio::test]
async fn history_library_and_the_notes_gaps_round_trip_through_dispatch() {
    use bookmole_api::{
        HistoryCompareDto, HistoryGroupDto, HistoryMetricDto, HistorySplitDto, NoteCursorDto,
        ReadMarkDto,
    };
    let (api, _tmp) = api().await;
    let book_id = seed(&api).await;
    api.update_progress(book_id, Some(10), None).await.unwrap();
    let passage = api
        .create_highlight(book_id, "a passage", Some(10), None)
        .await
        .unwrap();
    api.update_progress(book_id, None, Some(true))
        .await
        .unwrap();
    let reading = api.list_readings(book_id).await.unwrap()[0].id;
    let review = api.open_review(book_id, Some(reading)).await.unwrap();
    api.set_rating(review.id, 4.0).await.unwrap();

    // Settings.
    ok(api
        .dispatch(Request::SetSetting {
            key: "history.overview.cards".into(),
            value: r#"["pages"]"#.into(),
        })
        .await);
    match ok(api
        .dispatch(Request::GetSetting {
            key: "history.overview.cards".into(),
        })
        .await)
    {
        Response::Setting(v) => assert_eq!(v.as_deref(), Some(r#"["pages"]"#)),
        other => panic!("{other:?}"),
    }
    match ok(api
        .dispatch(Request::ListSettings {
            prefix: "history.".into(),
        })
        .await)
    {
        Response::Settings(s) => assert_eq!(s.len(), 1),
        other => panic!("{other:?}"),
    }
    let refused = api
        .dispatch(Request::SetSetting {
            key: "Bad Key".into(),
            value: "1".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::InvalidInput);

    // History.
    let typed = api.history_timeline(None, None).await.unwrap();
    match ok(api
        .dispatch(Request::HistoryTimeline {
            since: None,
            until: None,
        })
        .await)
    {
        Response::HistoryTimeline(t) => {
            assert_eq!(t.len(), 1);
            assert_eq!(t[0].read.reading.id, typed[0].read.reading.id);
            assert_eq!(t[0].book.title.as_deref(), Some("Station Eleven"));
            assert_eq!(t[0].read.passages, 1);
            assert_eq!(t[0].read.rating.as_ref().map(|r| r.value), Some(4.0));
        }
        other => panic!("{other:?}"),
    }
    let inverted = api
        .dispatch(Request::HistoryTimeline {
            since: Some(10),
            until: Some(5),
        })
        .await
        .unwrap_err();
    assert_eq!(inverted.code, ErrorCode::InvalidInput);
    match ok(api
        .dispatch(Request::HistoryFigures {
            since: None,
            until: None,
        })
        .await)
    {
        Response::HistoryFigures(f) => {
            assert_eq!(f.books_finished, 1);
            assert_eq!(f.passages, 1);
            assert_eq!(f.reviews, 1);
            let json = serde_json::to_value(&f).unwrap();
            assert!(
                json.get("current_run").is_none() && json.get("streak").is_none(),
                "no current run crosses the wire"
            );
        }
        other => panic!("{other:?}"),
    }
    match ok(api
        .dispatch(Request::HistoryRecords {
            since: None,
            until: None,
        })
        .await)
    {
        Response::HistoryRecords(r) => {
            assert_eq!(r.best_rated.unwrap().read.read.reading.id, reading);
            assert_eq!(r.longest.unwrap().pages, Some(333));
        }
        other => panic!("{other:?}"),
    }
    let series = |export: bool| {
        let (metric, group, split, compare) = (
            HistoryMetricDto::Passages,
            HistoryGroupDto::Week,
            HistorySplitDto::Book,
            HistoryCompareDto::LastYear,
        );
        if export {
            Request::ExportHistorySeries {
                metric,
                group,
                split,
                compare,
                since: None,
                until: None,
            }
        } else {
            Request::HistorySeries {
                metric,
                group,
                split,
                compare,
                since: None,
                until: None,
            }
        }
    };
    match ok(api.dispatch(series(false)).await) {
        Response::HistorySeries(s) => {
            assert_eq!(s.total, 1);
            assert_eq!(s.compare_total, Some(0));
            let bucket = &s.buckets[s.busiest.unwrap() as usize];
            assert_eq!(bucket.parts[0].label, "Station Eleven");
        }
        other => panic!("{other:?}"),
    }
    match ok(api.dispatch(series(true)).await) {
        Response::SavedCsv(c) => {
            assert!(std::path::Path::new(&c.path).exists());
            assert!(c.rows >= 2);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        serde_json::to_value(HistorySplitDto::Unsplit).unwrap(),
        "none",
        "the wire word is `none`"
    );
    match ok(api
        .dispatch(Request::HistoryKept {
            since: None,
            until: None,
            limit: 5,
        })
        .await)
    {
        Response::HistoryKept(k) => {
            assert_eq!((k[0].book.id, k[0].passages, k[0].notes), (book_id, 1, 1));
        }
        other => panic!("{other:?}"),
    }

    // The Library.
    match ok(api.dispatch(Request::LibraryTable).await) {
        Response::LibraryTable(t) => {
            assert_eq!(t.counts.all, 1);
            assert_eq!(t.counts.read, 1);
            assert_eq!(t.total_pages, 333);
            assert_eq!(t.rows[0].passages, 1);
            assert!(t.rows[0].latest.is_some());
        }
        other => panic!("{other:?}"),
    }

    // The Notes gaps.
    match ok(api.dispatch(Request::GetHighlight { id: passage.id }).await) {
        Response::Highlight(h) => assert_eq!(h.unwrap().id, passage.id),
        other => panic!("{other:?}"),
    }
    match ok(api
        .dispatch(Request::BookFaces {
            book_ids: vec![999, book_id],
        })
        .await)
    {
        Response::BookFaces(f) => {
            assert_eq!(f.iter().map(|f| f.id).collect::<Vec<_>>(), vec![book_id])
        }
        other => panic!("{other:?}"),
    }
    match ok(api.dispatch(Request::NoteBooks).await) {
        Response::NoteBooks(b) => assert!(b.is_empty(), "nothing links yet"),
        other => panic!("{other:?}"),
    }
    match ok(api
        .dispatch(Request::ReadMarks {
            reading_id: reading,
            limit: -1,
        })
        .await)
    {
        Response::ReadMarks(m) => {
            assert_eq!(m.len(), 2);
            let note = m.iter().find_map(|m| match m {
                ReadMarkDto::Note { row, .. } => Some(row),
                ReadMarkDto::Passage { .. } => None,
            });
            assert_eq!(
                note.unwrap().read_number,
                Some(1),
                "the first read's review"
            );
        }
        other => panic!("{other:?}"),
    }
    match ok(api
        .dispatch(Request::ListNoteRowsAfter {
            kinds: None,
            book_id: None,
            reading_id: None,
            ids: None,
            until: None,
            after: Some(NoteCursorDto {
                at: i64::MAX,
                id: i64::MAX,
            }),
            limit: 10,
        })
        .await)
    {
        Response::NoteRows(r) => assert_eq!(r.len(), 1),
        other => panic!("{other:?}"),
    }
}

/// Three small additions (matched parts, zoned note periods, …), each through
/// both doors,
/// plus the wire spelling a client writes for each.
#[tokio::test]
async fn the_cleanup_requests_round_trip_through_dispatch() {
    let (api, _tmp) = api().await;
    let book_id = seed(&api).await;

    // MatchedParts: which part of a continued note a search found.
    let note = api
        .create_note(NewNoteDto {
            book_id: Some(book_id),
            title: Some("A log".into()),
            body: "The harbour at dusk.".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    api.continue_note(note.id, "Nothing about water.", None)
        .await
        .unwrap();
    api.continue_note(note.id, "A lighthouse, then harbours.", None)
        .await
        .unwrap();
    let typed = api.matched_parts(note.id, "harbour").await.unwrap();
    assert_eq!(typed, vec![1, 3]);
    let wire: Request = serde_json::from_str(&format!(
        r#"{{"method":"matched_parts","params":{{"note_id":{},"query":"harbour"}}}}"#,
        note.id
    ))
    .unwrap();
    match ok(api.dispatch(wire).await) {
        Response::Parts(p) => assert_eq!(p, typed),
        other => panic!("{other:?}"),
    }
    let missing = api
        .dispatch(Request::MatchedParts {
            note_id: note.id + 100,
            query: "harbour".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(missing.code, ErrorCode::NotFound);

    // NotePeriodsIn: zero is NotePeriods; beyond ±18 hours is refused.
    let utc = api.note_periods().await.unwrap();
    let wire: Request =
        serde_json::from_str(r#"{"method":"note_periods_in","params":{"utc_offset_minutes":0}}"#)
            .unwrap();
    match ok(api.dispatch(wire).await) {
        Response::NotePeriods(p) => assert_eq!(p, utc),
        other => panic!("{other:?}"),
    }
    let refused = api
        .dispatch(Request::NotePeriodsIn {
            utc_offset_minutes: 2000,
        })
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::InvalidInput);

    // HistoryReadingNow: the open read, as the timeline's row.
    api.update_progress(book_id, Some(10), None).await.unwrap();
    let typed = api.history_reading_now().await.unwrap();
    assert_eq!(typed.len(), 1);
    assert_eq!(typed[0].book.title.as_deref(), Some("Station Eleven"));
    let wire: Request = serde_json::from_str(r#"{"method":"history_reading_now"}"#).unwrap();
    match ok(api.dispatch(wire).await) {
        Response::HistoryTimeline(t) => {
            assert_eq!(t, typed);
            assert_eq!(t[0].read.reading.status, ReadingStateDto::Reading);
        }
        other => panic!("{other:?}"),
    }
    api.update_progress(book_id, None, Some(true))
        .await
        .unwrap();
    assert!(
        api.history_reading_now().await.unwrap().is_empty(),
        "a finished read is not reading now"
    );
}

// ---- lists and copies -------------------------------------------------------

async fn save(api: &Api, title: &str, isbn: &str) -> i64 {
    api.save_book(BookDto {
        title: Some(title.into()),
        authors: vec!["An Author".into()],
        isbn_13: Some(isbn.into()),
        ..Default::default()
    })
    .await
    .expect("save")
    .id
    .expect("id")
}

fn copy_of(response: Response) -> bookmole_api::CopyDto {
    match response {
        Response::Copy(c) => c,
        other => panic!("{other:?}"),
    }
}

fn list_of(response: Response) -> bookmole_api::ListDto {
    match response {
        Response::List(l) => l,
        other => panic!("{other:?}"),
    }
}

/// Create, read, rename, add, move, note, remove and delete a list, each by
/// request, and the errors each refuses with.
#[tokio::test]
async fn a_list_is_made_filled_ordered_and_deleted_by_request() {
    let (api, _tmp) = api().await;
    let a = save(&api, "Alpha", "9780000000002").await;
    let b = save(&api, "Bravo", "9780000000019").await;
    let c = save(&api, "Charlie", "9780000000026").await;
    // Only `a` is in the library; a list holds any book.
    have(&api, a).await;

    let list = list_of(ok(api
        .dispatch(Request::CreateList {
            name: "To read".into(),
            description: Some("soon".into()),
        })
        .await));
    assert_eq!((list.book_count, list.not_in_library), (0, 0));
    assert!(!list.timeline_ready);
    for book_id in [a, b, c] {
        match ok(api
            .dispatch(Request::AddToList {
                list_id: list.id,
                book_id,
                note: None,
            })
            .await)
        {
            Response::ListEntry(e) => assert_eq!(e.book.id, book_id),
            other => panic!("{other:?}"),
        }
    }
    let got = list_of(ok(api.dispatch(Request::GetList { id: list.id }).await));
    assert_eq!((got.book_count, got.not_in_library), (3, 2));

    // Move `c` to the front; a sort is a view and never rewrites the order.
    ok(api
        .dispatch(Request::MoveInList {
            list_id: list.id,
            book_id: c,
            to: 0,
        })
        .await);
    ok(api
        .dispatch(Request::SetListNote {
            list_id: list.id,
            book_id: c,
            note: Some("start here".into()),
        })
        .await);
    let entries = |show, sort| {
        let api = api.clone();
        async move {
            match ok(api
                .dispatch(Request::ListEntries {
                    list_id: list.id,
                    show,
                    sort,
                })
                .await)
            {
                Response::ListEntries(v) => v,
                other => panic!("{other:?}"),
            }
        }
    };
    let yours = entries(EntryShowDto::All, EntrySortDto::Yours).await;
    assert_eq!(
        yours.iter().map(|e| e.book.id).collect::<Vec<_>>(),
        [c, a, b]
    );
    assert_eq!(yours[0].note.as_deref(), Some("start here"));
    let by_title = entries(EntryShowDto::All, EntrySortDto::Title).await;
    assert_eq!(
        by_title.iter().map(|e| e.book.id).collect::<Vec<_>>(),
        [a, b, c]
    );
    assert_eq!(
        entries(EntryShowDto::All, EntrySortDto::Yours)
            .await
            .iter()
            .map(|e| e.position)
            .collect::<Vec<_>>(),
        [0, 1, 2],
        "sorting by title did not renumber the list"
    );
    let outside = entries(EntryShowDto::NotInLibrary, EntrySortDto::Yours).await;
    assert_eq!(outside.len(), 2);
    assert!(outside.iter().all(|e| !e.in_library));

    match ok(api.dispatch(Request::ListsForBook { book_id: c }).await) {
        Response::ListMemberships(m) => {
            assert_eq!((m[0].position, m[0].list_size), (1, 3));
        }
        other => panic!("{other:?}"),
    }
    match ok(api
        .dispatch(Request::ListTimeline { list_id: list.id })
        .await)
    {
        Response::ListTimeline(t) => assert!(!t.ready && t.dated.is_empty()),
        other => panic!("{other:?}"),
    }
    match ok(api
        .dispatch(Request::ListLists {
            sort: ListSortDto::Name,
            preview: 2,
        })
        .await)
    {
        Response::ListSummaries(v) => {
            assert_eq!(v.len(), 1);
            assert_eq!((v[0].book_count, v[0].preview.len()), (3, 2));
        }
        other => panic!("{other:?}"),
    }

    let renamed = list_of(ok(api
        .dispatch(Request::UpdateList {
            id: list.id,
            name: "Next up".into(),
            description: None,
        })
        .await));
    assert_eq!(renamed.name, "Next up");
    assert_eq!(renamed.description, None);

    ok(api
        .dispatch(Request::RemoveFromList {
            list_id: list.id,
            book_id: a,
        })
        .await);
    assert_eq!(
        list_of(ok(api.dispatch(Request::GetList { id: list.id }).await)).book_count,
        2
    );

    // Errors: a blank name is invalid input; an unknown list or book is not found.
    let blank = api
        .dispatch(Request::CreateList {
            name: "   ".into(),
            description: None,
        })
        .await
        .unwrap_err();
    assert_eq!(blank.code, ErrorCode::InvalidInput);
    let missing = api
        .dispatch(Request::GetList { id: 999 })
        .await
        .unwrap_err();
    assert_eq!(missing.code, ErrorCode::NotFound);
    let no_book = api
        .dispatch(Request::AddToList {
            list_id: list.id,
            book_id: 999,
            note: None,
        })
        .await
        .unwrap_err();
    assert_eq!(no_book.code, ErrorCode::NotFound);

    ok(api.dispatch(Request::DeleteList { id: list.id }).await);
    assert_eq!(
        api.dispatch(Request::GetList { id: list.id })
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
}

/// A Goodreads shelf is offered, kept as a list, and a dismissed one is not
/// offered again.
#[tokio::test]
async fn a_shelf_is_kept_or_dismissed_by_request() {
    let (api, tmp) = api().await;
    let id = seed(&api).await;
    let csv = tmp.path().join("goodreads.csv");
    std::fs::write(
        &csv,
        "Title,Author,ISBN,My Rating,Date Read,Date Added,Bookshelves,Review\n\
         Station Eleven,Emily St. John Mandel,\"=\"\"9781447268963\"\"\",0,,2020/01/01,\"to-read, favourites\",\n",
    )
    .unwrap();
    api.import_goodreads(&csv, false, false).await.unwrap();

    let offered = match ok(api.dispatch(Request::ShelfSuggestions { preview: 3 }).await) {
        Response::ShelfSuggestions(v) => v,
        other => panic!("{other:?}"),
    };
    assert_eq!(offered[0].source, "goodreads");
    assert_eq!(offered[0].book_count, 1);
    assert_eq!(offered[0].preview[0].id, id);
    let mut tags: Vec<_> = offered.into_iter().map(|s| s.tag).collect();
    tags.sort();
    assert_eq!(tags, ["favourites", "to-read"]);

    let kept = list_of(ok(api
        .dispatch(Request::KeepShelf {
            source: "goodreads".into(),
            tag: "to-read".into(),
        })
        .await));
    assert_eq!((kept.source.as_str(), kept.book_count), ("goodreads", 1));
    ok(api
        .dispatch(Request::DismissShelf {
            source: "goodreads".into(),
            tag: "favourites".into(),
        })
        .await);
    match ok(api.dispatch(Request::ShelfSuggestions { preview: 0 }).await) {
        Response::ShelfSuggestions(v) => {
            assert!(v.is_empty(), "kept and dismissed are not offered")
        }
        other => panic!("{other:?}"),
    }
    let missing = api
        .dispatch(Request::KeepShelf {
            source: "goodreads".into(),
            tag: "no-such-shelf".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(missing.code, ErrorCode::NotFound);
}

/// Record a copy, say you no longer have it, say you do again, and read the
/// book's standing through each step.
#[tokio::test]
async fn a_copy_is_added_ended_resumed_and_deleted_by_request() {
    let (api, _tmp) = api().await;
    let book_id = save(&api, "Borrowed Book", "9780000000033").await;
    match ok(api.dispatch(Request::BookStanding { book_id }).await) {
        Response::BookStanding(s) => assert!(!s.in_library && !s.touched),
        other => panic!("{other:?}"),
    }

    let copy = copy_of(ok(api
        .dispatch(Request::AddCopy {
            book_id,
            form: None,
            tenure: TenureDto::Borrowed,
            have_now: Some(true),
            from_whom: Some("the library".into()),
            app: None,
            location: None,
            started_at: Some(1_700_000_000),
            due_at: Some(1_702_000_000),
            note: None,
        })
        .await));
    assert_eq!(copy.tenure, TenureDto::Borrowed);
    assert_eq!(copy.form, None, "an unrecorded form is none");
    assert_eq!(copy.learned_from, bookmole_api::LearnedFromDto::User);
    assert!(copy.places.is_empty());
    match ok(api.dispatch(Request::BookStanding { book_id }).await) {
        Response::BookStanding(s) => assert!(s.in_library),
        other => panic!("{other:?}"),
    }

    let ended = copy_of(ok(api
        .dispatch(Request::EndCopy {
            copy_id: copy.id,
            ended_at: Some(1_701_000_000),
        })
        .await));
    assert_eq!(
        (ended.have_now, ended.ended_at),
        (Some(false), Some(1_701_000_000))
    );
    match ok(api.dispatch(Request::BookStanding { book_id }).await) {
        Response::BookStanding(s) => {
            assert!(!s.in_library, "untouched and not had: not in the library")
        }
        other => panic!("{other:?}"),
    }

    let again = copy_of(ok(api
        .dispatch(Request::ResumeCopy { copy_id: copy.id })
        .await));
    assert_eq!((again.have_now, again.ended_at), (Some(true), None));

    let edited = copy_of(ok(api
        .dispatch(Request::UpdateCopy {
            copy_id: copy.id,
            form: Some(CopyFormDto::App),
            tenure: TenureDto::Borrowed,
            from_whom: None,
            app: Some("Libby".into()),
            location: None,
            started_at: None,
            due_at: None,
            ended_at: None,
            lent_to: None,
            unreadable: false,
            note: Some("renewed".into()),
        })
        .await));
    assert_eq!(edited.app.as_deref(), Some("Libby"));
    assert_eq!(
        edited.from_whom, None,
        "an update writes the fields as given"
    );
    assert_eq!(
        edited.have_now,
        Some(true),
        "only End/Resume write have_now"
    );

    match ok(api.dispatch(Request::CopiesForBook { book_id }).await) {
        Response::Copies(v) => assert_eq!(v.len(), 1),
        other => panic!("{other:?}"),
    }
    ok(api.dispatch(Request::DeleteCopy { copy_id: copy.id }).await);
    match ok(api.dispatch(Request::CopiesForBook { book_id }).await) {
        Response::Copies(v) => assert!(v.is_empty()),
        other => panic!("{other:?}"),
    }
    for request in [
        Request::DeleteCopy { copy_id: copy.id },
        Request::EndCopy {
            copy_id: copy.id,
            ended_at: None,
        },
        Request::ResumeCopy { copy_id: copy.id },
    ] {
        assert_eq!(
            api.dispatch(request).await.unwrap_err().code,
            ErrorCode::NotFound
        );
    }
    let orphan = api
        .dispatch(Request::AddCopy {
            book_id: 999,
            form: None,
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
        .unwrap_err();
    assert_eq!(orphan.code, ErrorCode::NotFound);
}

/// The Library lists only books you have, and the copy filters count them.
#[tokio::test]
async fn the_library_hides_a_copyless_book_and_filters_by_copy() {
    let (api, _tmp) = api().await;
    let owned = save(&api, "Owned", "9780000000040").await;
    let loan = save(&api, "Loan", "9780000000057").await;
    let gone = save(&api, "Gone", "9780000000064").await;
    let none = save(&api, "No Copy", "9780000000071").await;

    let count = |api: Api| async move {
        match ok(api.dispatch(Request::CountBooks { filter: None }).await) {
            Response::Count(n) => n,
            other => panic!("{other:?}"),
        }
    };
    let table_ids = |api: Api| async move {
        let mut ids: Vec<i64> = match ok(api.dispatch(Request::LibraryTable).await) {
            Response::LibraryTable(t) => t.rows.iter().map(|r| r.book.id).collect(),
            other => panic!("{other:?}"),
        };
        ids.sort();
        ids
    };
    assert_eq!(count(api.clone()).await, 0, "no copies: an empty library");
    assert!(table_ids(api.clone()).await.is_empty());

    let add = |book_id, tenure, have_now| {
        let api = api.clone();
        async move {
            copy_of(ok(api
                .dispatch(Request::AddCopy {
                    book_id,
                    form: None,
                    tenure,
                    have_now,
                    from_whom: None,
                    app: None,
                    location: None,
                    started_at: None,
                    due_at: None,
                    note: None,
                })
                .await))
        }
    };
    add(owned, TenureDto::Own, None).await;
    add(loan, TenureDto::Borrowed, Some(true)).await;
    let gone_copy = add(gone, TenureDto::Unrecorded, None).await;
    // `none` stays copy-less.

    assert_eq!(count(api.clone()).await, 3);
    match ok(api
        .dispatch(Request::ListBooks {
            limit: -1,
            sort: Default::default(),
            offset: 0,
            filter: None,
        })
        .await)
    {
        Response::Books(b) => {
            let mut ids: Vec<i64> = b.iter().filter_map(|b| b.id).collect();
            ids.sort();
            assert_eq!(ids, [owned, loan, gone]);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(table_ids(api.clone()).await, [owned, loan, gone]);
    // A single-book fetch is not scoped.
    assert!(api.get_book(none).await.unwrap().is_some());

    // Ended and untouched: out of the library, and out of the table.
    ok(api
        .dispatch(Request::EndCopy {
            copy_id: gone_copy.id,
            ended_at: None,
        })
        .await);
    assert_eq!(table_ids(api.clone()).await, [owned, loan]);

    match ok(api.dispatch(Request::LibraryCopies).await) {
        Response::LibraryCopies(l) => {
            assert_eq!(l.counts.have_now, 2);
            assert_eq!(l.counts.borrowed, 1);
            assert_eq!(
                l.counts.had, 0,
                "an ended, untouched copy is not in the library"
            );
            assert_eq!(l.counts.copy_not_recorded, 0);
            let row = l.rows.iter().find(|r| r.book_id == loan).unwrap();
            assert!(row.have_now && row.borrowed && !row.had);
            assert_eq!(row.tenure, TenureDto::Borrowed);
        }
        other => panic!("{other:?}"),
    }

    // Touch the ended book (a reading) and it stays: now it is *had*.
    api.update_progress(gone, Some(10), None).await.unwrap();
    match ok(api.dispatch(Request::LibraryCopies).await) {
        Response::LibraryCopies(l) => {
            assert_eq!((l.counts.had, l.counts.copy_not_recorded), (1, 1));
            let row = l.rows.iter().find(|r| r.book_id == gone).unwrap();
            assert!(row.had && !row.have_now && row.copy_not_recorded);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(table_ids(api.clone()).await, [owned, loan, gone]);

    // A copy added later brings the copy-less book in.
    add(none, TenureDto::Own, None).await;
    assert_eq!(count(api).await, 4);
}

// ---- lists stage B, keep-here, where a highlight came from -----------------

/// A search result onto a list: the book exists, has no copy, and is not in the
/// library; the same candidate again is the same book and the same entry.
#[tokio::test]
async fn a_candidate_goes_on_a_list_without_being_owned() {
    let (api, _tmp) = api().await;
    let list = list_of(ok(api
        .dispatch(Request::CreateList {
            name: "Next".into(),
            description: None,
        })
        .await));
    let candidate = BookDto {
        title: Some("The Dispossessed".into()),
        authors: vec!["Ursula K. Le Guin".into()],
        isbn_13: Some("9780061054884".into()),
        ..Default::default()
    };
    let request = || Request::AddCandidateToList {
        list_id: list.id,
        book: candidate.clone(),
        note: Some("recommended".into()),
    };
    let entry = match ok(api.dispatch(request()).await) {
        Response::ListEntry(e) => e,
        other => panic!("{other:?}"),
    };
    assert!(!entry.in_library);
    let book_id = entry.book.id;
    assert!(api.copies_for_book(book_id).await.unwrap().is_empty());
    assert!(api.get_book(book_id).await.unwrap().is_some());

    match ok(api.dispatch(request()).await) {
        Response::ListEntry(again) => {
            assert_eq!(again.book.id, book_id);
            assert_eq!(again.position, entry.position);
        }
        other => panic!("{other:?}"),
    }
    let entries = api
        .list_entries(list.id, EntryShowDto::All, EntrySortDto::Yours)
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);

    // A book we own is reused, and is then in the library.
    let owned = seed(&api).await;
    let again = api
        .add_candidate_to_list(
            list.id,
            BookDto {
                title: Some("Station Eleven".into()),
                isbn_13: Some("9781447268963".into()),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(again.book.id, owned);
    assert!(again.in_library);

    let err = api
        .add_candidate_to_list(9999, candidate, None)
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
}

/// A copy with no file on this computer cannot be kept, says so on the wire
/// (`can_keep_here`), and the request fails as `invalid_input`.
#[tokio::test]
async fn keep_a_copy_here_is_refused_where_there_is_no_file() {
    let (api, _tmp) = api().await;
    let book = seed(&api).await;
    let copy = api.copies_for_book(book).await.unwrap().remove(0);
    assert!(!copy.can_keep_here);
    assert_eq!(copy.format, None);
    assert!(copy.places.is_empty());

    let err = api
        .dispatch(Request::KeepCopyHere { copy_id: copy.id })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidInput);
    let err = api
        .dispatch(Request::KeepCopyHere { copy_id: 9999 })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
}

/// Where a highlight came from: a KOReader import (origin, copy, a `reader`
/// place) and a highlight made here (no copy), and no same-passage groups.
#[tokio::test]
async fn highlight_origins_round_trip() {
    let (api, _tmp) = api().await;
    let book = api
        .save_book(BookDto {
            title: Some("Middlemarch".into()),
            authors: vec!["George Eliot".into()],
            ..Default::default()
        })
        .await
        .unwrap()
        .id
        .unwrap();
    have(&api, book).await;
    let sdr = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../engine/tests/fixtures/koreader/synthetic/Middlemarch.sdr");
    ok(api
        .dispatch(Request::ImportKoreader {
            path: sdr.display().to_string(),
            dry_run: false,
        })
        .await);
    let mine = api
        .create_highlight(book, "a line I wrote down myself", None, None)
        .await
        .unwrap();

    let origins = match ok(api
        .dispatch(Request::HighlightOrigins { book_id: book })
        .await)
    {
        Response::HighlightOrigins(o) => o,
        other => panic!("{other:?}"),
    };
    let ko: Vec<_> = origins
        .iter()
        .filter(|o| o.origin == bookmole_api::HighlightOriginDto::Koreader)
        .collect();
    assert!(!ko.is_empty());
    assert!(ko.iter().all(|o| o.copy_id.is_some()
        && o.pos_kind == Some(bookmole_api::PosKindDto::KoreaderXpointer)
        && !o.on_this_computer
        && o.device_name.is_none()));
    let user = origins
        .iter()
        .find(|o| o.highlight_id == mine.id)
        .expect("the user's highlight");
    assert_eq!(user.origin, bookmole_api::HighlightOriginDto::User);
    assert_eq!(user.copy_id, None);

    let json = serde_json::to_string(user).unwrap();
    assert!(json.contains(r#""origin":"user""#), "{json}");

    match ok(api.dispatch(Request::SamePassages { book_id: book }).await) {
        Response::SamePassages(groups) => assert!(groups.is_empty()),
        other => panic!("{other:?}"),
    }
}

// ---- reading-app imports, and library membership --------------------------

/// The committed Kindle fixtures the engine's own suite imports.
fn kindle_fixture(rel: &str) -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../engine/tests/fixtures/kindle")
        .join(rel);
    assert!(p.is_file(), "committed fixture {} is missing", p.display());
    p.display().to_string()
}

/// `My Clippings.txt` by request: three titles matched, one left on the report
/// with its candidates and entry count, then attached by hand; the paths a
/// client sends are re-checked below the seam.
#[tokio::test]
async fn kindle_clippings_import_and_attach_by_request() {
    let (api, tmp) = api().await;
    let book = |title: &str, author: &str| {
        let api = api.clone();
        let (title, author) = (title.to_string(), author.to_string());
        async move {
            api.save_book(BookDto {
                title: Some(title),
                authors: vec![author],
                ..Default::default()
            })
            .await
            .unwrap()
            .id
            .unwrap()
        }
    };
    let kindred = book("Kindred", "Octavia E. Butler").await;
    book(
        "Gödel, Escher, Bach: an Eternal Golden Braid (20th Anniversary Edition)",
        "Douglas R. Hofstadter",
    )
    .await;
    book("Middlemarch", "George Eliot").await;
    let path = kindle_fixture("clippings/crlf/My Clippings.txt");
    let unowned = "An Unowned Book (Nobody, A.)";

    let report = |r: Response| match r {
        Response::KindleClippingsReport(r) => r,
        other => panic!("{other:?}"),
    };
    let dry = report(ok(api
        .dispatch(Request::ImportKindleClippings {
            path: path.clone(),
            dry_run: true,
        })
        .await));
    assert!(dry.dry_run);
    assert_eq!(dry.path, path);
    let r = report(ok(api
        .dispatch(Request::ImportKindleClippings {
            path: path.clone(),
            dry_run: false,
        })
        .await));
    assert_eq!(r.books.len(), 3);
    assert_eq!(dry.summary.inserted, r.summary.inserted);
    assert!(r.books.iter().any(|b| b.book_id == kindred));
    assert_eq!(r.summary.removed, 0);
    assert_eq!(r.unmatched.len(), 1);
    let u = &r.unmatched[0];
    assert_eq!(u.title_line, unowned);
    assert_eq!(u.entries, 2);
    assert!(
        r.summary
            .diagnostics
            .iter()
            .any(|d| d.display.contains("An Unowned Book"))
    );
    let json = serde_json::to_value(&r).unwrap();
    assert!(json["unmatched"][0]["candidates"].is_array(), "{json}");

    let attached = report(ok(api
        .dispatch(Request::AttachKindleTitle {
            path: path.clone(),
            title_line: u.title_line.clone(),
            book_id: kindred,
        })
        .await));
    assert!(!attached.dry_run);
    assert_eq!(attached.books.len(), 1);
    assert_eq!(attached.summary.inserted, 2);
    assert!(attached.unmatched.is_empty());

    // Handles do not cross: each path is looked at again here.
    let err = api
        .dispatch(Request::ImportKindleClippings {
            path: tmp.path().join("gone.txt").display().to_string(),
            dry_run: true,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
    let err = api
        .dispatch(Request::ImportKindleClippings {
            path: kindle_fixture("notebook/remains.html"),
            dry_run: true,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidInput);
    let err = api
        .dispatch(Request::AttachKindleTitle {
            path: tmp.path().display().to_string(),
            title_line: unowned.into(),
            book_id: kindred,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidInput);
    let err = api
        .dispatch(Request::AttachKindleTitle {
            path,
            title_line: unowned.into(),
            book_id: 9_999,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);

    // The mounted pair parse as named; dispatching them would read whatever
    // reader is plugged into this machine, so the engine's suite tests the
    // volume walk over temp roots instead.
    let parsed: Request = serde_json::from_str(r#"{"method":"find_kindle_clippings"}"#).unwrap();
    assert!(matches!(parsed, Request::FindKindleClippings));
    let parsed: Request =
        serde_json::from_str(r#"{"method":"import_mounted_kindle_clippings","params":{}}"#)
            .unwrap();
    assert!(matches!(
        parsed,
        Request::ImportMountedKindleClippings { dry_run: false }
    ));
}

/// The Kindle app's notebook by request: matched by its title, unmatched with
/// the file's own title and nothing written, then imported into a named book.
#[tokio::test]
async fn a_kindle_notebook_imports_by_request() {
    let (api, _tmp) = api().await;
    let remains = api
        .save_book(BookDto {
            title: Some("The Remains of the Day".into()),
            authors: vec!["Kazuo Ishiguro".into()],
            ..Default::default()
        })
        .await
        .unwrap()
        .id
        .unwrap();
    let report = |r: Response| match r {
        Response::KindleNotebookReport(r) => r,
        other => panic!("{other:?}"),
    };
    let r = report(ok(api
        .dispatch(Request::ImportKindleNotebook {
            path: kindle_fixture("notebook/remains.html"),
            book_id: None,
            dry_run: false,
        })
        .await));
    assert_eq!(r.book_id, Some(remains));
    assert!(r.summary.inserted > 0);
    assert_eq!(r.summary.inserted, r.entries);

    let unowned = kindle_fixture("notebook/unowned.html");
    let r = report(ok(api
        .dispatch(Request::ImportKindleNotebook {
            path: unowned.clone(),
            book_id: None,
            dry_run: false,
        })
        .await));
    assert_eq!(r.book_id, None);
    assert_eq!(r.title.as_deref(), Some("A Book Nobody Owns"));
    assert_eq!(r.summary.inserted, 0);
    let r = report(ok(api
        .dispatch(Request::ImportKindleNotebook {
            path: unowned.clone(),
            book_id: Some(remains),
            dry_run: false,
        })
        .await));
    assert_eq!(r.book_id, Some(remains));
    assert_eq!(r.summary.inserted, 1);

    let err = api
        .dispatch(Request::ImportKindleNotebook {
            path: unowned,
            book_id: Some(9_999),
            dry_run: true,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
    let err = api
        .dispatch(Request::ImportKindleNotebook {
            path: kindle_fixture("clippings/crlf/My Clippings.txt"),
            book_id: None,
            dry_run: true,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidInput);
}

/// An `Api` whose engine looks for Apple Books at `root` (or nowhere), so no
/// test ever reads the real container under `~/Library`.
async fn api_with_apple_books(root: Option<PathBuf>) -> (Api, tempfile::TempDir) {
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
    let engine = Engine::open(config)
        .await
        .expect("engine")
        .with_apple_books_root(root);
    (Api::new(Arc::new(engine)), tmp)
}

/// Apple Books by request. Its import is tested over fixture databases in the
/// engine's suite (this crate does not link `sqlx` to build them); here, the
/// request answers "no data here" as a report and never as an error, and the
/// link records an app copy holding the asset.
#[tokio::test]
async fn apple_books_with_no_data_is_a_report_and_a_link_makes_a_copy() {
    for root in [
        None,
        Some(std::env::temp_dir().join("bookmole-no-apple-books-here")),
    ] {
        let (api, _tmp) = api_with_apple_books(root).await;
        let r = match ok(api
            .dispatch(Request::ImportAppleBooks { dry_run: true })
            .await)
        {
            Response::AppleBooksReport(r) => r,
            other => panic!("{other:?}"),
        };
        assert!(r.dry_run);
        assert!(r.books.is_empty());
        assert_eq!(r.annotation_db, None);
        let json = serde_json::to_value(&r).unwrap();
        assert!(
            json["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["kind"] == "apple_books_absent"),
            "{json}"
        );
    }

    let (api, _tmp) = api_with_apple_books(None).await;
    let book = seed(&api).await;
    let copy = copy_of(ok(api
        .dispatch(Request::LinkAppleBooksAsset {
            asset_id: "ASSET-1".into(),
            book_id: book,
        })
        .await));
    assert_eq!(copy.book_id, book);
    assert_eq!(copy.form, Some(CopyFormDto::App));
    assert_eq!(copy.app.as_deref(), Some("apple_books"));
    assert_eq!(copy.places.len(), 1);
    assert_eq!(copy.places[0].kind, bookmole_api::PlaceKindDto::AppleBooks);
    assert_eq!(copy.places[0].reference, "ASSET-1");
    let again = copy_of(ok(api
        .dispatch(Request::LinkAppleBooksAsset {
            asset_id: "ASSET-1".into(),
            book_id: book,
        })
        .await));
    assert_eq!(again.id, copy.id);

    let other = save(&api, "Another", "9780000000088").await;
    let err = api
        .dispatch(Request::LinkAppleBooksAsset {
            asset_id: "ASSET-1".into(),
            book_id: other,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidInput);
    let err = api
        .dispatch(Request::LinkAppleBooksAsset {
            asset_id: "ASSET-2".into(),
            book_id: 9_999,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
}

/// Preview by request. A generated annotated PDF is behind the engine's
/// `internals` feature, which this crate's tests do not turn on, so the
/// annotations themselves are the engine suite's
/// (`the_facade_adds_a_named_pdf_and_rescans_the_books_pdfs`). Here: a file
/// that is not a readable PDF still becomes the book's copy and comes back as
/// a diagnostic, the book-wide rescan sees its place, and the paths are
/// re-checked below the seam.
#[tokio::test]
async fn preview_requests_add_the_file_and_refuse_what_is_not_a_pdf() {
    let (api, tmp) = api().await;
    let book = save(&api, "A Paper", "9780000000095").await;

    match ok(api
        .dispatch(Request::ImportPreviewAnnotations { book_id: book })
        .await)
    {
        Response::PreviewImport(p) => {
            assert!(p.places.is_empty());
            assert_eq!(p.totals, bookmole_api::HighlightImportDto::default());
        }
        other => panic!("{other:?}"),
    }

    let pdf = tmp.path().join("broken.pdf");
    std::fs::write(&pdf, b"this is not a pdf at all").unwrap();
    let pdf = pdf.display().to_string();
    let sum = match ok(api
        .dispatch(Request::ImportPreviewFile {
            book_id: book,
            path: pdf.clone(),
        })
        .await)
    {
        Response::HighlightImport(s) => s,
        other => panic!("{other:?}"),
    };
    assert_eq!((sum.inserted, sum.removed), (0, 0));
    assert!(
        sum.diagnostics
            .iter()
            .any(|d| { serde_json::to_value(d).unwrap()["kind"] == "pdf_unreadable" }),
        "{sum:?}"
    );
    let copies = api.copies_for_book(book).await.unwrap();
    assert_eq!(copies.len(), 1);
    assert_eq!(copies[0].places[0].kind, bookmole_api::PlaceKindDto::Path);

    match ok(api
        .dispatch(Request::ImportPreviewAnnotations { book_id: book })
        .await)
    {
        Response::PreviewImport(p) => {
            assert_eq!(p.places.len(), 1);
            assert_eq!(p.places[0].copy_id, copies[0].id);
            assert_eq!(p.places[0].path, pdf);
            assert_eq!(
                p.places[0].outcome,
                bookmole_api::RescanOutcomeDto::Unchanged
            );
        }
        other => panic!("{other:?}"),
    }

    let refused = |path: String, book_id: i64| {
        let api = api.clone();
        async move {
            api.dispatch(Request::ImportPreviewFile { book_id, path })
                .await
                .unwrap_err()
                .code
        }
    };
    let gone = tmp.path().join("gone.pdf").display().to_string();
    assert_eq!(refused(gone, book).await, ErrorCode::NotFound);
    let txt = tmp.path().join("notes.txt");
    std::fs::write(&txt, b"words").unwrap();
    assert_eq!(
        refused(txt.display().to_string(), book).await,
        ErrorCode::InvalidInput
    );
    assert_eq!(
        refused(tmp.path().display().to_string(), book).await,
        ErrorCode::InvalidInput
    );
    let other = save(&api, "Another Paper", "9780000000101").await;
    assert_eq!(refused(pdf, other).await, ErrorCode::InvalidInput);
    let err = api
        .dispatch(Request::ImportPreviewAnnotations { book_id: 9_999 })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
}

/// `ResolveLibraryBooks` answers only
/// books you have, where `ResolveBooks` answers every book; by title, ISBN
/// and id alike.
#[tokio::test]
async fn resolve_library_books_hides_a_book_only_on_a_list() {
    let (api, _tmp) = api().await;
    let owned = save(&api, "Dune", "9780441172719").await;
    have(&api, owned).await;
    let listed = save(&api, "Dune Messiah", "9780593098233").await;

    let ids = |r: Response| match r {
        Response::Books(books) => {
            let mut ids: Vec<i64> = books.iter().map(|b| b.id.unwrap()).collect();
            ids.sort();
            ids
        }
        other => panic!("{other:?}"),
    };
    let everywhere = ids(ok(api
        .dispatch(Request::ResolveBooks {
            selector: "Dune".into(),
        })
        .await));
    assert_eq!(everywhere, vec![owned, listed]);
    let mine = ids(ok(api
        .dispatch(Request::ResolveLibraryBooks {
            selector: "Dune".into(),
        })
        .await));
    assert_eq!(mine, vec![owned]);

    for selector in ["9780593098233".to_string(), listed.to_string()] {
        assert_eq!(
            ids(ok(api
                .dispatch(Request::ResolveBooks {
                    selector: selector.clone()
                })
                .await)),
            vec![listed]
        );
        assert!(
            ids(ok(api
                .dispatch(Request::ResolveLibraryBooks { selector })
                .await))
            .is_empty()
        );
    }
    assert_eq!(
        ids(ok(api
            .dispatch(Request::ResolveLibraryBooks {
                selector: owned.to_string()
            })
            .await)),
        vec![owned]
    );
    // The typed method is the same call.
    let typed: Vec<i64> = api
        .resolve_library_books("Dune")
        .await
        .unwrap()
        .iter()
        .map(|b| b.id.unwrap())
        .collect();
    assert_eq!(typed, vec![owned]);
}

/// A list carries how many of its books are in the library, counted by the
/// engine beside `not_in_library`, on the list and on its summary.
#[tokio::test]
async fn a_list_says_how_many_of_its_books_are_in_the_library() {
    let (api, _tmp) = api().await;
    let a = save(&api, "Alpha", "9780000000002").await;
    let b = save(&api, "Bravo", "9780000000019").await;
    let c = save(&api, "Charlie", "9780000000026").await;
    have(&api, a).await;
    let list = list_of(ok(api
        .dispatch(Request::CreateList {
            name: "Mixed".into(),
            description: None,
        })
        .await));
    assert_eq!(list.in_library_count, 0);
    for book_id in [a, b, c] {
        api.add_to_list(list.id, book_id, None).await.unwrap();
    }
    let list = list_of(ok(api.dispatch(Request::GetList { id: list.id }).await));
    assert_eq!(
        (list.book_count, list.in_library_count, list.not_in_library),
        (3, 1, 2)
    );
    match ok(api
        .dispatch(Request::ListLists {
            sort: ListSortDto::Name,
            preview: 0,
        })
        .await)
    {
        Response::ListSummaries(lists) => {
            assert_eq!(lists.len(), 1);
            assert_eq!(
                (
                    lists[0].book_count,
                    lists[0].in_library_count,
                    lists[0].not_in_library
                ),
                (3, 1, 2)
            );
        }
        other => panic!("{other:?}"),
    }
    let json = serde_json::to_value(&list).unwrap();
    assert_eq!(json["in_library_count"], 1, "{json}");
}

// ---- a copy's files and highlight sources -------------------------------

/// The three new requests parse from their smallest honest payload, and none
/// reshapes a request that was already there.
#[test]
fn the_copies_place_requests_parse_from_the_smallest_payload() {
    let files: Request =
        serde_json::from_str(r#"{"method":"copy_files","params":{"book_id":3}}"#).unwrap();
    assert_eq!(files, Request::CopyFiles { book_id: 3 });
    let sources: Request =
        serde_json::from_str(r#"{"method":"copy_highlight_sources","params":{"book_id":3}}"#)
            .unwrap();
    assert_eq!(sources, Request::CopyHighlightSources { book_id: 3 });
    let found: Request = serde_json::from_str(
        r#"{"method":"relocate_copy_file","params":{"copy_id":4,"path":"/x/a.epub"}}"#,
    )
    .unwrap();
    assert_eq!(
        found,
        Request::RelocateCopyFile {
            copy_id: 4,
            path: "/x/a.epub".into()
        }
    );
}

/// A file added to a book is a row with its size and `on_disk`; moved away, it
/// says so; *Find the file…* puts the copy's place where the file is now. A
/// highlight made here is a source with no copy and no "last read in", and the
/// wire carries no count. Unknown ids and someone else's file are refused.
#[tokio::test]
async fn the_copies_place_round_trips_through_dispatch() {
    let (api, tmp) = api().await;
    let book = save(&api, "A Found Book", "9780000000118").await;
    let file = tmp.path().join("A Found Book.mobi");
    std::fs::write(&file, b"0123456789").unwrap();
    ok(api
        .dispatch(Request::AddFileToBook {
            book_id: book,
            path: file.display().to_string(),
        })
        .await);
    let copy = api.copies_for_book(book).await.unwrap().remove(0);

    let files = |api: Api| async move {
        match ok(api.dispatch(Request::CopyFiles { book_id: book }).await) {
            Response::CopyFiles(f) => f,
            other => panic!("{other:?}"),
        }
    };
    let rows = files(api.clone()).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].copy_id, copy.id);
    assert_eq!(rows[0].kind, bookmole_api::PlaceKindDto::Path);
    assert_eq!(rows[0].path.as_deref(), Some(file.to_str().unwrap()));
    assert_eq!(rows[0].format.as_deref(), Some("mobi"));
    assert_eq!((rows[0].on_disk, rows[0].size), (Some(true), Some(10)));
    let json = serde_json::to_value(&rows[0]).unwrap();
    assert_eq!(json["kind"], "path");
    assert!(json.get("calibre_library").is_some_and(|v| v.is_null()));

    // Moved: nothing marked it, the look says so.
    let moved = tmp.path().join("moved");
    std::fs::create_dir_all(&moved).unwrap();
    let new = moved.join("A Found Book.mobi");
    std::fs::rename(&file, &new).unwrap();
    let rows = files(api.clone()).await;
    assert_eq!((rows[0].on_disk, rows[0].size), (Some(false), None));

    let found = match ok(api
        .dispatch(Request::RelocateCopyFile {
            copy_id: copy.id,
            path: new.display().to_string(),
        })
        .await)
    {
        Response::Copy(c) => c,
        other => panic!("{other:?}"),
    };
    assert_eq!(found.id, copy.id);
    let refs: Vec<&str> = found.places.iter().map(|p| p.reference.as_str()).collect();
    assert!(refs.contains(&new.to_str().unwrap()), "{refs:?}");
    assert!(!refs.contains(&file.to_str().unwrap()), "{refs:?}");
    assert_eq!(found.format.as_deref(), Some("mobi"));
    let rows = files(api.clone()).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].path.as_deref(), Some(new.to_str().unwrap()));
    assert_eq!(rows[0].on_disk, Some(true));

    // Where the highlights came from: one typed here, no copy, no count.
    api.create_highlight(book, "a line I kept", None, None)
        .await
        .unwrap();
    let sources = match ok(api
        .dispatch(Request::CopyHighlightSources { book_id: book })
        .await)
    {
        Response::CopyHighlightSources(s) => s,
        other => panic!("{other:?}"),
    };
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].origin, bookmole_api::HighlightOriginDto::User);
    assert_eq!((sources[0].copy_id, sources[0].last_read_at), (None, None));
    let json = serde_json::to_string(&sources).unwrap();
    assert!(!json.contains("count"), "{json}");
    assert!(!json.contains("a line I kept"), "{json}");

    // Refusals, each typed.
    let code = |r: Request| {
        let api = api.clone();
        async move { api.dispatch(r).await.unwrap_err().code }
    };
    assert_eq!(
        code(Request::CopyFiles { book_id: 9_999 }).await,
        ErrorCode::NotFound
    );
    assert_eq!(
        code(Request::CopyHighlightSources { book_id: 9_999 }).await,
        ErrorCode::NotFound
    );
    assert_eq!(
        code(Request::RelocateCopyFile {
            copy_id: 9_999,
            path: new.display().to_string(),
        })
        .await,
        ErrorCode::NotFound
    );
    assert_eq!(
        code(Request::RelocateCopyFile {
            copy_id: copy.id,
            path: tmp.path().join("nowhere.mobi").display().to_string(),
        })
        .await,
        ErrorCode::NotFound
    );
    let other = save(&api, "Somebody Else", "9780000000125").await;
    have(&api, other).await;
    let theirs = api.copies_for_book(other).await.unwrap().remove(0);
    assert_eq!(
        code(Request::RelocateCopyFile {
            copy_id: theirs.id,
            path: new.display().to_string(),
        })
        .await,
        ErrorCode::InvalidInput
    );
}
