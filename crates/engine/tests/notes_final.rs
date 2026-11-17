mod common;

use bookmole::{Engine, EngineConfig, GraphScope, NewNoteInput, NoteKind, NoteRowQuery, NoteScope};
use common::seed_book;

/// A file database: `Storage::pool` is crate-private, so a test sets a note's moment through its
/// own connection to the same file.
async fn engine() -> (tempfile::TempDir, Engine) {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(tmp.path().join("database")).unwrap();
    let config = EngineConfig {
        db_url: format!(
            "sqlite://{}?mode=rwc",
            tmp.path().join("database/app.db").display()
        ),
        images_dir: tmp.path().join("database/images"),
        files_dir: tmp.path().join("database/files"),
        vault_dir: tmp.path().join("vault"),
        log_dir: tmp.path().join("logs"),
        exports_dir: tmp.path().join("exports"),
        google_api_key: None,
        calibre_bin_dir: None,
    };
    let engine = Engine::open(config).await.expect("engine opens");
    (tmp, engine)
}

async fn raw(engine: &Engine) -> sqlx::SqlitePool {
    sqlx::SqlitePool::connect(engine.db_url()).await.unwrap()
}

async fn note(engine: &Engine, book_id: Option<i64>, title: Option<&str>, body: &str) -> i64 {
    engine
        .create_note(NewNoteInput {
            book_id,
            kind: NoteKind::Note,
            title: title.map(Into::into),
            body: body.into(),
            ..Default::default()
        })
        .await
        .expect("create note")
        .id
}

async fn backdate(engine: &Engine, id: i64, at: i64) {
    sqlx::query("UPDATE notes SET created_at = ?1, latest_part_at = ?1 WHERE id = ?2")
        .bind(at)
        .bind(id)
        .execute(&raw(engine).await)
        .await
        .unwrap();
}

fn all(limit: i64) -> NoteRowQuery {
    NoteRowQuery {
        limit,
        ..Default::default()
    }
}

#[tokio::test]
async fn a_heading_names_the_note_and_leaves_the_excerpt() {
    let (_tmp, engine) = engine().await;
    let id = note(
        &engine,
        None,
        None,
        "# maps that lie\n\nEvery map is **an argument** about [[Borders|the border]].",
    )
    .await;
    let rec = engine.get_note(id).await.unwrap().unwrap();
    assert_eq!(
        rec.title, "maps that lie",
        "the title comes from the heading"
    );

    let rows = engine.note_rows(&all(10)).await.unwrap();
    assert_eq!(rows[0].heading.as_deref(), Some("maps that lie"));
    assert_eq!(
        rows[0].excerpt,
        "Every map is an argument about the border."
    );
    assert_eq!(rows[0].links_out, 1);
    assert_eq!(rows[0].parts, 1);
}

#[tokio::test]
async fn a_derived_title_is_no_heading_and_a_typed_one_is() {
    let (_tmp, engine) = engine().await;
    let book = seed_book(&engine, "Middlemarch").await;
    let derived = note(
        &engine,
        None,
        None,
        "Dorothea walks to the market alone today.",
    )
    .await;
    let typed = note(
        &engine,
        None,
        Some("Market"),
        "Dorothea walks to the market.",
    )
    .await;
    let reflection = engine.open_reflection(book, None).await.unwrap().id;
    engine
        .update_note_body(
            &engine.get_note(reflection).await.unwrap().unwrap(),
            "# A heading inside",
        )
        .await
        .unwrap();

    let rows = engine.note_rows(&all(10)).await.unwrap();
    let heading = |id: i64| {
        rows.iter()
            .find(|r| r.note.id == id)
            .unwrap()
            .heading
            .clone()
    };
    assert_eq!(heading(derived), None);
    assert_eq!(heading(typed).as_deref(), Some("Market"));
    assert_eq!(
        heading(reflection),
        None,
        "a reflection's title is generated"
    );
}

#[tokio::test]
async fn rows_order_by_the_latest_part_and_every_narrowing_is_a_predicate() {
    let (_tmp, engine) = engine().await;
    let book = seed_book(&engine, "Middlemarch").await;
    let old = note(&engine, Some(book), Some("Old"), "first").await;
    let mid = note(&engine, None, Some("Mid"), "second").await;
    let new = note(&engine, Some(book), Some("New"), "third").await;
    backdate(&engine, old, 1_000).await;
    backdate(&engine, mid, 2_000).await;
    backdate(&engine, new, 3_000).await;

    let ids =
        |rows: Vec<bookmole::NoteRow>| rows.into_iter().map(|r| r.note.id).collect::<Vec<_>>();
    assert_eq!(
        ids(engine.note_rows(&all(10)).await.unwrap()),
        vec![new, mid, old]
    );

    engine
        .continue_note(old, "a second sitting", Some(40))
        .await
        .unwrap();
    let rows = engine.note_rows(&all(10)).await.unwrap();
    assert_eq!(rows[0].note.id, old);
    assert_eq!(rows[0].parts, 2);
    assert_eq!(rows[0].latest_part_page, Some(40));
    assert_eq!(
        rows[0].excerpt, "a second sitting",
        "the latest part speaks for the row"
    );
    let moved_at = rows[0].latest_part_at;
    engine
        .update_note_part(old, 1, "first, edited")
        .await
        .unwrap();
    assert_eq!(
        engine.note_rows(&all(10)).await.unwrap()[0].latest_part_at,
        moved_at
    );

    let q = |scope, until, ids_: Option<Vec<i64>>| NoteRowQuery {
        scope,
        until,
        ids: ids_,
        limit: 10,
        ..Default::default()
    };
    assert_eq!(
        ids(engine
            .note_rows(&q(NoteScope::Book(book), None, None))
            .await
            .unwrap()),
        vec![old, new]
    );
    assert_eq!(
        ids(engine
            .note_rows(&q(NoteScope::All, Some(3_000), None))
            .await
            .unwrap()),
        vec![mid]
    );
    assert_eq!(
        ids(engine
            .note_rows(&q(NoteScope::All, None, Some(vec![mid, new])))
            .await
            .unwrap()),
        vec![new, mid],
        "ids keep the engine's order, never the caller's"
    );
    let reviews = NoteRowQuery {
        kinds: Some(vec![NoteKind::Review]),
        limit: 10,
        ..Default::default()
    };
    assert!(engine.note_rows(&reviews).await.unwrap().is_empty());
    assert_eq!(
        ids(engine.note_rows(&all(1)).await.unwrap()),
        vec![old],
        "the limit cuts last"
    );
}

#[tokio::test]
async fn periods_are_the_months_latest_parts_fall_in_newest_first() {
    let (_tmp, engine) = engine().await;
    let a = note(&engine, None, Some("A"), "x").await;
    let b = note(&engine, None, Some("B"), "y").await;
    let c = note(&engine, None, Some("C"), "z").await;
    backdate(&engine, a, 1_696_118_400).await; // 2023-10-01 UTC
    backdate(&engine, b, 1_698_796_799).await; // 2023-10-31 23:59:59 UTC
    backdate(&engine, c, 1_704_067_200).await; // 2024-01-01 UTC
    assert_eq!(
        engine.note_periods().await.unwrap(),
        vec![(2024, 1), (2023, 10)]
    );
}

#[tokio::test]
async fn periods_in_a_zone_file_each_note_under_the_reader_s_month() {
    let (_tmp, engine) = engine().await;
    let a = note(&engine, None, Some("A"), "x").await;
    let b = note(&engine, None, Some("B"), "y").await;
    let c = note(&engine, None, Some("C"), "z").await;
    backdate(&engine, a, 1_696_118_400).await; // 2023-10-01 00:00 UTC
    backdate(&engine, b, 1_698_796_799).await; // 2023-10-31 23:59:59 UTC
    backdate(&engine, c, 1_704_067_200).await; // 2024-01-01 00:00 UTC
    assert_eq!(
        engine.note_periods_in(0).await.unwrap(),
        engine.note_periods().await.unwrap(),
        "an offset of zero is UTC"
    );
    assert_eq!(
        engine.note_periods_in(60).await.unwrap(),
        vec![(2024, 1), (2023, 11), (2023, 10)]
    );
    assert_eq!(
        engine.note_periods_in(-480).await.unwrap(),
        vec![(2023, 12), (2023, 10), (2023, 9)]
    );
    assert!(engine.note_periods_in(18 * 60).await.is_ok());
    assert!(
        engine.note_periods_in(18 * 60 + 1).await.is_err(),
        "beyond ±18 hours is no zone"
    );
    assert!(engine.note_periods_in(-(18 * 60 + 1)).await.is_err());
}

async fn two_threads(engine: &Engine) -> [i64; 7] {
    let middlemarch = seed_book(engine, "Middlemarch").await;
    let borders = seed_book(engine, "Borders").await;
    let hub = note(engine, Some(middlemarch), Some("Hub"), "the centre").await;
    let a = note(engine, Some(borders), Some("A"), "see [[Hub]]").await;
    let c = note(engine, None, Some("C"), "leaf").await;
    let b = note(
        engine,
        Some(middlemarch),
        Some("B"),
        "[[Hub]] and [[C]] and [[Unwritten]]",
    )
    .await;
    let x = note(engine, None, Some("X"), "[[Y]]").await;
    let y = note(engine, None, Some("Y"), "[[X]]").await;
    let lone = note(engine, None, Some("Lone"), "nothing").await;
    for (i, id) in [hub, a, c, b, x, y, lone].into_iter().enumerate() {
        backdate(engine, id, 1_000 + i as i64 * 100).await;
    }
    [hub, a, b, c, x, y, lone]
}

#[tokio::test]
async fn threads_have_hubs_and_every_member_row_carries_its_hub() {
    let (_tmp, engine) = engine().await;
    let [hub, a, b, c, x, y, lone] = two_threads(&engine).await;

    let rows = engine.note_rows(&all(20)).await.unwrap();
    let thread = |id| rows.iter().find(|r| r.note.id == id).unwrap().thread;
    for m in [hub, a, c] {
        assert_eq!(
            thread(m),
            Some(b),
            "Hub and B tie at two neighbours; B is newer"
        );
    }
    assert_eq!(thread(b), Some(b));
    assert_eq!(thread(x), thread(y));
    assert_eq!(thread(y), Some(y), "a tie on degree goes to the newest");
    assert_eq!(thread(lone), None);

    let threads = engine.threads_for(&[a, lone], Some(1_150)).await.unwrap();
    assert_eq!(threads.len(), 1, "only the threads holding a named note");
    let t = &threads[0];
    assert_eq!(t.hub.note.id, b);
    assert_eq!(t.notes, 4);
    assert_eq!(t.members, vec![b, c, a, hub], "newest first");
    assert_eq!(t.joined, 2, "B and C are at or after 1150");
    let books: Vec<_> = t
        .books
        .iter()
        .map(|b| b.display_title().to_string())
        .collect();
    assert_eq!(books, vec!["Middlemarch", "Borders"]);

    let both = engine.threads_for(&[hub, x], None).await.unwrap();
    assert_eq!(
        both.iter().map(|t| t.hub.note.id).collect::<Vec<_>>(),
        vec![y, b]
    );
    assert!(both.iter().all(|t| t.joined == 0));
}

#[tokio::test]
async fn links_you_made_come_newest_source_first_in_body_order() {
    let (_tmp, engine) = engine().await;
    let [hub, a, b, c, ..] = two_threads(&engine).await;
    let links = engine.links_from_notes(&[a, b]).await.unwrap();
    let shape: Vec<_> = links
        .iter()
        .map(|l| {
            (
                l.from.note.id,
                l.target_title.as_str(),
                l.to.as_ref().map(|n| n.id),
            )
        })
        .collect();
    assert_eq!(
        shape,
        vec![
            (b, "Hub", Some(hub)),
            (b, "C", Some(c)),
            (b, "Unwritten", None),
            (a, "Hub", Some(hub)),
        ]
    );
}

#[tokio::test]
async fn the_graph_draws_each_scope() {
    let (_tmp, engine) = engine().await;
    let [hub, a, b, c, x, y, lone] = two_threads(&engine).await;
    let keys = |g: &bookmole::NoteGraphView| {
        let mut k: Vec<String> = g.nodes.iter().map(|n| n.key.clone()).collect();
        k.sort();
        k
    };
    let n = |id: i64| format!("n{id}");

    let everything = engine.note_graph(GraphScope::Everything).await.unwrap();
    let mut want = vec![
        n(hub),
        n(a),
        n(b),
        n(c),
        n(x),
        n(y),
        "tunwritten".to_string(),
    ];
    want.sort();
    assert_eq!(keys(&everything), want, "no lone note");
    assert!(!keys(&everything).contains(&n(lone)));
    let unwritten = everything
        .nodes
        .iter()
        .find(|n| n.key == "tunwritten")
        .unwrap();
    assert!(unwritten.row.is_none());
    assert_eq!(unwritten.title, "Unwritten");

    let recent = engine
        .note_graph(GraphScope::Recent { since: 1_450 })
        .await
        .unwrap();
    let mut want = vec![n(x), n(y)];
    want.sort();
    assert_eq!(
        keys(&recent),
        want,
        "Y is recent, X is one step away, Lone has no edge"
    );

    let around = engine
        .note_graph(GraphScope::Around {
            note_id: hub,
            steps: 1,
        })
        .await
        .unwrap();
    let mut want = vec![n(hub), n(a), n(b)];
    want.sort();
    assert_eq!(keys(&around), want);
    let two = engine
        .note_graph(GraphScope::Around {
            note_id: hub,
            steps: 2,
        })
        .await
        .unwrap();
    assert!(keys(&two).contains(&n(c)));
    assert!(
        engine
            .note_graph(GraphScope::Around {
                note_id: hub,
                steps: 3
            })
            .await
            .is_err()
    );
    assert!(
        engine
            .note_graph(GraphScope::Around {
                note_id: 999,
                steps: 1
            })
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_note_continues_in_parts_and_an_outside_edit_can_unpart_it() {
    let (_tmp, engine) = engine().await;
    let id = note(&engine, None, Some("Reading log"), "Part one words.").await;
    let rec = engine.get_note(id).await.unwrap().unwrap();

    let parts = engine
        .continue_note(id, "Second thoughts.", Some(251))
        .await
        .unwrap();
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0].body, "Part one words.");
    assert_eq!(parts[0].at, rec.created_at.unwrap().unix_timestamp());
    assert_eq!(parts[1].page, Some(251));
    let parts = engine.continue_note(id, "Third.", None).await.unwrap();
    assert_eq!(
        parts.iter().map(|p| p.body.as_str()).collect::<Vec<_>>(),
        vec!["Part one words.", "Second thoughts.", "Third."],
        "continuing never changes an earlier part"
    );

    let raw = engine.note_body(&rec).unwrap();
    assert!(raw.contains("<!-- part 2 · "));
    assert!(raw.contains(" · p. 251 -->"));

    let parts = engine
        .update_note_part(id, 2, "Revised second.")
        .await
        .unwrap();
    assert_eq!(parts[1].body, "Revised second.");
    assert_eq!(parts[1].page, Some(251), "the marker is untouched");
    assert_eq!(parts[2].body, "Third.");

    assert!(engine.continue_note(id, "   ", None).await.is_err());
    assert!(
        engine
            .continue_note(id, "x\n<!-- part 9 · 2026-01-01T00:00:00Z -->\ny", None)
            .await
            .is_err()
    );
    assert!(engine.update_note_part(id, 4, "nope").await.is_err());
    assert!(engine.update_note_part(id, 0, "nope").await.is_err());

    let count = |engine: &Engine| {
        let engine_url = engine.db_url().to_string();
        async move {
            let pool = sqlx::SqlitePool::connect(&engine_url).await.unwrap();
            sqlx::query_scalar::<_, i64>("SELECT parts FROM notes WHERE id = ?")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    assert_eq!(count(&engine).await, 3);

    let path = engine.note_path(&rec);
    let content = std::fs::read_to_string(&path).unwrap();
    let kept: String = content
        .lines()
        .filter(|l| !l.starts_with("<!-- part "))
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(&path, kept).unwrap();
    engine.refresh_note_from_disk(&rec).await.unwrap();
    assert_eq!(count(&engine).await, 1);
    assert_eq!(engine.note_parts(id).await.unwrap().len(), 1);
}

#[tokio::test]
async fn the_matched_parts_are_the_ones_the_search_found() {
    let (_tmp, engine) = engine().await;
    let id = note(&engine, None, Some("Reading log"), "The harbour at dusk.").await;
    engine
        .continue_note(id, "Nothing about water.", None)
        .await
        .unwrap();
    engine
        .continue_note(id, "Back to the harbours and a lighthouse.", Some(12))
        .await
        .unwrap();

    assert_eq!(
        engine.matched_parts(id, "harbour").await.unwrap(),
        vec![1, 3],
        "stemmed as the index stems: harbours is harbour"
    );
    assert_eq!(
        engine.matched_parts(id, "lighthouse").await.unwrap(),
        vec![3]
    );
    assert_eq!(
        engine.matched_parts(id, "dusk lighthouse").await.unwrap(),
        vec![1, 3],
        "no part holds both words, so the parts holding either"
    );
    assert_eq!(
        engine
            .matched_parts(id, "harbour lighthouse")
            .await
            .unwrap(),
        vec![3],
        "a part holding every word wins over parts holding some"
    );
    assert!(
        engine.matched_parts(id, "log").await.unwrap().is_empty(),
        "a title-only match names no part"
    );
    assert!(engine.matched_parts(id, "").await.unwrap().is_empty());
    assert!(engine.matched_parts(id + 100, "harbour").await.is_err());

    for q in ["harbour", "lighthouse", "water", "dusk lighthouse"] {
        let hits = engine.search_marks(q, None, None, 10).await.unwrap();
        assert!(hits.iter().any(|h| h.as_note().is_some_and(|n| n.id == id)));
        assert!(
            !engine.matched_parts(id, q).await.unwrap().is_empty(),
            "{q}: the search found the note, so some part matched"
        );
    }
}

#[tokio::test]
async fn an_unlinked_mention_is_offered_and_linking_it_closes_the_offer() {
    let (_tmp, engine) = engine().await;
    let target = note(&engine, None, None, "# Maps That Lie\n\nOn cartography.").await;
    let mentioner = note(
        &engine,
        None,
        Some("Atlas"),
        "As in [[Elsewhere|maps that lie]], then: maps that lie are everywhere.",
    )
    .await;
    let linked = note(
        &engine,
        None,
        Some("Linked"),
        "[[Maps That Lie]] — maps that lie",
    )
    .await;
    let substring = note(&engine, None, Some("Sub"), "roadmaps that lies are made of").await;
    let derived = note(&engine, None, None, "a derived title for this one").await;
    let _echo = note(
        &engine,
        None,
        Some("Echo"),
        "a derived title for this one again",
    )
    .await;

    let hits = engine.unlinked_mentions(target, 10).await.unwrap();
    let ids: Vec<i64> = hits
        .iter()
        .filter_map(|h| h.as_note().map(|n| n.id))
        .collect();
    assert_eq!(ids, vec![mentioner], "not the linked note, not a substring");
    assert!(!ids.contains(&linked) && !ids.contains(&substring));
    assert!(
        engine
            .unlinked_mentions(derived, 10)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        engine
            .unlinked_mentions(target, 0)
            .await
            .unwrap()
            .is_empty()
    );

    engine.link_mention(mentioner, target).await.unwrap();
    let rec = engine.get_note(mentioner).await.unwrap().unwrap();
    let body = engine.note_body(&rec).unwrap();
    assert!(
        body.contains("then: [[Maps That Lie|maps that lie]] are everywhere."),
        "the first mention outside a link, aliased because the case differs"
    );
    assert!(
        body.contains("[[Elsewhere|maps that lie]]"),
        "the alias inside a link was not it"
    );
    assert!(
        engine
            .backlinks(target)
            .await
            .unwrap()
            .iter()
            .any(|n| n.id == mentioner)
    );
    assert!(
        engine
            .unlinked_mentions(target, 10)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        engine.link_mention(substring, target).await.is_err(),
        "NotFound: no mention"
    );
    assert!(engine.link_mention(target, target).await.is_err());
}

#[tokio::test]
async fn a_typed_passage_is_the_users_and_two_identical_are_two() {
    let (_tmp, engine) = engine().await;
    let book = seed_book(&engine, "Middlemarch").await;
    engine.update_progress(book, Some(10), None).await.unwrap();
    let reading = engine.active_reading(book).await.unwrap().unwrap().id;

    let a = engine
        .create_highlight(book, "  Patience is bitter.  ", Some(12), Some("One"))
        .await
        .unwrap();
    let b = engine
        .create_highlight(book, "Patience is bitter.", None, None)
        .await
        .unwrap();
    assert_ne!(a.id, b.id);
    assert_eq!(a.text, "Patience is bitter.");
    assert_eq!(a.source, "user");
    assert_eq!(a.page, Some(12));
    assert_eq!(
        a.reading_id,
        Some(reading),
        "placed in the read it was typed during"
    );
    assert!(
        engine
            .create_highlight(book, "   ", None, None)
            .await
            .is_err()
    );
    assert!(
        engine
            .create_highlight(9_999, "x", None, None)
            .await
            .is_err()
    );
    assert_eq!(engine.list_highlights(book).await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_preview_resolves_out_and_finds_the_links_waiting_for_it() {
    let (_tmp, engine) = engine().await;
    let book = seed_book(&engine, "Middlemarch").await;
    let han = note(&engine, None, Some("Vocation"), "a calling").await;
    let waiting = note(
        &engine,
        Some(book),
        Some("Waiting"),
        "Intro.\n\nOne day [[maps that lie]] will exist.",
    )
    .await;

    let p = engine
        .preview_links(
            None,
            "# Maps That Lie\n\nAbout [[vocation]] and [[Nobody]].",
        )
        .await
        .unwrap();
    let out: Vec<_> = p
        .outgoing
        .iter()
        .map(|o| (o.target_title.as_str(), o.to.as_ref().map(|n| n.id)))
        .collect();
    assert_eq!(out, vec![("vocation", Some(han)), ("Nobody", None)]);
    assert_eq!(p.incoming.len(), 1);
    assert_eq!(p.incoming[0].note.as_ref().unwrap().id, waiting);
    assert_eq!(
        p.incoming[0].paragraph.as_ref().unwrap().text,
        "One day [[maps that lie]] will exist."
    );
    assert!(p.incoming[0].book.is_some());

    let titled = engine
        .preview_links(Some("Vocation"), "plain")
        .await
        .unwrap();
    assert!(
        titled.incoming.is_empty(),
        "Vocation's links are resolved, not waiting"
    );
    let unnamed = engine.preview_links(None, "no heading").await.unwrap();
    assert!(unnamed.incoming.is_empty() && unnamed.outgoing.is_empty());
}
