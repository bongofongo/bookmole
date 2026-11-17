//! The wireless rendezvous end to end. No broadcast packet is emitted (UDP goes through
//! `ScriptedBeacon`); the TCP half is a real loopback socket.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use bookmole::wireless::{
    Ack, Beacon, Hello, Here, Listener, ListenerMode, Open, PushFrame, body_challenge,
    here_challenge, mac, open_challenge,
};
use bookmole::{Engine, EngineConfig};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// `storage::now_unix` is `pub(crate)` on purpose; the facade must not expose the clock.
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
/// Carries a `partial_md5_checksum`, which makes its import idempotent.
const SIDECAR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/koreader/synthetic/Multi-Chapter.sdr/metadata.epub.lua"
);

/// No checksum, so there is nothing to key a mapping on and no de-duplication.
const UNIDENTIFIED_SIDECAR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/koreader/synthetic/The-Trial.sdr/metadata.epub.lua"
);

#[derive(Debug, Default)]
struct ScriptedBeacon {
    probes: Mutex<Vec<(Vec<u8>, SocketAddr)>>,
    replies: Mutex<Vec<(Vec<u8>, SocketAddr)>>,
    /// Held open so the responder parks rather than ending, as a real socket does between datagrams.
    exhausted: tokio::sync::Notify,
}

#[async_trait::async_trait]
impl Beacon for ScriptedBeacon {
    async fn recv(&self) -> Option<(Vec<u8>, SocketAddr)> {
        loop {
            if let Some(next) = self.probes.lock().await.pop() {
                return Some(next);
            }
            self.exhausted.notified().await;
        }
    }
    async fn send_to(&self, bytes: &[u8], to: SocketAddr) -> std::io::Result<()> {
        self.replies.lock().await.push((bytes.to_vec(), to));
        Ok(())
    }
}

impl ScriptedBeacon {
    async fn probe(&self, hello: &Hello) {
        let from: SocketAddr = "192.0.2.55:61862".parse().unwrap();
        self.probes
            .lock()
            .await
            .push((serde_json::to_vec(hello).unwrap(), from));
        self.exhausted.notify_waiters();
    }

    async fn answers(&self) -> Vec<Here> {
        for _ in 0..200 {
            {
                let got = self.replies.lock().await;
                if !got.is_empty() {
                    return got
                        .iter()
                        .map(|(b, _)| serde_json::from_slice(b).unwrap())
                        .collect();
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        Vec::new()
    }

    async fn silent(&self) -> bool {
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        self.replies.lock().await.is_empty()
    }
}

async fn engine() -> (tempfile::TempDir, Engine) {
    let tmp = tempfile::tempdir().unwrap();
    let config = EngineConfig {
        db_url: "sqlite::memory:".into(),
        images_dir: tmp.path().join("database/images"),
        files_dir: tmp.path().join("database/files"),
        vault_dir: tmp.path().join("vault"),
        log_dir: tmp.path().join("logs"),
        exports_dir: tmp.path().join("exports"),
        google_api_key: None,
        calibre_bin_dir: None,
    };
    (tmp, Engine::open(config).await.unwrap())
}

async fn pair(engine: &Engine, tmp: &std::path::Path) -> (String, String) {
    let mount = tmp.join("reader");
    std::fs::create_dir_all(mount.join("koreader/frontend")).unwrap();
    std::fs::create_dir_all(mount.join("koreader/plugins")).unwrap();
    std::fs::write(mount.join("koreader/reader.lua"), "").unwrap();
    let report = engine.install_plugin(&mount).await.unwrap();
    let src = std::fs::read_to_string(
        mount
            .join("koreader/plugins/bookmole.koplugin")
            .join("pairing.lua"),
    )
    .unwrap();
    let token = src
        .lines()
        .find_map(|l| l.trim().strip_prefix("token     = "))
        .unwrap()
        .trim_matches(['"', ','])
        .to_string();
    (report.device_id, token)
}

async fn start(listener: &Arc<Listener>, engine: &Engine, beacon: Arc<ScriptedBeacon>, mins: u32) {
    listener
        .start(
            engine.storage().clone(),
            "test-desk".into(),
            LOOPBACK,
            beacon,
            Some(mins),
            unix_now(),
        )
        .await
        .unwrap();
}

struct Reader {
    stream: BufReader<TcpStream>,
    nonce: String,
    token: String,
}

impl Reader {
    async fn connect(port: u16, device_id: &str, token: &str, nonce: &str) -> (Self, Ack) {
        let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        let mut me = Reader {
            stream: BufReader::new(stream),
            nonce: nonce.into(),
            token: token.into(),
        };
        let open = Open {
            v: 1,
            device_id: device_id.into(),
            nonce: nonce.into(),
            mac: mac(token, &open_challenge(nonce)),
        };
        me.send(&open).await;
        let ack = me.ack().await;
        (me, ack)
    }

    async fn send<T: serde::Serialize>(&mut self, v: &T) {
        let mut line = serde_json::to_vec(v).unwrap();
        line.push(b'\n');
        self.stream.get_mut().write_all(&line).await.unwrap();
        self.stream.get_mut().flush().await.unwrap();
    }

    async fn ack(&mut self) -> Ack {
        let mut line = String::new();
        self.stream.read_line(&mut line).await.unwrap();
        serde_json::from_str(line.trim_end()).unwrap_or(Ack {
            ok: false,
            error: Some("no reply".into()),
        })
    }

    async fn push(&mut self, name: &str, body: &str) -> Ack {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(body.as_bytes());
        let hex = digest.iter().fold(String::new(), |mut s, b| {
            use std::fmt::Write as _;
            let _ = write!(s, "{b:02x}");
            s
        });
        let frame = PushFrame::Entry {
            name: name.into(),
            len: body.len() as u64,
            sha256: hex.clone(),
            mac: mac(&self.token, &body_challenge(&self.nonce, &hex)),
        };
        self.send(&frame).await;
        self.stream
            .get_mut()
            .write_all(body.as_bytes())
            .await
            .unwrap();
        self.stream.get_mut().flush().await.unwrap();
        self.ack().await
    }

    async fn done(&mut self) -> Ack {
        self.send(&PushFrame::Done).await;
        self.ack().await
    }
}

#[tokio::test]
async fn a_paired_reader_finds_us_and_pushes_its_highlights() {
    let (tmp, engine) = engine().await;
    let (device_id, token) = pair(&engine, tmp.path()).await;
    let listener = Arc::new(Listener::new());
    let beacon = Arc::new(ScriptedBeacon::default());
    start(&listener, &engine, Arc::clone(&beacon), 5).await;

    beacon
        .probe(&Hello {
            v: 1,
            device_id: device_id.clone(),
            nonce: "n1".into(),
        })
        .await;
    let here = beacon.answers().await;
    assert_eq!(here.len(), 1);
    assert_eq!(here[0].name, "test-desk");
    assert_eq!(
        here[0].mac,
        mac(&token, &here_challenge("n1", here[0].tcp_port)),
        "the reader verifies identity, not address"
    );

    let body = std::fs::read_to_string(SIDECAR).unwrap();
    let (mut reader, ack) = Reader::connect(here[0].tcp_port, &device_id, &token, "push-1").await;
    assert!(ack.ok, "{ack:?}");
    assert!(
        reader
            .push("The-Trial.sdr/metadata.epub.lua", &body)
            .await
            .ok
    );
    assert!(reader.done().await.ok);

    let books = engine.list_books(&Default::default()).await.unwrap();
    assert_eq!(books.len(), 1, "the sidecar's own book was created");
    let marks = engine.list_highlights(books[0].id.unwrap()).await.unwrap();
    assert!(!marks.is_empty(), "and its highlights came with it");

    let device = engine
        .paired_devices()
        .await
        .unwrap()
        .into_iter()
        .find(|d| d.device_id == device_id)
        .unwrap();
    assert!(device.last_wireless_at.is_some());
    assert_eq!(device.last_lan_addr.as_deref(), Some("127.0.0.1"));
    assert_eq!(
        device.last_synced_at, None,
        "what a reader chose to send is not everything it had"
    );
}

#[tokio::test]
async fn pushing_the_same_thing_twice_changes_nothing() {
    let (tmp, engine) = engine().await;
    let (device_id, token) = pair(&engine, tmp.path()).await;
    let listener = Arc::new(Listener::new());
    let body = std::fs::read_to_string(SIDECAR).unwrap();

    let mut counts = Vec::new();
    for nonce in ["a", "b"] {
        let beacon = Arc::new(ScriptedBeacon::default());
        // `Some(0)`, always: a window would close after the first push and the second could not run.
        listener
            .start(
                engine.storage().clone(),
                "desk".into(),
                LOOPBACK,
                beacon,
                Some(0),
                unix_now(),
            )
            .await
            .unwrap();
        let port = listener.status(0).await.tcp_port.unwrap();
        let (mut reader, ack) = Reader::connect(port, &device_id, &token, nonce).await;
        assert!(ack.ok);
        assert!(
            reader
                .push("The-Trial.sdr/metadata.epub.lua", &body)
                .await
                .ok
        );
        assert!(reader.done().await.ok);

        let books = engine.list_books(&Default::default()).await.unwrap();
        let marks = engine.list_highlights(books[0].id.unwrap()).await.unwrap();
        counts.push((books.len(), marks.len()));
    }
    assert_eq!(counts[0], counts[1], "a second push created something");
    listener.stop().await;
}

/// A sidecar without a `partial_md5_checksum` is not idempotent: the import creates a book and
/// warns `SidecarNotIdentified`, over the wire as over a cable.
#[tokio::test]
async fn a_sidecar_with_no_checksum_is_no_more_idempotent_over_the_wire() {
    let (tmp, engine) = engine().await;
    let (device_id, token) = pair(&engine, tmp.path()).await;
    let listener = Arc::new(Listener::new());
    let body = std::fs::read_to_string(UNIDENTIFIED_SIDECAR).unwrap();

    for nonce in ["a", "b"] {
        listener
            .start(
                engine.storage().clone(),
                "desk".into(),
                LOOPBACK,
                Arc::new(ScriptedBeacon::default()),
                Some(0),
                unix_now(),
            )
            .await
            .unwrap();
        let port = listener.status(0).await.tcp_port.unwrap();
        let (mut reader, ack) = Reader::connect(port, &device_id, &token, nonce).await;
        assert!(ack.ok);
        assert!(
            reader
                .push("The-Trial.sdr/metadata.epub.lua", &body)
                .await
                .ok
        );
        assert!(reader.done().await.ok);
    }
    assert_eq!(
        engine.list_books(&Default::default()).await.unwrap().len(),
        2,
        "this is the documented cable behaviour, not a wireless bug"
    );
    listener.stop().await;
}

#[tokio::test]
async fn a_stranger_is_answered_with_silence_and_refused_on_connect() {
    let (tmp, engine) = engine().await;
    let (device_id, token) = pair(&engine, tmp.path()).await;
    let listener = Arc::new(Listener::new());
    let beacon = Arc::new(ScriptedBeacon::default());
    start(&listener, &engine, Arc::clone(&beacon), 5).await;
    let port = listener.status(0).await.tcp_port.unwrap();

    // A probe naming an unpaired device gets silence, not a refusal, which would confirm we exist.
    beacon
        .probe(&Hello {
            v: 1,
            device_id: "somebody-elses-reader".into(),
            nonce: "n".into(),
        })
        .await;
    assert!(beacon.silent().await, "an unpaired probe learned we exist");

    let (_, ack) = Reader::connect(port, &device_id, "not-the-token", "n2").await;
    assert!(!ack.ok);
    assert!(ack.error.unwrap().contains("prove"), "and it is told why");

    let (_, ack) = Reader::connect(port, "nobody", &token, "n3").await;
    assert!(!ack.ok);

    let (_, ack) = Reader::connect(port, &device_id, &token, "n4").await;
    assert!(ack.ok);
}

#[tokio::test]
async fn a_replayed_nonce_is_refused() {
    let (tmp, engine) = engine().await;
    let (device_id, token) = pair(&engine, tmp.path()).await;
    let listener = Arc::new(Listener::new());
    // `Always`: in a window the first completed session closes the door, leaving a replay nothing
    // to reach.
    start(&listener, &engine, Arc::new(ScriptedBeacon::default()), 0).await;
    let port = listener.status(0).await.tcp_port.unwrap();

    let (mut first, ack) = Reader::connect(port, &device_id, &token, "same").await;
    assert!(ack.ok);
    assert!(first.done().await.ok);

    let (_, ack) = Reader::connect(port, &device_id, &token, "same").await;
    assert!(
        !ack.ok,
        "a nonce this listener already honoured was accepted"
    );
    assert!(ack.error.unwrap().contains("already used"));

    let (_, ack) = Reader::connect(port, &device_id, &token, "different").await;
    assert!(ack.ok);
    listener.stop().await;
}

/// The nonce set is per open door: closing clears it, so a desktop left open for weeks does
/// not accumulate an unbounded set.
#[tokio::test]
async fn closing_the_door_forgets_the_nonces_it_honoured() {
    let (tmp, engine) = engine().await;
    let (device_id, token) = pair(&engine, tmp.path()).await;
    let listener = Arc::new(Listener::new());

    for _ in 0..2 {
        start(&listener, &engine, Arc::new(ScriptedBeacon::default()), 5).await;
        let port = listener.status(0).await.tcp_port.unwrap();
        let (mut r, ack) = Reader::connect(port, &device_id, &token, "same-every-time").await;
        assert!(ack.ok, "a fresh window remembered the last one's nonces");
        assert!(r.done().await.ok);
    }
}

#[tokio::test]
async fn a_swapped_body_is_refused_before_it_is_parsed() {
    let (tmp, engine) = engine().await;
    let (device_id, token) = pair(&engine, tmp.path()).await;
    let listener = Arc::new(Listener::new());
    start(&listener, &engine, Arc::new(ScriptedBeacon::default()), 5).await;
    let port = listener.status(0).await.tcp_port.unwrap();

    let (mut reader, ack) = Reader::connect(port, &device_id, &token, "n").await;
    assert!(ack.ok);

    let body = "return { }";
    let frame = PushFrame::Entry {
        name: "swapped.lua".into(),
        len: body.len() as u64,
        sha256: "00".repeat(32),
        mac: mac(&token, &body_challenge("n", &"00".repeat(32))),
    };
    reader.send(&frame).await;
    reader
        .stream
        .get_mut()
        .write_all(body.as_bytes())
        .await
        .unwrap();
    let ack = reader.ack().await;
    assert!(!ack.ok);
    assert!(
        engine
            .list_books(&Default::default())
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn with_the_listener_off_there_is_nothing_to_find() {
    let (tmp, engine) = engine().await;
    let (device_id, _) = pair(&engine, tmp.path()).await;
    assert_eq!(
        engine.listener_status().await.unwrap().mode,
        ListenerMode::Off,
        "nothing is bound until somebody asks"
    );
    assert_eq!(engine.listener_status().await.unwrap().tcp_port, None);

    let listener = Arc::new(Listener::new());
    let beacon = Arc::new(ScriptedBeacon::default());
    start(&listener, &engine, Arc::clone(&beacon), 5).await;
    let port = listener.status(0).await.tcp_port.unwrap();
    listener.stop().await;

    beacon
        .probe(&Hello {
            v: 1,
            device_id,
            nonce: "n".into(),
        })
        .await;
    assert!(beacon.silent().await);
    assert!(
        TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .is_err(),
        "the tcp listener outlived the stop"
    );
    assert_eq!(listener.status(0).await.mode, ListenerMode::Off);

    // Stopping twice is not an error: the window may have expired before the user pressed stop.
    assert_eq!(listener.stop().await.mode, ListenerMode::Off);
    assert_eq!(
        engine.stop_listening().await.unwrap().mode,
        ListenerMode::Off
    );
}

#[tokio::test]
async fn a_completed_push_closes_the_window_it_came_through() {
    let (tmp, engine) = engine().await;
    let (device_id, token) = pair(&engine, tmp.path()).await;
    let listener = Arc::new(Listener::new());
    start(&listener, &engine, Arc::new(ScriptedBeacon::default()), 5).await;
    let port = listener.status(0).await.tcp_port.unwrap();

    let (mut reader, ack) = Reader::connect(port, &device_id, &token, "n").await;
    assert!(ack.ok);
    let body = std::fs::read_to_string(SIDECAR).unwrap();
    assert!(reader.push("a.lua", &body).await.ok);
    assert!(reader.done().await.ok);

    let after = listener.status(unix_now()).await;
    assert_eq!(after.mode, ListenerMode::Off, "the door stayed open");
    assert_eq!(after.tcp_port, None);
    assert_eq!(after.pushes, 1);
    assert!(after.last_push_at.is_some());
    assert!(
        TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .is_err(),
        "the socket outlived the window"
    );
}

#[tokio::test]
async fn a_bad_sidecar_is_reported_and_the_session_continues() {
    let (tmp, engine) = engine().await;
    let (device_id, token) = pair(&engine, tmp.path()).await;
    let listener = Arc::new(Listener::new());
    start(&listener, &engine, Arc::new(ScriptedBeacon::default()), 5).await;
    let port = listener.status(0).await.tcp_port.unwrap();

    let (mut reader, ack) = Reader::connect(port, &device_id, &token, "n").await;
    assert!(ack.ok);
    let bad = reader
        .push("broken.lua", "this is not lua at all {{{")
        .await;
    assert!(!bad.ok, "a sidecar that cannot be parsed was accepted");

    let body = std::fs::read_to_string(SIDECAR).unwrap();
    assert!(
        reader.push("good.lua", &body).await.ok,
        "one bad file ended the session"
    );
    assert!(reader.done().await.ok);
    assert_eq!(
        engine.list_books(&Default::default()).await.unwrap().len(),
        1
    );
}

async fn fake_reader(
    token: String,
    bodies: Vec<(String, String)>,
) -> (u16, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut reader = BufReader::new(read);

        // Framed as `SimpleTCPServer` does: KOReader hands a request to its callback only on an empty
        // line, so a double accepting a bare line would hide a missing terminator.
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let open: Open = serde_json::from_str(line.trim_end()).unwrap();
        let mut terminator = String::new();
        reader.read_line(&mut terminator).await.unwrap();
        assert_eq!(
            terminator.trim_end(),
            "",
            "a pull request ends with a blank line; without one KOReader's \
             SimpleTCPServer never calls its receiveCallback at all"
        );
        // The reader verifies the desktop exactly as the desktop verifies the reader.
        assert_eq!(open.mac, mac(&token, &open_challenge(&open.nonce)));
        write.write_all(b"{\"ok\":true}\n").await.unwrap();

        for (name, body) in &bodies {
            use sha2::{Digest, Sha256};
            let hex = Sha256::digest(body.as_bytes())
                .iter()
                .fold(String::new(), |mut s, b| {
                    use std::fmt::Write as _;
                    let _ = write!(s, "{b:02x}");
                    s
                });
            let frame = PushFrame::Entry {
                name: name.clone(),
                len: body.len() as u64,
                sha256: hex.clone(),
                mac: mac(&token, &body_challenge(&open.nonce, &hex)),
            };
            let mut out = serde_json::to_vec(&frame).unwrap();
            out.push(b'\n');
            write.write_all(&out).await.unwrap();
            write.write_all(body.as_bytes()).await.unwrap();
            write.flush().await.unwrap();
            let mut ack = String::new();
            reader.read_line(&mut ack).await.unwrap();
        }
        let mut out = serde_json::to_vec(&PushFrame::Done).unwrap();
        out.push(b'\n');
        write.write_all(&out).await.unwrap();
        write.flush().await.unwrap();
    });
    (port, task)
}

#[derive(Debug)]
struct Responder {
    token: String,
    tcp_port: u16,
    /// Sign the reply with this instead, to play a rogue.
    key: Option<String>,
    replies: Mutex<Vec<(Vec<u8>, SocketAddr)>>,
}

impl Responder {
    fn new(token: &str, tcp_port: u16) -> Self {
        Responder {
            token: token.into(),
            tcp_port,
            key: None,
            replies: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl Beacon for Responder {
    async fn recv(&self) -> Option<(Vec<u8>, SocketAddr)> {
        if let Some(next) = self.replies.lock().await.pop() {
            return Some(next);
        }
        std::future::pending().await
    }
    async fn send_to(&self, bytes: &[u8], _to: SocketAddr) -> std::io::Result<()> {
        let hello: Hello = serde_json::from_slice(bytes).unwrap();
        let key = self.key.clone().unwrap_or_else(|| self.token.clone());
        let here = Here {
            v: 1,
            name: "Kindle".into(),
            tcp_port: self.tcp_port,
            mac: mac(&key, &here_challenge(&hello.nonce, self.tcp_port)),
        };
        self.replies.lock().await.push((
            serde_json::to_vec(&here).unwrap(),
            SocketAddr::new(LOOPBACK, 61862),
        ));
        Ok(())
    }
}

async fn device_row(engine: &Engine, device_id: &str) -> bookmole::PairedDevice {
    engine
        .paired_devices()
        .await
        .unwrap()
        .into_iter()
        .find(|d| d.device_id == device_id)
        .unwrap()
}

#[tokio::test]
async fn the_desktop_can_pull_from_a_reader_whose_window_is_open() {
    let (tmp, engine) = engine().await;
    let (device_id, token) = pair(&engine, tmp.path()).await;
    let body = std::fs::read_to_string(SIDECAR).unwrap();
    let (port, task) = fake_reader(
        token.clone(),
        vec![("Multi-Chapter.sdr/metadata.epub.lua".into(), body)],
    )
    .await;

    let device = device_row(&engine, &device_id).await;
    let beacon = Responder::new(&token, port);
    let report = bookmole::wireless::pull_from(
        engine.storage(),
        &device,
        &beacon,
        &[SocketAddr::new(LOOPBACK, 61862)],
        "pull-1",
    )
    .await
    .unwrap();
    task.await.unwrap();

    assert_eq!(report.pulled.len(), 1);
    let books = engine.list_books(&Default::default()).await.unwrap();
    assert_eq!(books.len(), 1);
    assert!(
        !engine
            .list_highlights(books[0].id.unwrap())
            .await
            .unwrap()
            .is_empty()
    );

    let after = device_row(&engine, &device_id).await;
    assert!(after.last_wireless_at.is_some());
    assert_eq!(after.last_lan_addr.as_deref(), Some("127.0.0.1"));
    assert_eq!(after.last_synced_at, None);

    assert_eq!(
        engine.listener_status().await.unwrap().mode,
        ListenerMode::Off
    );
}

#[tokio::test]
async fn a_responder_that_cannot_prove_the_token_is_never_connected_to() {
    let (tmp, engine) = engine().await;
    let (device_id, token) = pair(&engine, tmp.path()).await;

    let trap = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let port = trap.local_addr().unwrap().port();
    let connected = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = Arc::clone(&connected);
    tokio::spawn(async move {
        if trap.accept().await.is_ok() {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    });

    let device = device_row(&engine, &device_id).await;
    let mut beacon = Responder::new(&token, port);
    beacon.key = Some("a-rogue-on-the-cafe-lan".into());

    // The seeker keeps reading rather than trusting the first reply, so a lone rogue leaves it
    // waiting until the real socket's deadline.
    let outcome = tokio::time::timeout(
        std::time::Duration::from_millis(300),
        bookmole::wireless::pull_from(
            engine.storage(),
            &device,
            &beacon,
            &[SocketAddr::new(LOOPBACK, 61862)],
            "n",
        ),
    )
    .await;
    if let Ok(r) = outcome {
        assert!(r.is_err(), "a rogue's answer was believed");
    }
    assert!(
        !connected.load(std::sync::atomic::Ordering::SeqCst),
        "the desktop dialled a responder that could not prove the token"
    );
    assert!(
        engine
            .list_books(&Default::default())
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_reader_whose_window_is_shut_is_a_refusal_that_says_what_to_do() {
    let (tmp, engine) = engine().await;
    let (device_id, _) = pair(&engine, tmp.path()).await;

    #[derive(Debug)]
    struct Silence;
    #[async_trait::async_trait]
    impl Beacon for Silence {
        async fn recv(&self) -> Option<(Vec<u8>, SocketAddr)> {
            None
        }
        async fn send_to(&self, _: &[u8], _: SocketAddr) -> std::io::Result<()> {
            Ok(())
        }
    }

    let device = device_row(&engine, &device_id).await;
    let err = bookmole::wireless::pull_from(
        engine.storage(),
        &device,
        &Silence,
        &[SocketAddr::new(LOOPBACK, 61862)],
        "n",
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("window"), "{err}");

    assert_eq!(device_row(&engine, &device_id).await.last_wireless_at, None);
}

/// The refusal lands before any packet is built: there is no token to challenge with.
#[tokio::test]
async fn pulling_from_a_reader_we_never_paired_with_is_refused() {
    let (_tmp, engine) = engine().await;
    let err = engine.pull_from_reader("nobody").await.unwrap_err();
    assert!(err.to_string().contains("paired"), "{err}");
}
