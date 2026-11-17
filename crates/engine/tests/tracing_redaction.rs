//! The one place it is worth asserting on log *output*.
//!
//! Everything else about degradation is now assertable on the returned
//! `Diagnostic`, which is stronger and does not couple tests to log text. But
//! "an API key never reaches a log" is a property of the emission itself, and
//! nothing else can check it.
//!
//! Deliberately hand-rolled rather than using `tracing-test`, which installs a
//! *global* default subscriber and thereby makes tests order-dependent and
//! hostile to parallel execution.

use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::subscriber::with_default;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::Registry;

/// Every field value rendered by any event, plus every event message.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<String>>>);

impl Captured {
    fn contents(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

struct Collector(Captured);

impl Visit for Collector {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0
            .0
            .lock()
            .unwrap()
            .push(format!("{}={:?}", field.name(), value));
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0
            .0
            .lock()
            .unwrap()
            .push(format!("{}={}", field.name(), value));
    }
}

impl<S: tracing::Subscriber> Layer<S> for Captured {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        event.record(&mut Collector(self.clone()));
    }
}

const KEY: &str = "AIzaSyTOTALLY_SECRET_KEY_VALUE";

/// `scrub_key` is the only thing standing between a reqwest error (which
/// embeds the full request URL) and a key in a log file the user might paste
/// into a bug report.
#[test]
fn an_api_key_never_reaches_a_tracing_field() {
    let captured = Captured::default();
    let subscriber = Registry::default().with(captured.clone());

    with_default(subscriber, || {
        // The shape a reqwest failure actually takes: the whole URL, key and
        // all, inside the error text.
        let leaked = format!(
            "error sending request for url (https://www.googleapis.com/books/v1/volumes?q=dune&key={KEY})"
        );
        let err = bookmole::EngineError::Provider {
            provider: bookmole::ProviderId::GoogleBooks,
            message: bookmole::providers::googlebooks::scrub_key(&leaked),
        };
        let diag = bookmole::Diagnostic::provider_failed(bookmole::ProviderId::GoogleBooks, &err);

        // Exactly how search.rs logs a degradation.
        tracing::warn!(provider = "googlebooks", detail = %diag.detail, "provider degraded");
        tracing::error!(error = %err, "provider error");
    });

    let lines = captured.contents();
    assert!(!lines.is_empty(), "the capturing layer saw no events");
    for line in &lines {
        assert!(
            !line.contains(KEY),
            "an API key reached a tracing field: {line}"
        );
        assert!(
            !line.contains("AIzaSy"),
            "a Google key prefix reached a tracing field: {line}"
        );
    }
    // And the redaction marker is present, so this is not passing merely
    // because nothing was logged.
    assert!(
        lines.iter().any(|l| l.contains("key=REDACTED")),
        "expected a redacted key marker, got: {lines:?}"
    );
}

/// The engine must never install a subscriber of its own — that is a frontend
/// decision, and a library that grabs the global default breaks every embedder.
#[test]
fn the_engine_installs_no_global_subscriber() {
    // If the engine had installed one, this would be a no-op and the layer
    // below would capture nothing.
    let captured = Captured::default();
    with_default(Registry::default().with(captured.clone()), || {
        tracing::warn!(marker = "probe", "test event");
    });
    assert!(
        captured.contents().iter().any(|l| l.contains("probe")),
        "a global subscriber was already installed, so events bypassed the test layer"
    );
}

/// A **note body and its file path** must never reach a log above `trace!`.
///
/// The engine's tracing rule names highlight text, note bodies and search
/// queries as the user's private reading, never logged above `trace!`. The
/// vault path is the subtle half and
/// is why this test exists at all: a note's filename is its slugified *title*,
/// and a derived title is the first six words of the body — so
/// `20260805-dorothea-s-dignity-under-casaubon-s.md` **is** the note's opening
/// words, and a watcher that logged the path it was working on would be logging
/// prose. The mount watcher logs its volume paths at `info!` quite correctly; a
/// vault path is not the same kind of path.
///
/// Filtered to `DEBUG` and above deliberately, so this asserts the *rule* —
/// `trace!` may carry it — rather than asserting that nothing is ever logged.
#[tokio::test]
async fn a_note_body_never_reaches_a_log_above_trace() {
    use bookmole::{Engine, EngineConfig, NewNoteInput, VaultStir, VaultWatcher};

    const SECRET: &str = "Dorothea's dignity under Casaubon's calculation";

    let tmp = tempfile::tempdir().unwrap();
    let engine = Engine::open(EngineConfig::rooted_at(tmp.path()))
        .await
        .unwrap();
    let created = engine
        .create_note(NewNoteInput {
            body: SECRET.into(),
            ..Default::default()
        })
        .await
        .unwrap();
    // Every word of the body ends up in the filename, since the title is
    // derived from it — which is the whole point of this test.
    assert!(
        created.file.to_string_lossy().contains("dignity"),
        "the fixture no longer puts the body's words in the path, so this \
         test would pass for the wrong reason"
    );

    let captured = Captured::default();
    let subscriber = Registry::default().with(captured.clone().with_filter(LevelFilter::DEBUG));

    let storage = engine.storage().clone();
    let vault = engine.vault_dir().to_path_buf();
    let path = created.file.clone();

    // `with_default` is not held across an await — the subscriber guard is not
    // `Send` — so the watcher is driven inside a `block_in_place`-free local
    // runtime instead: one current-thread runtime, entirely inside the scope.
    std::thread::scope(|s| {
        s.spawn(|| {
            tracing::subscriber::with_default(subscriber, || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                rt.block_on(async {
                    // An edit made outside, then the file removed — both of the
                    // watcher's outcomes, plus the sweep.
                    let raw = std::fs::read_to_string(&path).unwrap();
                    std::fs::write(&path, raw.replace(SECRET, "rewritten outside")).unwrap();

                    let (tx, rx) = tokio::sync::mpsc::channel(8);
                    let mut watcher = VaultWatcher::from_stirs(&vault, storage.clone(), rx)
                        .quiet_for(std::time::Duration::from_millis(30));
                    tx.send(VaultStir(path.clone())).await.unwrap();
                    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), watcher.next())
                        .await;

                    std::fs::remove_file(&path).unwrap();
                    tx.send(VaultStir(path.clone())).await.unwrap();
                    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), watcher.next())
                        .await;
                });
            });
        });
    });

    for line in captured.contents() {
        for word in ["Dorothea", "dignity", "Casaubon", "calculation"] {
            assert!(
                !line.contains(word),
                "a note's prose reached a log above trace!: {line}"
            );
        }
        assert!(
            !line.contains("vault/"),
            "a vault path reached a log above trace!: {line}"
        );
    }
}

/// A **search query** is the user's private reading too, and a new query path is
/// exactly where a helpful `info!("searching for {q}")` gets added.
///
/// The sibling of the test above, and it covers the two things a search touches
/// that the watcher does not: the words the reader typed, and the text of the
/// highlight that came back. Filtered to `DEBUG` for the same reason — this
/// asserts the *rule* (`trace!` may carry it), not that nothing is logged. The
/// count of hits is fine at `debug!`: a number is not the query.
#[test]
fn a_search_query_never_reaches_a_log_above_trace() {
    use bookmole::{Engine, EngineConfig, NewHighlight, NewNoteInput};

    const TYPED: &str = "Casaubon";
    const PASSAGE: &str = "Casaubon's calculation, which Dorothea never saw coming";

    let captured = Captured::default();
    let subscriber = Registry::default().with(captured.clone().with_filter(LevelFilter::DEBUG));

    std::thread::scope(|s| {
        s.spawn(|| {
            tracing::subscriber::with_default(subscriber, || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                rt.block_on(async {
                    let tmp = tempfile::tempdir().unwrap();
                    let engine = Engine::open(EngineConfig::rooted_at(tmp.path()))
                        .await
                        .unwrap();
                    let book = engine
                        .save_book(&bookmole::Book {
                            title: Some("Middlemarch".into()),
                            ..Default::default()
                        })
                        .await
                        .unwrap()
                        .id
                        .unwrap();
                    engine
                        .storage()
                        .insert_highlight(
                            book,
                            &NewHighlight {
                                text: PASSAGE.into(),
                                chapter: None,
                                page: None,
                                pos0: None,
                                pos1: None,
                                ko_datetime: Some("2026-01-01 10:00:00".into()),
                                ko_datetime_updated: None,
                                color: None,
                                note: None,
                                source: "koreader".into(),
                            },
                        )
                        .await
                        .unwrap();
                    engine
                        .create_note(NewNoteInput {
                            book_id: Some(book),
                            body: PASSAGE.into(),
                            ..Default::default()
                        })
                        .await
                        .unwrap();

                    let hits = engine.search_marks(TYPED, None, None, 10).await.unwrap();
                    assert_eq!(
                        hits.len(),
                        2,
                        "the fixture has to actually match, or this passes for \
                         the wrong reason"
                    );
                    // A query that matches nothing goes down the same path.
                    assert!(
                        engine
                            .search_marks("thermodynamics", None, None, 10)
                            .await
                            .unwrap()
                            .is_empty()
                    );
                });
            });
        });
    });

    for line in captured.contents() {
        for word in ["Casaubon", "Dorothea", "calculation", "thermodynamics"] {
            assert!(
                !line.contains(word),
                "a search query or a highlight reached a log above trace!: {line}"
            );
        }
    }
}

/// **The pairing token is never logged, at any level** — not even `trace!`.
///
/// Migration `0019` says so and is stricter than the engine's rule about
/// highlight text and search queries, which permits `trace!`. Wireless contact
/// is where a debug line would be reached for: the listener verifies a MAC and
/// refuses, and *"expected a38b… got 26ae…"* is the first thing anybody writes
/// when a credential will not verify on a device they cannot attach a debugger
/// to. It is also the line that puts a reproducible forgery in a log file.
///
/// So the capture is unfiltered — every level, including `TRACE` — and it
/// drives the whole rendezvous: a probe from a paired reader (which makes the
/// listener compute a MAC under the real token), a connection that proves
/// itself, and a connection that fails to. The refusal path matters most: the
/// success path has no reason to mention the secret and the failure path has an
/// excuse.
#[tokio::test]
async fn the_pairing_token_never_reaches_a_log_at_any_level() {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::Arc;

    use bookmole::wireless::{Beacon, Listener, mac, open_challenge};
    use bookmole::{Engine, EngineConfig};
    use tokio::io::AsyncWriteExt;

    /// A beacon that replays one probe and records nothing. No packet leaves
    /// the machine; see `tests/wireless.rs` for the same seam in full.
    #[derive(Debug)]
    struct OneProbe(tokio::sync::Mutex<Option<Vec<u8>>>);

    #[async_trait::async_trait]
    impl Beacon for OneProbe {
        async fn recv(&self) -> Option<(Vec<u8>, SocketAddr)> {
            match self.0.lock().await.take() {
                Some(b) => Some((b, "192.0.2.9:61862".parse().unwrap())),
                // Park rather than end: a real socket waits between datagrams.
                None => std::future::pending().await,
            }
        }
        async fn send_to(&self, _: &[u8], _: SocketAddr) -> std::io::Result<()> {
            Ok(())
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let engine = Engine::open(EngineConfig::rooted_at(tmp.path()))
        .await
        .unwrap();

    let mount = tmp.path().join("reader");
    std::fs::create_dir_all(mount.join("koreader/frontend")).unwrap();
    std::fs::create_dir_all(mount.join("koreader/plugins")).unwrap();
    std::fs::write(mount.join("koreader/reader.lua"), "").unwrap();
    let device_id = engine.install_plugin(&mount).await.unwrap().device_id;
    let token = std::fs::read_to_string(
        mount
            .join("koreader/plugins/bookmole.koplugin")
            .join("pairing.lua"),
    )
    .unwrap()
    .lines()
    .find_map(|l| l.trim().strip_prefix("token     = "))
    .unwrap()
    .trim_matches(['"', ','])
    .to_string();
    assert_eq!(token.len(), 64, "the fixture stopped being a real token");

    let captured = Captured::default();
    // **No level filter at all.** This is the one secret `trace!` may not carry.
    let subscriber = Registry::default().with(captured.clone());
    #[allow(clippy::disallowed_methods)]
    let guard = tracing::subscriber::set_default(subscriber);

    let listener = Arc::new(Listener::new());
    let probe = serde_json::to_vec(&serde_json::json!({
        "v": 1, "device_id": device_id, "nonce": "n1"
    }))
    .unwrap();
    listener
        .start(
            engine.storage().clone(),
            "desk".into(),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            Arc::new(OneProbe(tokio::sync::Mutex::new(Some(probe)))),
            Some(0),
            1_700_000_000,
        )
        .await
        .unwrap();
    let port = listener.status(0).await.tcp_port.unwrap();

    // A connection that proves itself, and one that does not.
    for (nonce, key) in [("good", token.as_str()), ("bad", "not-the-token")] {
        let mut s = tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        let open = serde_json::json!({
            "v": 1, "device_id": device_id, "nonce": nonce,
            "mac": mac(key, &open_challenge(nonce)),
        });
        let mut line = serde_json::to_vec(&open).unwrap();
        line.push(b'\n');
        s.write_all(&line).await.unwrap();
        s.flush().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    }
    listener.stop().await;
    drop(guard);

    for line in captured.contents() {
        assert!(
            !line.contains(&token),
            "the pairing token reached a log: {line}"
        );
        // A MAC computed under the real token is not the token, but publishing
        // one beside its challenge is a gift to anybody replaying it — and it
        // is what a "why did this not verify" line would print.
        assert!(
            !line.contains(&mac(&token, &open_challenge("good"))),
            "a MAC computed under the pairing token reached a log: {line}"
        );
    }
    assert!(
        !captured.contents().is_empty(),
        "nothing was captured at all, so this test proves nothing"
    );
}

/// **Book search, end to end: no query, no typed ISBN and no key above
/// `trace!`.** What a reader searched for is as private as what they read.
///
/// Every path a search now takes, against loopback servers and then against
/// ports nothing listens on: the parsed free-text search, the session started
/// and polled, a repeat answered from what was kept, the concurrent ISBN
/// lookup, an ISBN typed into the search box, the offline stand-in, and the
/// books index: the books the app already holds (`yours`), *you have this*
/// against a library holding the very book, and a search of the library
/// itself. The library and the
/// providers answer with the query's own words in their titles, so a line that
/// logged a result's title would trip this as surely as one that logged the
/// query. Filtered to `DEBUG`, as the two tests above are: `trace!` may carry
/// a query, and nothing else may.
#[test]
fn a_book_search_never_logs_the_query_the_isbn_or_the_key_above_trace() {
    use bookmole::providers::googlebooks::GoogleBooksProvider;
    use bookmole::providers::openlibrary::OpenLibraryProvider;
    use bookmole::providers::{Manners, MetadataProvider};
    use bookmole::{Engine, EngineConfig, SearchRequest, SearchScope};
    use wiremock::matchers::{method, path, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const WORDS: [&str; 5] = [
        "Zephyrine",
        "Quillfeather",
        "Brookevarnish",
        "Brookevarnsh",
        "chronicel",
    ];
    const TYPED: &str = "Brookevarnish chronicle by Zephyrine Quillfeather";
    const ISBN_TYPED: &str = "978-0-441-17271-9";
    const ISBNS: [&str; 3] = ["9780441172719", "0441172719", "978-0-441-17271-9"];

    let captured = Captured::default();
    let subscriber = Registry::default().with(captured.clone().with_filter(LevelFilter::DEBUG));

    std::thread::scope(|s| {
        s.spawn(|| {
            tracing::subscriber::with_default(subscriber, || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                rt.block_on(async {
                    let ol = MockServer::start().await;
                    Mock::given(method("GET"))
                        .and(path("/search.json"))
                        .respond_with(ResponseTemplate::new(200).set_body_string(
                            r#"{"docs":[{"key":"/works/OL1W","title":"Brookevarnish Chronicle",
                                "author_name":["Zephyrine Quillfeather"],
                                "editions":{"docs":[{"key":"/books/OL1M",
                                  "title":"Brookevarnish Chronicle","isbn":["9780441172719"]}]}}]}"#,
                        ))
                        .mount(&ol)
                        .await;
                    Mock::given(method("GET"))
                        .and(path_regex("^/isbn/"))
                        .respond_with(ResponseTemplate::new(200).set_body_string(
                            r#"{"key":"/books/OL1M","title":"Brookevarnish Chronicle",
                                "isbn_13":["9780441172719"]}"#,
                        ))
                        .mount(&ol)
                        .await;
                    let gb = MockServer::start().await;
                    Mock::given(method("GET"))
                        .respond_with(ResponseTemplate::new(200).set_body_string(
                            r#"{"items":[{"id":"g1","volumeInfo":{"title":"Brookevarnish Chronicle",
                                "authors":["Zephyrine Quillfeather"],
                                "industryIdentifiers":[{"type":"ISBN_13","identifier":"9780441172719"}]}}]}"#,
                        ))
                        .mount(&gb)
                        .await;

                    let tmp = tempfile::tempdir().unwrap();
                    let engine = Engine::open(EngineConfig {
                        google_api_key: None,
                        ..EngineConfig::rooted_at(tmp.path())
                    })
                    .await
                    .unwrap();
                    let client = reqwest::Client::builder().no_proxy().build().unwrap();
                    let providers = |ol: String, gb: String| -> Vec<Box<dyn MetadataProvider>> {
                        vec![
                            Box::new(
                                OpenLibraryProvider::with_base(client.clone(), ol)
                                    .with_manners(std::sync::Arc::new(Manners::unlimited())),
                            ),
                            Box::new(
                                GoogleBooksProvider::with_base(
                                    client.clone(),
                                    Some(KEY.into()),
                                    format!("{gb}/books/v1/volumes"),
                                )
                                .with_manners(std::sync::Arc::new(Manners::unlimited())),
                            ),
                        ]
                    };
                    engine.replace_providers(providers(ol.uri(), gb.uri()));
                    // A one-work offline index holding the book, so suggestions,
                    // correction and the index's names are on the path too.
                    // Built in code: this test never opens the
                    // embedded file.
                    engine.use_wellknown(bookmole::search::wellknown::Loader::Works(
                        std::sync::Arc::new(vec![bookmole::WellKnownWork {
                            wikidata: 1,
                            sitelinks: 12,
                            year: Some(1990),
                            ol_works: vec!["OL1W".into()],
                            author: Some("Zephyrine Quillfeather".into()),
                            titles: vec![bookmole::search::wellknown::WellKnownTitle {
                                text: "Brookevarnish Chronicle".into(),
                                langs: vec!["en".into()],
                                original: true,
                                alias: false,
                            }],
                            ..Default::default()
                        }]),
                    ));
                    // A book of the app's own that every search finds: the
                    // `yours` results and *you have this* both have it to say.
                    engine
                        .save_book(&bookmole::Book {
                            title: Some("Brookevarnish Chronicle".into()),
                            authors: vec!["Zephyrine Quillfeather".into()],
                            isbn_13: Some("9780441172719".into()),
                            ..Default::default()
                        })
                        .await
                        .unwrap();
                    let text = |t: &str| SearchRequest {
                        query: Some(t.into()),
                        limit: 20,
                        ..Default::default()
                    };

                    // Parsed, then repeated from what was kept.
                    let first = engine.search(&text(TYPED)).await.unwrap();
                    assert!(!first.results.is_empty(), "the fixture has to answer");
                    engine.search(&text(TYPED)).await.unwrap();
                    // A session, started and polled to its end.
                    let mut snap = engine
                        .start_search(TYPED, SearchScope::Everything, 20)
                        .await
                        .unwrap();
                    assert!(!snap.yours.is_empty(), "the Yours path was not taken");
                    while !snap.done {
                        snap = engine
                            .poll_search(&snap.session, snap.revision, std::time::Duration::from_secs(1))
                            .await
                            .unwrap();
                    }
                    assert!(
                        snap.works.iter().any(|r| r.work.held.book_id().is_some()),
                        "the held path was not taken"
                    );
                    engine.end_search(&snap.session);
                    // As you type, and a misspelling corrected: the
                    // fix is the user's query too.
                    let suggested = engine.suggest_works("Brookev", 5).await.unwrap();
                    assert!(!suggested.suggestions.is_empty(), "no suggestion path");
                    let mut fixed = engine
                        .start_search("Brookevarnsh chronicel", SearchScope::Everything, 20)
                        .await
                        .unwrap();
                    while !fixed.done {
                        fixed = engine
                            .poll_search(&fixed.session, fixed.revision, std::time::Duration::from_secs(1))
                            .await
                            .unwrap();
                    }
                    // The fix's own work is the first answer here, so it is
                    // weighed and not used; the marker below proves the weighing.
                    assert!(fixed.correction.is_none());
                    engine
                        .start_search_as_typed("Brookevarnsh chronicel", SearchScope::Everything, 20)
                        .await
                        .unwrap();
                    // The library's own search, by words, by a typo and by ISBN.
                    for q in [TYPED, "Brookevarnsh", ISBN_TYPED] {
                        engine.search_library(q, 20).await.unwrap();
                    }
                    // The concurrent ISBN lookup, and an ISBN typed as text.
                    let found = engine.lookup_isbn_reported(ISBN_TYPED).await.unwrap();
                    assert!(found.book.is_some());
                    engine.search(&text(ISBN_TYPED)).await.unwrap();

                    // Offline: kept answers stand in for unreachable hosts.
                    engine.storage().age_kept_searches(3 * 86_400).await.unwrap();
                    let dead = || {
                        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                        let port = l.local_addr().unwrap().port();
                        drop(l);
                        format!("http://127.0.0.1:{port}")
                    };
                    engine.replace_providers(providers(dead(), dead()));
                    let offline = engine.search(&text(TYPED)).await.unwrap();
                    assert!(
                        offline.warnings.iter().any(|d| matches!(
                            d.kind,
                            bookmole::DiagnosticKind::ProviderAnsweredFromCache { .. }
                        )),
                        "the offline path was not taken: {:?}",
                        offline.warnings
                    );
                    for d in &offline.warnings {
                        let line = d.to_string();
                        for w in WORDS.iter().chain(ISBNS.iter()) {
                            assert!(!line.contains(w), "a diagnostic carried the query: {line}");
                        }
                        assert!(!line.contains("AIzaSy"), "{line}");
                    }
                    engine.search(&text(ISBN_TYPED)).await.unwrap();
                    engine
                        .start_search("Quillfeather", SearchScope::LocalOnly, 20)
                        .await
                        .unwrap();
                });
            });
        });
    });

    let lines = captured.contents();
    // The paths were taken and logged: the session's start and the offline
    // degradation, at least.
    for marker in [
        "search started",
        "provider degraded",
        "books searched",
        "a candidate correction was weighed",
    ] {
        assert!(
            lines.iter().any(|l| l.contains(marker)),
            "no {marker:?} line, so this proves nothing: {lines:?}"
        );
    }
    for line in lines {
        for w in WORDS.iter().chain(ISBNS.iter()) {
            assert!(
                !line.contains(w),
                "a query or a typed ISBN reached a log above trace!: {line}"
            );
        }
        assert!(
            !line.contains(KEY) && !line.contains("AIzaSy"),
            "a key: {line}"
        );
    }
}
