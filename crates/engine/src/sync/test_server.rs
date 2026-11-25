//! A sync server in memory, enough of the protocol to carry real engines' syncs: capabilities,
//! creating a library, adding a device, appending an envelope and reading pages of the log.
//! [`TestServer::serve`] puts it on a loopback port, for tests that reach it over HTTP as a
//! shipped engine does.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fongo_sync_client::protocol::api::{
    AddDevice, Appended, BatchPage, Capabilities, CreateLibrary, DeviceRecord, Devices, ErrorBody,
    ErrorCode, ReadBatches, Route, StoredBatch,
};
use fongo_sync_client::protocol::{
    DeviceId, HEADER_DEVICE, HEADER_NONCE, HEADER_SIGNATURE, HEADER_TIMESTAMP, LibraryId,
    PROTOCOL_VERSION, RequestAuth, VerifyingKey, verify_request,
};
use fongo_sync_client::{Method, Request, Response, Transport, TransportError};

#[derive(Default)]
struct Library {
    devices: Vec<DeviceRecord>,
    log: Vec<Vec<u8>>,
}

#[derive(Default)]
struct State {
    libraries: HashMap<LibraryId, Library>,
    requests: usize,
    /// Reads see the log only this far, as from a replica of the server that lags.
    horizon: Option<usize>,
    /// The request with this number, counted from 1, never gets an answer.
    hang_at: Option<usize>,
    /// The request with this number to arrive waits for a release before it is answered.
    hold_at: Option<usize>,
    /// Requests that have arrived, answered or not.
    arrived: usize,
    /// Every request's endpoint, in the order they arrived.
    endpoints: Vec<&'static str>,
}

/// Cheap to clone; every clone is the same server.
#[derive(Clone, Default)]
pub struct TestServer(Arc<Mutex<State>>, Arc<Hold>);

/// A request held before it is answered, until [`TestServer::release`].
#[derive(Default)]
struct Hold {
    reached: tokio::sync::Notify,
    released: tokio::sync::Notify,
}

const PAGE_BATCHES: u32 = 2;
const SKEW: u64 = 300;

impl TestServer {
    fn with<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        f(&mut self.0.lock().unwrap())
    }

    /// Requests the server has answered.
    pub fn requests(&self) -> usize {
        self.with(|s| s.requests)
    }

    /// Envelopes in every library's log.
    pub fn log_len(&self) -> usize {
        self.with(|s| s.libraries.values().map(|l| l.log.len()).sum())
    }

    /// Every envelope appended, in the order the log holds them.
    pub fn envelopes(&self) -> Vec<Vec<u8>> {
        self.with(|s| s.libraries.values().flat_map(|l| l.log.clone()).collect())
    }

    /// The `n`-th request from now on, counted from 1, is never answered.
    pub fn hang_at(&self, n: Option<usize>) {
        self.with(|s| s.hang_at = n.map(|n| s.requests + n));
    }

    /// The `n`-th request to arrive from now on waits until [`TestServer::release`].
    pub fn hold_at(&self, n: usize) {
        self.with(|s| s.hold_at = Some(s.arrived + n));
    }

    /// Returns once the held request has arrived.
    pub async fn held(&self) {
        let reached = self.1.reached.notified();
        if self.with(|s| s.hold_at.is_some_and(|at| s.arrived >= at)) {
            return;
        }
        reached.await;
    }

    pub fn release(&self) {
        self.with(|s| s.hold_at = None);
        self.1.released.notify_waiters();
    }

    /// Requests that have arrived, answered or not.
    pub fn arrived(&self) -> usize {
        self.with(|s| s.arrived)
    }

    /// The endpoint of every request so far, in order.
    pub fn endpoints(&self) -> Vec<&'static str> {
        self.with(|s| s.endpoints.clone())
    }

    /// Appends `n` copies of the last envelope of every library's log, as if its device had sent
    /// the same changes again and again.
    pub fn repeat_last(&self, n: usize) {
        self.with(|s| {
            for library in s.libraries.values_mut() {
                if let Some(last) = library.log.last().cloned() {
                    library.log.extend(std::iter::repeat_n(last, n));
                }
            }
        });
    }

    /// Reads see only the first `horizon` envelopes of a log, or all of it.
    pub fn lag_reads(&self, horizon: Option<usize>) {
        self.with(|s| s.horizon = horizon);
    }

    /// Devices every library lists, recovery keys left out.
    pub fn devices(&self) -> usize {
        self.with(|s| {
            s.libraries
                .values()
                .flat_map(|l| &l.devices)
                .filter(|d| !d.recovery)
                .count()
        })
    }

    /// This server on a loopback port, over HTTP/1.1, until the returned handle is dropped.
    pub fn serve(&self) -> std::io::Result<HttpServer> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let server = self.clone();
        let accepting = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stopping.load(Ordering::SeqCst) {
                    break;
                }
                if let Ok(stream) = stream {
                    let server = server.clone();
                    std::thread::spawn(move || answer(&server, stream));
                }
            }
        });
        Ok(HttpServer {
            addr,
            stop,
            accepting: Some(accepting),
        })
    }
}

/// A [`TestServer`] served on a loopback port. Each connection carries one request and is closed
/// after its answer. Dropping it stops the server.
pub struct HttpServer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    accepting: Option<std::thread::JoinHandle<()>>,
}

impl HttpServer {
    /// The server's origin, `http://127.0.0.1:<port>`.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wakes the accept loop so it sees the stop.
        let _ = TcpStream::connect(self.addr);
        if let Some(accepting) = self.accepting.take() {
            let _ = accepting.join();
        }
    }
}

const HEAD_LIMIT: usize = 64 << 10;

fn answer(server: &TestServer, mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let response = match read_request(&mut stream) {
        Some(request) => futures::executor::block_on(server.send(request))
            .unwrap_or_else(|_| refusal(ErrorCode::Internal)),
        None => refusal(ErrorCode::MalformedRequest),
    };
    let mut head = format!(
        "HTTP/1.1 {} -\r\ncontent-length: {}\r\nconnection: close\r\n",
        response.status,
        response.body.len()
    );
    for (name, value) in &response.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&response.body);
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut seen = Vec::new();
    let mut buf = [0u8; 8192];
    let head_end = loop {
        if let Some(at) = seen.windows(4).position(|w| w == b"\r\n\r\n") {
            break at;
        }
        if seen.len() > HEAD_LIMIT {
            return None;
        }
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => return None,
            Ok(n) => seen.extend_from_slice(&buf[..n]),
        }
    };
    let head = std::str::from_utf8(&seen[..head_end]).ok()?;
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = match first.next()? {
        "GET" => Method::Get,
        "POST" => Method::Post,
        _ => return None,
    };
    let target = first.next()?.to_owned();
    let mut headers = Vec::new();
    let mut length = 0usize;
    for line in lines {
        let (name, value) = line.split_once(':')?;
        let (name, value) = (name.trim(), value.trim());
        if name.eq_ignore_ascii_case("content-length") {
            length = value.parse().ok()?;
        }
        let known = [
            HEADER_DEVICE,
            HEADER_TIMESTAMP,
            HEADER_NONCE,
            HEADER_SIGNATURE,
            "content-type",
        ];
        if let Some(name) = known.into_iter().find(|k| name.eq_ignore_ascii_case(k)) {
            headers.push((name, value.to_owned()));
        }
    }
    let mut body = seen[head_end + 4..].to_vec();
    while body.len() < length {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => return None,
            Ok(n) => body.extend_from_slice(&buf[..n]),
        }
    }
    body.truncate(length);
    Some(Request {
        method,
        target,
        headers,
        body,
    })
}

impl Transport for TestServer {
    async fn send(&self, request: Request) -> Result<Response, TransportError> {
        let released = self.1.released.notified();
        let hold = self.with(|s| {
            s.arrived += 1;
            s.hold_at == Some(s.arrived)
        });
        if hold {
            self.1.reached.notify_waiters();
            released.await;
        }
        tokio::task::yield_now().await;
        let response = self.with(|s| {
            s.requests += 1;
            s.endpoints.push(endpoint(&request));
            (s.hang_at != Some(s.requests)).then(|| s.handle(&request).unwrap_or_else(refusal))
        });
        match response {
            Some(response) => Ok(response),
            None => std::future::pending().await,
        }
    }
}

fn endpoint(request: &Request) -> &'static str {
    match (
        Route::parse(&request.target).map(|(r, _)| r),
        request.method,
    ) {
        (Some(Route::Capabilities), _) => "capabilities",
        (Some(Route::Libraries), _) => "create",
        (Some(Route::Devices(_)), Method::Get) => "devices",
        (Some(Route::Devices(_)), Method::Post) => "add device",
        (Some(Route::Batches(_)), Method::Get) => "read",
        (Some(Route::Batches(_)), Method::Post) => "append",
        _ => "other",
    }
}

fn refusal(code: ErrorCode) -> Response {
    json(code.status(), &ErrorBody::new(code))
}

fn json<T: serde::Serialize>(status: u16, value: &T) -> Response {
    Response {
        status,
        headers: vec![("content-type".into(), "application/json".into())],
        body: serde_json::to_vec(value).unwrap(),
    }
}

fn header<'a>(request: &'a Request, name: &str) -> Result<&'a str, ErrorCode> {
    request
        .headers
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, v)| v.as_str())
        .ok_or(ErrorCode::Unauthenticated)
}

fn authenticate(request: &Request, key: &VerifyingKey) -> Result<(), ErrorCode> {
    let auth = RequestAuth::from_headers(
        header(request, HEADER_DEVICE)?,
        header(request, HEADER_TIMESTAMP)?,
        header(request, HEADER_NONCE)?,
        header(request, HEADER_SIGNATURE)?,
    )?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    verify_request(
        &auth,
        key,
        request.method.as_str(),
        &request.target,
        &request.body,
        now,
        SKEW,
    )?;
    Ok(())
}

impl State {
    /// The record of the library's key that signed `request`.
    fn member(&self, request: &Request, id: LibraryId) -> Result<DeviceRecord, ErrorCode> {
        let device = DeviceId::from_hex(header(request, HEADER_DEVICE)?)
            .ok_or(ErrorCode::Unauthenticated)?;
        let record = self
            .libraries
            .get(&id)
            .ok_or(ErrorCode::LibraryNotFound)?
            .devices
            .iter()
            .find(|d| DeviceId::of(&d.public_key) == device)
            .cloned()
            .ok_or(ErrorCode::UnknownDevice)?;
        authenticate(request, &record.public_key)?;
        Ok(record)
    }

    fn handle(&mut self, request: &Request) -> Result<Response, ErrorCode> {
        let (route, query) = Route::parse(&request.target).ok_or(ErrorCode::NotFound)?;
        match (route, request.method) {
            (Route::Capabilities, Method::Get) => Ok(json(
                200,
                &Capabilities {
                    protocol_version: PROTOCOL_VERSION,
                    blobs: false,
                    notifications: false,
                    skew_secs: SKEW,
                    max_batch_bytes: Some(1 << 20),
                    max_library_bytes: None,
                    max_devices: Some(8),
                    max_page_batches: Some(PAGE_BATCHES),
                    max_page_bytes: None,
                },
            )),
            (Route::Libraries, Method::Post) => {
                let create: CreateLibrary = serde_json::from_slice(&request.body)
                    .map_err(|_| ErrorCode::MalformedRequest)?;
                authenticate(request, &create.device_key)?;
                if !create.verify() {
                    return Err(ErrorCode::InvalidProof);
                }
                if self.libraries.contains_key(&create.library) {
                    return Err(ErrorCode::LibraryExists);
                }
                let devices = vec![
                    DeviceRecord {
                        public_key: create.device_key,
                        recovery: false,
                    },
                    DeviceRecord {
                        public_key: create.recovery_key,
                        recovery: true,
                    },
                ];
                self.libraries.insert(
                    create.library,
                    Library {
                        devices: devices.clone(),
                        log: Vec::new(),
                    },
                );
                Ok(json(201, &Devices { devices }))
            }
            (Route::Devices(id), Method::Get) => {
                self.member(request, id)?;
                let devices = self.libraries[&id].devices.clone();
                Ok(json(200, &Devices { devices }))
            }
            (Route::Devices(id), Method::Post) => {
                self.member(request, id)?;
                let add: AddDevice = serde_json::from_slice(&request.body)
                    .map_err(|_| ErrorCode::MalformedRequest)?;
                let library = self.libraries.get_mut(&id).unwrap();
                let record = DeviceRecord {
                    public_key: add.device_key,
                    recovery: false,
                };
                if !library.devices.contains(&record) {
                    library.devices.push(record);
                }
                let devices = library.devices.clone();
                Ok(json(200, &Devices { devices }))
            }
            (Route::Batches(id), Method::Post) => {
                if self.member(request, id)?.recovery {
                    return Err(ErrorCode::Forbidden);
                }
                let log = &mut self.libraries.get_mut(&id).unwrap().log;
                log.push(request.body.clone());
                Ok(json(
                    200,
                    &Appended {
                        seq: log.len() as u64,
                    },
                ))
            }
            (Route::Batches(id), Method::Get) => {
                if self.member(request, id)?.recovery {
                    return Err(ErrorCode::Forbidden);
                }
                let read = ReadBatches::parse_query(query.unwrap_or(""))
                    .map_err(|_| ErrorCode::MalformedRequest)?;
                let limit = read.limit.unwrap_or(PAGE_BATCHES).min(PAGE_BATCHES) as usize;
                let log = &self.libraries[&id].log;
                let log = &log[..self.horizon.unwrap_or(log.len()).min(log.len())];
                let after = usize::try_from(read.after).unwrap().min(log.len());
                let batches: Vec<StoredBatch> = log[after..]
                    .iter()
                    .take(limit)
                    .enumerate()
                    .map(|(i, envelope)| StoredBatch {
                        seq: (after + i + 1) as u64,
                        envelope: envelope.clone(),
                    })
                    .collect();
                let page = BatchPage {
                    more: after + batches.len() < log.len(),
                    batches,
                };
                Ok(Response {
                    status: 200,
                    headers: vec![("content-type".into(), "application/octet-stream".into())],
                    body: page.encode(),
                })
            }
            _ => Err(ErrorCode::MethodNotAllowed),
        }
    }
}
