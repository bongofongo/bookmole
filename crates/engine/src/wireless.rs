//! A paired reader reaching its paired computer over the LAN, feeding the cable path's import.
//! The pairing token never crosses the wire: both sides prove possession by HMAC, and every MAC
//! is domain-separated (`here:`, `open:`, `body:`) so one cannot be replayed as another.

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::diagnostic::Diagnostic;
use crate::error::Result;
use crate::koreader::{PullReport, import_book_from_sidecar_src_for_device};
use crate::storage::{PairedDevice, Storage, now_unix};

/// Fixed, not configurable: the tested value must be the shipped value. The TCP port is
/// announced in the reply.
pub const RENDEZVOUS_PORT: u16 = 61862;

/// Bumped when a message changes shape; both sides refuse a version they do not know.
pub const PROTOCOL_VERSION: u32 = 1;

const DEFAULT_WINDOW_MINUTES: u32 = 5;

/// Bounds what one entry may declare, so a TCP peer cannot make us allocate arbitrarily.
const MAX_SIDECAR_BYTES: u64 = 4 * 1024 * 1024;

const MAX_HEADER_BYTES: u64 = 64 * 1024;

/// The UDP responder and the TCP listener share one lifetime: both are owned by one `Running`,
/// whose `Drop` aborts both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListenerMode {
    Off,
    Window { until: i64 },
    Always,
}

impl ListenerMode {
    pub fn is_open(&self, now: i64) -> bool {
        match self {
            ListenerMode::Off => false,
            ListenerMode::Window { until } => now < *until,
            ListenerMode::Always => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenerStatus {
    pub mode: ListenerMode,
    pub tcp_port: Option<u16>,
    /// Completed pushes since start; never a count of anything outstanding.
    pub pushes: u64,
    pub last_push_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WirelessRefusal {
    #[error("bookmole is not listening")]
    NotListening,
    /// Answered with silence over UDP, so a stranger's probe learns nothing.
    #[error("no reader is paired with this bookmole under that id")]
    UnknownDevice,
    /// Deliberately carries no detail about which byte differed.
    #[error("that reader could not prove it holds the pairing token")]
    BadCredential,
    #[error("that request was already used")]
    ReplayedNonce,
    #[error("this reader speaks wireless protocol v{theirs}; this bookmole speaks v{ours}")]
    UnsupportedVersion { theirs: u32, ours: u32 },
    #[error("no paired reader answered; is its window open?")]
    ReaderNotFound,
    #[error(
        "that is not a pairing code: it is {PAIRING_CODE_LEN} letters and digits, as the reader shows them"
    )]
    NotAPairingCode,
    #[error(
        "no reader answered that code; check it against the reader's screen, that the code is still showing, and that both are on the same wifi"
    )]
    PairingNotAnswered,
    /// Re-keying on the code alone would let any reader claim another's id; the old token must be
    /// proven.
    #[error(
        "that reader answered as one this computer already knows, but could not prove it; on the reader, unlink this computer under Linked computers, then link again"
    )]
    RekeyNotProven,
    #[error("{host} is not an address this computer can find")]
    AddressNotFound { host: String },
    #[error("malformed wireless message: {0}")]
    Malformed(String),
    #[error("a pushed sidecar declared {len} bytes; the limit is {max}")]
    TooLarge { len: u64, max: u64 },
}

/// HMAC-SHA256 of `message` under `token`, lowercase hex. `token` is used as its own 64 hex
/// characters, not the 32 bytes they encode.
pub fn mac(token: &str, message: &str) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(token.as_bytes())
        .expect("hmac accepts a key of any length");
    m.update(message.as_bytes());
    let out = m.finalize().into_bytes();
    use std::fmt::Write as _;
    out.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

pub fn verify(token: &str, message: &str, presented: &str) -> bool {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let Ok(raw) = hex_to_bytes(presented) else {
        return false;
    };
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(token.as_bytes())
        .expect("hmac accepts a key of any length");
    m.update(message.as_bytes());
    m.verify_slice(&raw).is_ok()
}

fn hex_to_bytes(s: &str) -> std::result::Result<Vec<u8>, ()> {
    if !s.len().is_multiple_of(2) {
        return Err(());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| ()))
        .collect()
}

/// Binds the announced port, so a captured reply cannot be re-advertised elsewhere.
pub fn here_challenge(nonce: &str, tcp_port: u16) -> String {
    format!("here:{PROTOCOL_VERSION}:{nonce}:{tcp_port}")
}

pub fn open_challenge(nonce: &str) -> String {
    format!("open:{PROTOCOL_VERSION}:{nonce}")
}

/// Covers the body's hash, so the credential is bound to the payload and not just the session.
pub fn body_challenge(nonce: &str, sha256_hex: &str) -> String {
    format!("body:{PROTOCOL_VERSION}:{nonce}:{sha256_hex}")
}

pub const PAIRING_CODE_LEN: usize = 16;

const CODE_ALPHABET: &str = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Bounded because the message is one datagram.
const MAX_KNOWN_IDS: usize = 16;

const PAIR_ATTEMPTS: u32 = 8;

/// Drops case, spaces and hyphens; reads `O` as `0` and `I`/`L` as `1`.
pub fn normalise_code(typed: &str) -> Option<String> {
    let mut out = String::with_capacity(PAIRING_CODE_LEN);
    for c in typed.chars() {
        let c = match c.to_ascii_uppercase() {
            ' ' | '-' | '\t' => continue,
            'O' => '0',
            'I' | 'L' => '1',
            c => c,
        };
        if !CODE_ALPHABET.contains(c) {
            return None;
        }
        out.push(c);
    }
    (out.len() == PAIRING_CODE_LEN).then_some(out)
}

/// `name` is last because a hostname may hold a colon.
pub fn pair_challenge(nonce: &str, at: i64, known: &[String], name: &str) -> String {
    format!(
        "pair:{PROTOCOL_VERSION}:{nonce}:{at}:{}:{name}",
        known.join(",")
    )
}

/// Covers our nonce, so a reply recorded from one attempt proves nothing in another.
pub fn paired_challenge(
    pair_nonce: &str,
    reader_nonce: &str,
    device_id: &str,
    plugin: i64,
    reader: &str,
) -> String {
    format!("paired:{PROTOCOL_VERSION}:{pair_nonce}:{reader_nonce}:{device_id}:{plugin}:{reader}")
}

pub fn rekey_challenge(pair_nonce: &str, reader_nonce: &str, device_id: &str) -> String {
    format!("rekey:{PROTOCOL_VERSION}:{pair_nonce}:{reader_nonce}:{device_id}")
}

pub fn token_challenge(pair_nonce: &str, reader_nonce: &str, device_id: &str) -> String {
    format!("token:{PROTOCOL_VERSION}:{pair_nonce}:{reader_nonce}:{device_id}")
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

// These three messages are the whole surface a LAN reader reaches; never accept
// `bookmole_api::Call` here.

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Hello {
    pub v: u32,
    pub device_id: String,
    pub nonce: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Here {
    pub v: u32,
    pub name: String,
    pub tcp_port: u16,
    pub mac: String,
}

/// Carries no `device_id`, so listeners and pull windows do not mistake it for a `Hello` probe.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Pair {
    pub v: u32,
    pub kind: String,
    pub nonce: String,
    pub at: i64,
    pub name: String,
    pub known: Vec<String>,
    pub mac: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Paired {
    pub v: u32,
    pub kind: String,
    pub nonce: String,
    pub device_id: String,
    #[serde(default)]
    pub reader: String,
    #[serde(default)]
    pub plugin: i64,
    pub mac: String,
    /// Not signed with the code: it proves possession of the token the reader already holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Open {
    pub v: u32,
    pub device_id: String,
    pub nonce: String,
    pub mac: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PushFrame {
    Entry {
        /// Diagnostics only; never used as a path or handle.
        name: String,
        len: u64,
        sha256: String,
        mac: String,
    },
    Done,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Ack {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Ack {
    fn ok() -> Self {
        Ack {
            ok: true,
            error: None,
        }
    }
    fn refused(r: &WirelessRefusal) -> Self {
        Ack {
            ok: false,
            error: Some(r.to_string()),
        }
    }
}

/// Entries always flow reader to desktop in both push and pull, so wireless is read-only toward
/// us by shape.
#[derive(Debug, Default)]
pub struct WirelessReport {
    pub device_id: String,
    pub pulled: Vec<PullReport>,
    pub warnings: Vec<Diagnostic>,
}

/// The UDP half, behind a trait so tests send no broadcast packets.
#[async_trait::async_trait]
pub trait Beacon: Send + Sync + std::fmt::Debug {
    async fn recv(&self) -> Option<(Vec<u8>, SocketAddr)>;
    async fn send_to(&self, bytes: &[u8], to: SocketAddr) -> std::io::Result<()>;
}

#[derive(Debug)]
pub struct UdpBeacon {
    socket: UdpSocket,
}

impl UdpBeacon {
    /// Binds the fixed rendezvous port, where probes arrive, with broadcast enabled.
    pub async fn bind(addr: IpAddr) -> std::io::Result<Self> {
        let socket = UdpSocket::bind(SocketAddr::new(addr, RENDEZVOUS_PORT)).await?;
        socket.set_broadcast(true)?;
        Ok(UdpBeacon { socket })
    }

    /// Binds any port: a host both listening and pulling already holds the fixed one, and a seeker
    /// sends first.
    pub async fn bind_ephemeral(addr: IpAddr) -> std::io::Result<Self> {
        let socket = UdpSocket::bind(SocketAddr::new(addr, 0)).await?;
        socket.set_broadcast(true)?;
        Ok(UdpBeacon { socket })
    }

    pub fn deadline(self, d: std::time::Duration) -> TimedBeacon {
        TimedBeacon {
            inner: self,
            until: tokio::time::Instant::now() + d,
        }
    }
}

#[derive(Debug)]
pub struct TimedBeacon {
    inner: UdpBeacon,
    until: tokio::time::Instant,
}

#[async_trait::async_trait]
impl Beacon for TimedBeacon {
    async fn recv(&self) -> Option<(Vec<u8>, SocketAddr)> {
        tokio::time::timeout_at(self.until, self.inner.recv())
            .await
            .ok()
            .flatten()
    }
    async fn send_to(&self, bytes: &[u8], to: SocketAddr) -> std::io::Result<()> {
        self.inner.send_to(bytes, to).await
    }
}

#[async_trait::async_trait]
impl Beacon for UdpBeacon {
    async fn recv(&self) -> Option<(Vec<u8>, SocketAddr)> {
        // A fixed buffer keeps a flood from costing allocation; a probe is a small JSON object.
        let mut buf = vec![0u8; 2048];
        match self.socket.recv_from(&mut buf).await {
            Ok((n, from)) => {
                buf.truncate(n);
                Some((buf, from))
            }
            Err(e) => {
                tracing::debug!(error = %e, "rendezvous socket closed");
                None
            }
        }
    }

    async fn send_to(&self, bytes: &[u8], to: SocketAddr) -> std::io::Result<()> {
        self.socket.send_to(bytes, to).await.map(|_| ())
    }
}

#[derive(Debug)]
struct State {
    mode: ListenerMode,
    tcp_port: Option<u16>,
    pushes: u64,
    last_push_at: Option<i64>,
    /// Cleared when the door shuts: a replayed `open` cannot be followed by foreign bytes (the body
    /// MAC covers its hash), so remembering nonces longer buys nothing.
    seen: HashSet<String>,
}

impl State {
    fn status(&self) -> ListenerStatus {
        ListenerStatus {
            mode: self.mode,
            tcp_port: self.tcp_port,
            pushes: self.pushes,
            last_push_at: self.last_push_at,
        }
    }

    /// Closes a window per session, not per sidecar, and leaves `Always` alone; one session carries
    /// every book.
    fn completed_push(&mut self, now: i64) {
        self.pushes += 1;
        self.last_push_at = Some(now);
        if matches!(self.mode, ListenerMode::Window { .. }) {
            self.mode = ListenerMode::Off;
        }
    }
}

/// `Drop` aborts both tasks, so the UDP responder cannot outlive the TCP listener.
#[derive(Debug)]
struct Running {
    tcp: JoinHandle<()>,
    udp: JoinHandle<()>,
    expiry: Option<JoinHandle<()>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.tcp.abort();
        self.udp.abort();
        if let Some(e) = &self.expiry {
            e.abort();
        }
    }
}

/// Lives in the engine, not the daemon: in-process API hosts have no daemon and must still listen.
/// Nothing binds until asked; every spawned task is owned by a `Running` whose `Drop` aborts it.
#[derive(Debug, Default)]
pub struct Listener {
    state: Mutex<Option<State>>,
    running: Mutex<Option<Running>>,
}

impl Listener {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reports `Off` for an expired window even if its tasks are not yet reaped.
    pub async fn status(&self, now: i64) -> ListenerStatus {
        let guard = self.state.lock().await;
        match guard.as_ref() {
            None => ListenerStatus {
                mode: ListenerMode::Off,
                tcp_port: None,
                pushes: 0,
                last_push_at: None,
            },
            Some(s) if !s.mode.is_open(now) => ListenerStatus {
                mode: ListenerMode::Off,
                tcp_port: None,
                ..s.status()
            },
            Some(s) => s.status(),
        }
    }

    /// Starting an already-running listener replaces it, so pressing *Listen now* twice extends the
    /// window.
    pub async fn start(
        self: &Arc<Self>,
        storage: Storage,
        name: String,
        bind: IpAddr,
        beacon: Arc<dyn Beacon>,
        minutes: Option<u32>,
        now: i64,
    ) -> Result<ListenerStatus> {
        let tcp = TcpListener::bind(SocketAddr::new(bind, 0)).await?;
        let port = tcp.local_addr()?.port();

        let mode = match minutes {
            Some(0) => ListenerMode::Always,
            Some(m) => ListenerMode::Window {
                until: now + i64::from(m) * 60,
            },
            None => ListenerMode::Window {
                until: now + i64::from(DEFAULT_WINDOW_MINUTES) * 60,
            },
        };

        // State is installed before the tasks, so a probe arriving between the two finds an open listener.
        {
            let mut guard = self.state.lock().await;
            let (pushes, last_push_at) = guard
                .as_ref()
                .map(|s| (s.pushes, s.last_push_at))
                .unwrap_or((0, None));
            *guard = Some(State {
                mode,
                tcp_port: Some(port),
                pushes,
                last_push_at,
                seen: HashSet::new(),
            });
        }

        let udp_task = {
            let me = Arc::clone(self);
            let storage = storage.clone();
            let beacon = Arc::clone(&beacon);
            tokio::spawn(async move { me.serve_beacon(storage, name, beacon, port).await })
        };
        let tcp_task = {
            let me = Arc::clone(self);
            let storage = storage.clone();
            tokio::spawn(async move { me.serve_tcp(storage, tcp).await })
        };
        let expiry = match mode {
            ListenerMode::Window { until } => {
                let me = Arc::clone(self);
                let secs = (until - now).max(0) as u64;
                Some(tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
                    me.stop().await;
                }))
            }
            _ => None,
        };

        *self.running.lock().await = Some(Running {
            tcp: tcp_task,
            udp: udp_task,
            expiry,
        });

        Ok(self.status(now).await)
    }

    /// Settles state before dropping the sockets: a push can close its own window from inside the
    /// accept task, which `Running::drop` aborts at the next await.
    pub async fn stop(&self) -> ListenerStatus {
        let status = {
            let mut guard = self.state.lock().await;
            if let Some(s) = guard.as_mut() {
                s.mode = ListenerMode::Off;
                s.tcp_port = None;
                s.seen.clear();
            }
            guard.as_ref().map(State::status).unwrap_or(ListenerStatus {
                mode: ListenerMode::Off,
                tcp_port: None,
                pushes: 0,
                last_push_at: None,
            })
        };
        // Dropped outside the state lock: aborted tasks may be waiting on it, and aborting under it
        // deadlocks.
        let taken = self.running.lock().await.take();
        drop(taken);
        status
    }

    async fn serve_beacon(
        &self,
        storage: Storage,
        name: String,
        beacon: Arc<dyn Beacon>,
        port: u16,
    ) {
        while let Some((bytes, from)) = beacon.recv().await {
            match self.answer_probe(&storage, &name, &bytes, port).await {
                Ok(Some(reply)) => {
                    if let Err(e) = beacon.send_to(&reply, from).await {
                        tracing::debug!(error = %e, "could not answer a rendezvous probe");
                    }
                }
                // Silence, not a refusal, so an unpaired scanner cannot learn anything is here.
                Ok(None) => {}
                Err(e) => tracing::debug!(error = %e, "unreadable rendezvous probe"),
            }
        }
    }

    async fn answer_probe(
        &self,
        storage: &Storage,
        name: &str,
        bytes: &[u8],
        port: u16,
    ) -> Result<Option<Vec<u8>>> {
        if !self.status(now_unix()).await.mode.is_open(now_unix()) {
            return Ok(None);
        }
        let hello: Hello = match serde_json::from_slice(bytes) {
            Ok(h) => h,
            Err(_) => return Ok(None),
        };
        if hello.v != PROTOCOL_VERSION {
            return Ok(None);
        }
        let Some(device) = storage.paired_device(&hello.device_id).await? else {
            return Ok(None);
        };
        let reply = Here {
            v: PROTOCOL_VERSION,
            name: name.to_string(),
            tcp_port: port,
            mac: mac(&device.token, &here_challenge(&hello.nonce, port)),
        };
        Ok(Some(serde_json::to_vec(&reply)?))
    }

    async fn serve_tcp(&self, storage: Storage, tcp: TcpListener) {
        loop {
            let Ok((stream, peer)) = tcp.accept().await else {
                return;
            };
            match self.serve_push(&storage, stream, peer).await {
                Ok(report) => {
                    tracing::info!(
                        device_id = %report.device_id,
                        sidecars = report.pulled.len(),
                        "a paired reader pushed over the network"
                    );
                }
                Err(e) => tracing::debug!(error = %e, "a wireless push did not complete"),
            }
        }
    }

    pub async fn serve_push(
        &self,
        storage: &Storage,
        stream: TcpStream,
        peer: SocketAddr,
    ) -> Result<WirelessReport> {
        let (read, mut write) = stream.into_split();
        let mut reader = BufReader::new(read);

        macro_rules! refuse {
            ($r:expr) => {{
                let r = $r;
                let _ = write
                    .write_all(&to_line(&Ack::refused(&r))?)
                    .await
                    .and(write.flush().await);
                return Err(r.into());
            }};
        }

        let open: Open = match read_json_line(&mut reader).await? {
            Some(o) => o,
            None => refuse!(WirelessRefusal::Malformed("no opening frame".into())),
        };
        if open.v != PROTOCOL_VERSION {
            refuse!(WirelessRefusal::UnsupportedVersion {
                theirs: open.v,
                ours: PROTOCOL_VERSION,
            });
        }

        let device: PairedDevice = match storage.paired_device(&open.device_id).await? {
            Some(d) => d,
            None => refuse!(WirelessRefusal::UnknownDevice),
        };
        if !verify(&device.token, &open_challenge(&open.nonce), &open.mac) {
            refuse!(WirelessRefusal::BadCredential);
        }
        {
            let mut guard = self.state.lock().await;
            let Some(state) = guard.as_mut() else {
                refuse!(WirelessRefusal::NotListening);
            };
            if !state.mode.is_open(now_unix()) {
                refuse!(WirelessRefusal::NotListening);
            }
            if !state.seen.insert(open.nonce.clone()) {
                refuse!(WirelessRefusal::ReplayedNonce);
            }
        }
        write.write_all(&to_line(&Ack::ok())?).await?;
        write.flush().await?;

        let report =
            receive_entries(storage, &device, &open.nonce, &mut reader, &mut write).await?;

        let now = now_unix();
        // `last_wireless_at` means this reader reached us; `last_synced_at` means everything it had is
        // here, which a push of some books is not.
        storage
            .stamp_wireless_contact(&device.device_id, Some(&peer.ip().to_string()))
            .await?;
        {
            let mut guard = self.state.lock().await;
            if let Some(state) = guard.as_mut() {
                state.completed_push(now);
            }
        }

        // The ack is written before the door shuts: closing a window aborts this very task, so a later
        // ack would never reach the reader.
        write.write_all(&to_line(&Ack::ok())?).await?;
        write.flush().await?;

        if !self.status(now).await.mode.is_open(now) {
            self.stop().await;
        }
        Ok(report)
    }
}

/// Shared by push and pull. Check order is load-bearing: hash, then MAC (which covers the
/// hash), then parse, so nothing unverified reaches the Lua sandbox.
async fn receive_entries<R, W>(
    storage: &Storage,
    device: &PairedDevice,
    nonce: &str,
    reader: &mut BufReader<R>,
    write: &mut W,
) -> Result<WirelessReport>
where
    R: tokio::io::AsyncRead + Unpin + Send,
    W: tokio::io::AsyncWrite + Unpin + Send,
{
    let mut report = WirelessReport {
        device_id: device.device_id.clone(),
        ..Default::default()
    };
    loop {
        let frame: PushFrame = match read_json_line(reader).await? {
            Some(f) => f,
            None => {
                return Err(
                    WirelessRefusal::Malformed("the connection ended mid-session".into()).into(),
                );
            }
        };
        let PushFrame::Entry {
            name,
            len,
            sha256,
            mac: entry_mac,
        } = frame
        else {
            return Ok(report);
        };
        if len > MAX_SIDECAR_BYTES {
            let r = WirelessRefusal::TooLarge {
                len,
                max: MAX_SIDECAR_BYTES,
            };
            let _ = write.write_all(&to_line(&Ack::refused(&r))?).await;
            return Err(r.into());
        }
        let mut body = vec![0u8; len as usize];
        reader.read_exact(&mut body).await?;

        if sha256_hex(&body) != sha256 {
            let r = WirelessRefusal::Malformed(format!(
                "the body of {name} does not match its declared hash"
            ));
            let _ = write.write_all(&to_line(&Ack::refused(&r))?).await;
            return Err(r.into());
        }
        if !verify(&device.token, &body_challenge(nonce, &sha256), &entry_mac) {
            let r = WirelessRefusal::BadCredential;
            let _ = write.write_all(&to_line(&Ack::refused(&r))?).await;
            return Err(r.into());
        }
        let Ok(src) = String::from_utf8(body) else {
            let r = WirelessRefusal::Malformed(format!("{name} is not UTF-8"));
            let _ = write.write_all(&to_line(&Ack::refused(&r))?).await;
            return Err(r.into());
        };

        match import_book_from_sidecar_src_for_device(
            storage,
            &src,
            &PathBuf::from(&name),
            Some(&device.device_id),
        )
        .await
        {
            Ok(pull) => {
                report.warnings.extend(pull.warnings.iter().cloned());
                report.pulled.push(pull);
                write.write_all(&to_line(&Ack::ok())?).await?;
            }
            Err(e) => {
                report
                    .warnings
                    .push(Diagnostic::sidecar_unreadable(PathBuf::from(&name), &e));
                write
                    .write_all(&to_line(&Ack {
                        ok: false,
                        error: Some(e.to_string()),
                    })?)
                    .await?;
            }
        }
        write.flush().await?;
    }
}

/// The rendezvous run the other way: same HELLO/HERE/OPEN, entries still flow reader to desktop.
/// No unsolicited beacon: an unchallenged announcement carries no fresh nonce and is replayable.
pub async fn pull_from(
    storage: &Storage,
    device: &PairedDevice,
    beacon: &dyn Beacon,
    targets: &[SocketAddr],
    nonce: &str,
) -> Result<WirelessReport> {
    let hello = Hello {
        v: PROTOCOL_VERSION,
        device_id: device.device_id.clone(),
        nonce: nonce.to_string(),
    };
    // Probe every broadcast address: the limited broadcast leaves by the default route, which under
    // a VPN is a tunnel no reader is on.
    let bytes = serde_json::to_vec(&hello)?;
    let mut sent = false;
    for target in targets {
        match beacon.send_to(&bytes, *target).await {
            Ok(()) => sent = true,
            Err(e) => tracing::debug!(error = %e, to = %target, "could not send a probe"),
        }
    }
    if !sent {
        return Err(WirelessRefusal::ReaderNotFound.into());
    }

    // Read until a reply proves the token rather than trusting the first replier; identity, not
    // address.
    let (addr, here) = loop {
        let Some((bytes, from)) = beacon.recv().await else {
            return Err(WirelessRefusal::ReaderNotFound.into());
        };
        if let Ok(here) = serde_json::from_slice::<Here>(&bytes)
            && here.v == PROTOCOL_VERSION
            && verify(
                &device.token,
                &here_challenge(nonce, here.tcp_port),
                &here.mac,
            )
        {
            break (from, here);
        }
    };

    let target = SocketAddr::new(addr.ip(), here.tcp_port);
    let stream = TcpStream::connect(target).await?;
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);

    write
        .write_all(&to_line(&Open {
            v: PROTOCOL_VERSION,
            device_id: device.device_id.clone(),
            nonce: nonce.to_string(),
            mac: mac(&device.token, &open_challenge(nonce)),
        })?)
        .await?;
    // The blank line terminates the request: the reader's HTTP-shaped server only dispatches on an
    // empty line and otherwise times out and drops the connection.
    write.write_all(b"\n").await?;
    write.flush().await?;

    match read_json_line::<Ack, _>(&mut reader).await? {
        Some(a) if a.ok => {}
        Some(a) => {
            return Err(WirelessRefusal::Malformed(
                a.error.unwrap_or_else(|| "the reader refused".into()),
            )
            .into());
        }
        None => return Err(WirelessRefusal::Malformed("the reader said nothing".into()).into()),
    }

    let report = receive_entries(storage, device, nonce, &mut reader, &mut write).await?;
    storage
        .stamp_wireless_contact(&device.device_id, Some(&addr.ip().to_string()))
        .await?;
    Ok(report)
}

/// Sent until answered, a second apart; the reader repeats its reply to the same nonce, so
/// retries are safe.
/// A reader we already know keeps its row.
pub async fn pair_with(
    storage: &Storage,
    beacon: &dyn Beacon,
    targets: &[SocketAddr],
    code: &str,
    name: &str,
    nonce: &str,
    now: i64,
) -> Result<PairedDevice> {
    let every = std::time::Duration::from_secs(1);
    pair_with_every(storage, beacon, targets, code, name, nonce, now, every).await
}

/// Private so a test can supply the pause between attempts instead of waiting real seconds.
#[allow(clippy::too_many_arguments)]
async fn pair_with_every(
    storage: &Storage,
    beacon: &dyn Beacon,
    targets: &[SocketAddr],
    code: &str,
    name: &str,
    nonce: &str,
    now: i64,
    every: std::time::Duration,
) -> Result<PairedDevice> {
    let code = normalise_code(code).ok_or(WirelessRefusal::NotAPairingCode)?;
    let known: Vec<String> = storage
        .list_paired_devices()
        .await?
        .into_iter()
        .map(|d| d.device_id)
        .take(MAX_KNOWN_IDS)
        .collect();
    let pair = Pair {
        v: PROTOCOL_VERSION,
        kind: "pair".into(),
        nonce: nonce.to_string(),
        at: now,
        name: name.to_string(),
        mac: mac(&code, &pair_challenge(nonce, now, &known, name)),
        known,
    };
    let bytes = serde_json::to_vec(&pair)?;

    let mut answer = None;
    'attempts: for _ in 0..PAIR_ATTEMPTS {
        let mut sent = false;
        for target in targets {
            match beacon.send_to(&bytes, *target).await {
                Ok(()) => sent = true,
                Err(e) => tracing::debug!(error = %e, "could not send a pairing request"),
            }
        }
        if !sent {
            break;
        }
        let until = tokio::time::Instant::now() + every;
        loop {
            match tokio::time::timeout_at(until, beacon.recv()).await {
                Err(_) => break,
                Ok(None) => break 'attempts,
                Ok(Some((reply, from))) => {
                    if let Ok(paired) = serde_json::from_slice::<Paired>(&reply)
                        && paired.v == PROTOCOL_VERSION
                        && paired.kind == "paired"
                        && verify(
                            &code,
                            &paired_challenge(
                                nonce,
                                &paired.nonce,
                                &paired.device_id,
                                paired.plugin,
                                &paired.reader,
                            ),
                            &paired.mac,
                        )
                    {
                        answer = Some((paired, from));
                        break 'attempts;
                    }
                }
            }
        }
    }
    let Some((paired, from)) = answer else {
        return Err(WirelessRefusal::PairingNotAnswered.into());
    };

    // The id becomes a database key and a broadcast string, so it must have the shape we mint (hex,
    // bounded length) even if signed.
    if paired.device_id.len() < 16
        || paired.device_id.len() > 64
        || !paired
            .device_id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(WirelessRefusal::Malformed("the reader named itself strangely".into()).into());
    }
    // An id we already hold is re-keyed only for the reader that proves its stored token; ids are
    // broadcast, so the code alone proves nothing.
    if let Some(existing) = storage.paired_device(&paired.device_id).await? {
        let proven = paired.proof.as_deref().is_some_and(|proof| {
            verify(
                &existing.token,
                &rekey_challenge(nonce, &paired.nonce, &paired.device_id),
                proof,
            )
        });
        if !proven {
            return Err(WirelessRefusal::RekeyNotProven.into());
        }
    }

    let token = mac(
        &code,
        &token_challenge(nonce, &paired.nonce, &paired.device_id),
    );
    let label: String = paired.reader.trim().chars().take(64).collect();
    storage
        .record_wireless_pairing(
            &paired.device_id,
            (!label.is_empty()).then_some(label.as_str()),
            &token,
            paired.plugin,
            now,
        )
        .await?;
    storage
        .stamp_wireless_contact(&paired.device_id, Some(&from.ip().to_string()))
        .await?;
    storage
        .paired_device(&paired.device_id)
        .await?
        .ok_or_else(|| WirelessRefusal::UnknownDevice.into())
}

fn to_line<T: serde::Serialize>(v: &T) -> Result<Vec<u8>> {
    // `to_string` escapes newlines inside strings, so a diagnostic cannot break a frame.
    let mut s = serde_json::to_vec(v)?;
    s.push(b'\n');
    Ok(s)
}

async fn read_json_line<T: serde::de::DeserializeOwned, R>(
    reader: &mut BufReader<R>,
) -> Result<Option<T>>
where
    R: tokio::io::AsyncRead + Unpin + Send,
{
    let mut line = String::new();
    let mut limited = reader.take(MAX_HEADER_BYTES);
    let n = limited.read_line(&mut line).await?;
    if n == 0 {
        return Ok(None);
    }
    Ok(serde_json::from_str(line.trim_end()).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    /// Vector computed by the device's own sha2.lua; the key is the 64-char hex text, not the 32
    /// bytes it encodes.
    #[test]
    fn our_hmac_agrees_with_the_device_and_uses_the_hex_as_the_key() {
        assert_eq!(
            mac(TOKEN, "nonce-0001"),
            "a38b20860c54b13a06e1fac37207b4ca1120db8946e82848818567a07d7a7cae",
            "this is `sha.hmac(sha.sha256, token, nonce)` on the device"
        );
        assert_ne!(
            mac(TOKEN, "nonce-0001"),
            "26ae9d255e7a30de01b32de4544430f32a4bdf4dea6907f07c3c604311a599fe",
            "that is the same function keyed with the decoded bytes"
        );
        assert!(verify(TOKEN, "nonce-0001", &mac(TOKEN, "nonce-0001")));
        assert!(!verify(
            "another-token",
            "nonce-0001",
            &mac(TOKEN, "nonce-0001")
        ));
        for bad in ["", "zz", "a38b2086", "not-hex-at-all"] {
            assert!(!verify(TOKEN, "nonce-0001", bad), "{bad}");
        }
    }

    #[test]
    fn a_discovery_reply_cannot_be_replayed_as_an_authorisation_to_push() {
        let here = mac(TOKEN, &here_challenge("n", 51000));
        let open = mac(TOKEN, &open_challenge("n"));
        let body = mac(TOKEN, &body_challenge("n", "abc"));
        assert_ne!(here, open);
        assert_ne!(open, body);
        assert!(!verify(TOKEN, &open_challenge("n"), &here));
        assert!(!verify(TOKEN, &here_challenge("n", 51001), &here));
    }

    #[test]
    fn a_code_is_read_the_way_a_person_types_it() {
        let code = "0123456789ABCDEF";
        assert_eq!(normalise_code(code).as_deref(), Some(code));
        assert_eq!(
            normalise_code(" 0123-4567 89ab-cdef ").as_deref(),
            Some(code)
        );
        assert_eq!(
            normalise_code("OIL0-0000-0000-000o").as_deref(),
            Some("0110000000000000")
        );
        assert_eq!(
            normalise_code("GHJK-MNPQ-RSTV-WXYZ").as_deref(),
            Some("GHJKMNPQRSTVWXYZ")
        );
        for bad in [
            "",
            "0123-4567-89AB-CDE",
            "0123-4567-89AB-CDEF-0",
            "U123-4567-89AB-CDEF",
            "0123_4567_89AB_CDEF",
        ] {
            assert_eq!(normalise_code(bad), None, "{bad}");
        }
    }

    #[derive(Debug)]
    struct ReaderWithCode {
        code: String,
        holds: Vec<(String, String)>,
        lose: std::sync::atomic::AtomicU32,
        heard: std::sync::atomic::AtomicU32,
        claims: Option<String>,
        addr: SocketAddr,
        tx: tokio::sync::mpsc::UnboundedSender<(Vec<u8>, SocketAddr)>,
        rx: Mutex<tokio::sync::mpsc::UnboundedReceiver<(Vec<u8>, SocketAddr)>>,
    }

    impl ReaderWithCode {
        fn showing(code: &str) -> Self {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            ReaderWithCode {
                code: code.into(),
                holds: Vec::new(),
                lose: 0.into(),
                heard: 0.into(),
                claims: None,
                addr: "192.0.2.42:61862".parse().unwrap(),
                tx,
                rx: Mutex::new(rx),
            }
        }
        fn heard(&self) -> u32 {
            self.heard.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl Beacon for ReaderWithCode {
        async fn recv(&self) -> Option<(Vec<u8>, SocketAddr)> {
            self.rx.lock().await.recv().await
        }
        async fn send_to(&self, bytes: &[u8], _to: SocketAddr) -> std::io::Result<()> {
            use std::sync::atomic::Ordering::SeqCst;
            self.heard.fetch_add(1, SeqCst);
            let Ok(pair) = serde_json::from_slice::<Pair>(bytes) else {
                return Ok(());
            };
            let signed = pair_challenge(&pair.nonce, pair.at, &pair.known, &pair.name);
            if pair.kind != "pair" || !verify(&self.code, &signed, &pair.mac) {
                return Ok(());
            }
            if self
                .lose
                .fetch_update(SeqCst, SeqCst, |n| n.checked_sub(1))
                .is_ok()
            {
                return Ok(());
            }
            let held = pair
                .known
                .iter()
                .find_map(|k| self.holds.iter().find(|(id, _)| id == k));
            let device_id = self.claims.clone().unwrap_or_else(|| match held {
                Some((id, _)) => id.clone(),
                None => "00112233445566778899aabbccddeeff".into(),
            });
            let proof = self
                .holds
                .iter()
                .find(|(id, _)| *id == device_id)
                .map(|(id, token)| mac(token, &rekey_challenge(&pair.nonce, "reader-nonce", id)));
            let reply = Paired {
                v: PROTOCOL_VERSION,
                kind: "paired".into(),
                nonce: "reader-nonce".into(),
                mac: mac(
                    &self.code,
                    &paired_challenge(&pair.nonce, "reader-nonce", &device_id, 3, "Kindle"),
                ),
                device_id,
                reader: "Kindle".into(),
                plugin: 3,
                proof,
            };
            let _ = self
                .tx
                .send((serde_json::to_vec(&reply).unwrap(), self.addr));
            Ok(())
        }
    }

    const CODE: &str = "0123456789ABCDEF";
    const NOW: i64 = 1_700_000_000;

    async fn link(storage: &Storage, reader: &ReaderWithCode, typed: &str) -> Result<PairedDevice> {
        let targets: [SocketAddr; 2] = [
            "255.255.255.255:61862".parse().unwrap(),
            "192.0.2.255:61862".parse().unwrap(),
        ];
        let every = std::time::Duration::from_millis(5);
        pair_with_every(
            storage,
            reader,
            &targets,
            typed,
            "desk",
            "desk-nonce",
            NOW,
            every,
        )
        .await
    }

    fn refusal(e: crate::error::EngineError) -> WirelessRefusal {
        match e {
            crate::error::EngineError::WirelessRefused(r) => r,
            other => panic!("expected a wireless refusal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_reader_showing_the_code_becomes_one_of_ours() {
        let storage = Storage::connect("sqlite::memory:").await.unwrap();
        let reader = ReaderWithCode::showing(CODE);

        let device = link(&storage, &reader, "0123-4567-89ab-cdef")
            .await
            .unwrap();

        assert_eq!(device.device_id, "00112233445566778899aabbccddeeff");
        assert_eq!(device.label.as_deref(), Some("Kindle"));
        assert_eq!(device.plugin_version, 3);
        assert_eq!(device.installed_at, NOW);
        assert_eq!(
            device.token,
            mac(
                CODE,
                &token_challenge("desk-nonce", "reader-nonce", &device.device_id)
            )
        );
        assert_eq!(device.token.len(), 64);
        assert!(device.last_wireless_at.is_some());
        assert_eq!(device.last_lan_addr.as_deref(), Some("192.0.2.42"));
        assert_eq!(device.last_seen_at, None);
        assert_eq!(device.last_mount_path, None);
        assert_eq!(storage.list_paired_devices().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn the_wrong_code_is_met_with_silence_and_nothing_is_recorded() {
        let storage = Storage::connect("sqlite::memory:").await.unwrap();
        let reader = ReaderWithCode::showing(CODE);

        let err = link(&storage, &reader, "FEDC-BA98-7654-3210")
            .await
            .unwrap_err();
        assert_eq!(refusal(err), WirelessRefusal::PairingNotAnswered);
        assert!(storage.list_paired_devices().await.unwrap().is_empty());
        assert_eq!(reader.heard(), PAIR_ATTEMPTS * 2);

        let quiet = ReaderWithCode::showing(CODE);
        let err = link(&storage, &quiet, "not a code").await.unwrap_err();
        assert_eq!(refusal(err), WirelessRefusal::NotAPairingCode);
        assert_eq!(quiet.heard(), 0);
    }

    #[tokio::test]
    async fn an_answer_lost_on_the_air_is_asked_for_again() {
        let storage = Storage::connect("sqlite::memory:").await.unwrap();
        let mut reader = ReaderWithCode::showing(CODE);
        reader.lose = 3.into();

        let device = link(&storage, &reader, CODE).await.unwrap();
        assert_eq!(device.device_id, "00112233445566778899aabbccddeeff");
        assert!(reader.heard() >= 4, "it took a fourth request to be heard");
    }

    #[tokio::test]
    async fn a_reader_we_already_know_keeps_its_row_and_its_name() {
        let storage = Storage::connect("sqlite::memory:").await.unwrap();
        let ours = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        storage
            .record_pairing(
                ours,
                Some("KINDLE"),
                "the-cable-token",
                2,
                Some("/Volumes/Kindle"),
                1_600_000_000,
            )
            .await
            .unwrap();
        storage.set_device_label(ours, "Bedside").await.unwrap();
        let before = storage.paired_device(ours).await.unwrap().unwrap();

        let mut reader = ReaderWithCode::showing(CODE);
        reader.holds = vec![
            ("somebody-elses".into(), "their-token".into()),
            (ours.into(), "the-cable-token".into()),
        ];
        let device = link(&storage, &reader, CODE).await.unwrap();

        assert_eq!(device.device_id, ours);
        assert_eq!(storage.list_paired_devices().await.unwrap().len(), 1);
        assert_eq!(
            device.token,
            mac(CODE, &token_challenge("desk-nonce", "reader-nonce", ours))
        );
        assert_eq!(device.plugin_version, 3);
        assert_eq!(device.label.as_deref(), Some("Bedside"));
        assert_eq!(device.installed_at, 1_600_000_000);
        assert_eq!(device.last_seen_at, before.last_seen_at);
        assert_eq!(device.last_mount_path.as_deref(), Some("/Volumes/Kindle"));
    }

    #[tokio::test]
    async fn an_id_that_is_not_the_readers_to_claim_is_refused() {
        let storage = Storage::connect("sqlite::memory:").await.unwrap();
        let id = |i: usize| format!("{i:032x}");
        for i in 0..=MAX_KNOWN_IDS {
            storage
                .record_pairing(&id(i), None, &format!("token-{i}"), 3, None, NOW)
                .await
                .unwrap();
        }
        let offered: Vec<String> = storage
            .list_paired_devices()
            .await
            .unwrap()
            .into_iter()
            .map(|d| d.device_id)
            .take(MAX_KNOWN_IDS)
            .collect();
        let unoffered = (0..=MAX_KNOWN_IDS)
            .map(id)
            .find(|i| !offered.contains(i))
            .unwrap();

        // A device shown a code could answer as a reader we know (ids are broadcast); without a proof of
        // that reader's token the row must not be re-keyed.
        for (claimed, held_token) in [
            (offered[0].clone(), None),
            (offered[0].clone(), Some("a-guess")),
            (unoffered.clone(), None),
        ] {
            let mut reader = ReaderWithCode::showing(CODE);
            reader.claims = Some(claimed.clone());
            if let Some(token) = held_token {
                reader.holds = vec![(claimed.clone(), token.into())];
            }
            let err = link(&storage, &reader, CODE).await.unwrap_err();
            assert_eq!(refusal(err), WirelessRefusal::RekeyNotProven, "{claimed}");
            let untouched = storage.paired_device(&claimed).await.unwrap().unwrap();
            assert!(
                untouched.token.starts_with("token-"),
                "the other reader's secret stands"
            );
            assert_eq!(untouched.last_wireless_at, None, "and nothing was stamped");
        }

        for strange in [
            "",
            "short",
            "NOT-HEX-NOT-HEX-NOT-HEX-NOT-HEX!",
            &"a".repeat(65),
        ] {
            let mut reader = ReaderWithCode::showing(CODE);
            reader.claims = Some(strange.to_string());
            let err = link(&storage, &reader, CODE).await.unwrap_err();
            assert!(
                matches!(refusal(err), WirelessRefusal::Malformed(_)),
                "{strange}"
            );
        }
        assert_eq!(
            storage.list_paired_devices().await.unwrap().len(),
            MAX_KNOWN_IDS + 1
        );
    }

    #[test]
    fn a_pairing_message_cannot_be_replayed_as_another() {
        let known = vec!["aa".to_string()];
        let all = [
            mac(CODE, &pair_challenge("n", 1, &known, "desk")),
            mac(CODE, &paired_challenge("n", "m", "dev", 3, "Kindle")),
            mac(CODE, &token_challenge("n", "m", "dev")),
            mac(CODE, &rekey_challenge("n", "m", "dev")),
            mac(CODE, &here_challenge("n", 1)),
            mac(CODE, &open_challenge("n")),
        ];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b);
            }
        }
        let earlier = mac(CODE, &paired_challenge("earlier", "m", "dev", 3, "Kindle"));
        assert!(!verify(
            CODE,
            &paired_challenge("n", "m", "dev", 3, "Kindle"),
            &earlier
        ));
    }

    #[test]
    fn a_window_closes_on_time_and_always_does_not() {
        let now = 1_700_000_000;
        assert!(!ListenerMode::Off.is_open(now));
        assert!(ListenerMode::Always.is_open(now + 999_999));
        let w = ListenerMode::Window { until: now + 60 };
        assert!(w.is_open(now));
        assert!(w.is_open(now + 59));
        assert!(!w.is_open(now + 60), "the boundary is closed, not open");
        assert!(!w.is_open(now + 61));
    }

    #[test]
    fn a_completed_push_closes_a_window_and_leaves_always_alone() {
        let mut s = State {
            mode: ListenerMode::Window {
                until: 1_700_000_060,
            },
            tcp_port: Some(1),
            pushes: 0,
            last_push_at: None,
            seen: HashSet::new(),
        };
        s.completed_push(1_700_000_001);
        assert_eq!(s.mode, ListenerMode::Off);
        assert_eq!(s.pushes, 1);
        assert_eq!(s.last_push_at, Some(1_700_000_001));

        s.mode = ListenerMode::Always;
        s.completed_push(1_700_000_002);
        assert_eq!(
            s.mode,
            ListenerMode::Always,
            "a desktop that lives on the LAN does not close after one reader"
        );
        assert_eq!(s.pushes, 2);
    }
}
