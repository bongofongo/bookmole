//! A sync server in memory, enough of the protocol to carry real engines' syncs: capabilities,
//! creating a library, adding a device, appending an envelope and reading pages of the log.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

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
    in_flight: usize,
    most_in_flight: usize,
}

/// Cheap to clone; every clone is the same server.
#[derive(Clone, Default)]
pub(crate) struct TestServer(Arc<Mutex<State>>);

const PAGE_BATCHES: u32 = 2;
const SKEW: u64 = 300;

impl TestServer {
    fn with<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        f(&mut self.0.lock().unwrap())
    }

    /// Requests the server has answered.
    pub(crate) fn requests(&self) -> usize {
        self.with(|s| s.requests)
    }

    /// Envelopes in every library's log.
    pub(crate) fn log_len(&self) -> usize {
        self.with(|s| s.libraries.values().map(|l| l.log.len()).sum())
    }

    /// Reads see only the first `horizon` envelopes of a log, or all of it.
    pub(crate) fn lag_reads(&self, horizon: Option<usize>) {
        self.with(|s| s.horizon = horizon);
    }

    /// The most requests that were ever in flight at once.
    pub(crate) fn most_in_flight(&self) -> usize {
        self.with(|s| s.most_in_flight)
    }
}

impl Transport for TestServer {
    async fn send(&self, request: Request) -> Result<Response, TransportError> {
        self.with(|s| {
            s.in_flight += 1;
            s.most_in_flight = s.most_in_flight.max(s.in_flight);
        });
        tokio::task::yield_now().await;
        let response = self.with(|s| {
            s.in_flight -= 1;
            s.requests += 1;
            s.handle(&request).unwrap_or_else(refusal)
        });
        Ok(response)
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
