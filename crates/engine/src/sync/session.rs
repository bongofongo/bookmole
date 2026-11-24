//! Turning sync on for a library, and syncing it: [`Engine`]'s side of the kernel client.

use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fongo_sync_client::{Error as ClientError, SyncReport, Transport};
use fongo_sync_sqlite::{Replica, Resume};
use sqlx::sqlite::SqliteConnection;
use sqlx::{ConnectOptions, Connection};
use zeroize::Zeroizing;

use super::http::{HttpTransport, Origin};
use super::identity::{self, Identity, Stored};
use super::{Receiver, kernel, replica, take_after, unwrap_application};
use crate::Engine;
use crate::error::{EngineError, Result};

/// Why sync could not do what was asked. None carries a secret or an address with a query.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SyncError {
    #[error("sync is off for this library")]
    Off,
    #[error("this library already syncs")]
    AlreadyOn,
    /// The database says it syncs but the data directory lacks this device's identity, as when
    /// a database is copied without it. Nothing syncs, and nothing is removed.
    #[error(
        "this library syncs, but this device's sync identity is missing from the data directory"
    )]
    NeedsCredentials,
    #[error("that is not a library's recovery code")]
    CodeInvalid,
    #[error("the sync server could not be reached")]
    Unreachable,
    /// `code` is the protocol's name for the refusal, such as `device_limit`.
    #[error("the sync server refused the request ({code})")]
    Refused { status: u16, code: String },
    /// The server answered with something the protocol does not allow, or its limits leave no
    /// room for a change this device has to send.
    #[error("the sync server cannot be used as it stands: {0}")]
    ServerFault(String),
    #[error("this device's clock is too far from the sync server's")]
    ClockSkew,
    /// The server speaks a newer protocol, or the database's sync tables are of a layout this
    /// version does not know.
    #[error("this version of the app cannot sync with this server or library; it needs an update")]
    SchemaMismatch,
    /// Stopped part-way, for instance by another writer holding the database. Whatever was
    /// committed stays; trying again carries on.
    #[error("sync stopped before it finished; try again")]
    Interrupted,
}

impl SyncError {
    /// Whether the error comes from reaching or using the server, which [`SyncStatus`] reports
    /// until a sync succeeds.
    fn is_a_problem_to_report(&self) -> bool {
        matches!(
            self,
            SyncError::Unreachable
                | SyncError::Refused { .. }
                | SyncError::ServerFault(_)
                | SyncError::ClockSkew
                | SyncError::SchemaMismatch
                | SyncError::Interrupted
        )
    }
}

/// Whether this library syncs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncStatus {
    Off,
    /// See [`SyncError::NeedsCredentials`].
    NeedsCredentials,
    On(SyncState),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncState {
    /// The library's id, in hex. Not a secret: the server knows it.
    pub library_id: String,
    /// The server's origin, `https://host[:port]`.
    pub server: String,
    pub device_label: String,
    /// How far into the server's log this library has read; 0 before the first sync.
    pub log_position: u64,
    /// What stopped the last sync in this process, until one succeeds.
    pub last_problem: Option<SyncError>,
}

/// The code that joins another device to a library. Whoever holds it holds the library: show it
/// to the reader, never log it. `Debug` hides it.
pub struct RecoveryCode(Zeroizing<String>);

impl RecoveryCode {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for RecoveryCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RecoveryCode(..)")
    }
}

/// What one [`Engine::sync_now`] did. It says whether things happened, never how many.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncOutcome {
    /// Something another device wrote arrived and changed this library.
    pub received: bool,
    /// Something written here was sent.
    pub sent: bool,
    /// Rows from other devices that could not be written yet, tried again by every sync.
    pub held: Vec<SyncRow>,
    /// Fields edited here and elsewhere at once; the newer edit was kept.
    pub conflicts: Vec<SyncConflict>,
    /// The pull stopped at a change from a device the server does not yet list; later syncs
    /// try it again.
    pub blocked: bool,
    /// Books whose cover changed on another device. A sync downloads nothing; the caller fetches
    /// these as it fetches any cover.
    pub covers_to_fetch: Vec<i64>,
}

/// A row named by its table and its key as the JSON array of its key columns (`[7]` for a row
/// with an integer id), or no key when it has no row here yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncRow {
    pub collection: String,
    pub key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncConflict {
    pub row: SyncRow,
    pub field: String,
    /// Whether the field now holds the other device's edit rather than this one's.
    pub remote_won: bool,
}

/// One sync at a time per engine, on a connection of its own.
#[derive(Default)]
pub(crate) struct Slot {
    pub(crate) held: tokio::sync::Mutex<Held>,
    problem: std::sync::Mutex<Option<SyncError>>,
}

/// The connection a sync runs on, kept between syncs: what committed applies left to do after
/// a sync that failed later waits in it for the next one that succeeds.
#[derive(Default)]
pub(crate) struct Held {
    pub(crate) conn: Option<SqliteConnection>,
    #[cfg(test)]
    pub(crate) opened: usize,
    #[cfg(test)]
    pub(crate) busy_retries: usize,
}

/// Attempts after the first when another writer holds the database, and the first wait.
const BUSY_RETRIES: u32 = 4;
const BUSY_BACKOFF: Duration = Duration::from_millis(100);
/// Shorter than the pool's: a sync that finds the database locked gives way and tries again
/// rather than wait, since in rollback-journal mode two waiting writers can only fail anyway.
const BUSY_TIMEOUT: Duration = Duration::from_secs(1);

const LABEL_MAX: usize = 64;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn label(device_label: &str) -> Result<String> {
    let label = device_label.trim();
    if label.is_empty() || label.chars().count() > LABEL_MAX || label.chars().any(char::is_control)
    {
        return Err(EngineError::InvalidInput(format!(
            "a device label is 1 to {LABEL_MAX} characters, with no line breaks"
        )));
    }
    Ok(label.to_owned())
}

impl Engine {
    fn in_memory(&self) -> bool {
        self.config.db_url.contains(":memory:")
    }

    async fn sync_installed(&self) -> Result<bool> {
        let mut conn = self.storage.pool().acquire().await?;
        super::installed(&mut conn).await
    }

    fn refuse_in_memory(&self) -> Result<()> {
        if self.in_memory() {
            return Err(EngineError::InvalidInput(
                "sync needs a library stored on disk".into(),
            ));
        }
        Ok(())
    }

    /// Whether this library syncs, and with what. Never waits for a sync in progress.
    pub async fn sync_status(&self) -> Result<SyncStatus> {
        if !self.sync_installed().await? {
            return Ok(SyncStatus::Off);
        }
        let Stored::Found(identity) = identity::read(&self.config.sync_identity)? else {
            return Ok(SyncStatus::NeedsCredentials);
        };
        let library = identity.credentials.library_id();
        let mut conn = self.storage.pool().acquire().await?;
        let log_position = fongo_sync_client::cursor(&mut conn, library)
            .await
            .map_err(engine_error)?;
        let last_problem = self
            .sync
            .problem
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        Ok(SyncStatus::On(SyncState {
            library_id: library.to_hex(),
            server: identity.origin.as_str().to_owned(),
            device_label: identity.device_label.clone(),
            log_position,
            last_problem,
        }))
    }

    /// Makes a new synced library on the server at `server_url` from this one, with this device
    /// as its first member, and returns its recovery code. Show the code to the reader: it is
    /// the only way to join another device, and to get back in if every device is lost.
    pub async fn create_sync_library(
        &self,
        server_url: &str,
        device_label: &str,
    ) -> Result<RecoveryCode> {
        let origin = Origin::parse(server_url)?;
        let transport = HttpTransport::new(origin.clone())?;
        self.create_sync_library_via(origin, device_label, &transport)
            .await
    }

    pub(crate) async fn create_sync_library_via<T: Transport>(
        &self,
        origin: Origin,
        device_label: &str,
        transport: &T,
    ) -> Result<RecoveryCode> {
        self.refuse_in_memory()?;
        let device_label = label(device_label)?;
        let mut held = self.sync.held.lock().await;
        if self.sync_installed().await? {
            return Err(SyncError::AlreadyOn.into());
        }
        let credentials = fongo_sync_client::create_library(transport, now)
            .await
            .map_err(engine_error)?;
        let code = RecoveryCode(Zeroizing::new(credentials.recovery_code()));
        self.install_identity(
            &mut held,
            &Identity {
                origin,
                device_label,
                credentials,
            },
        )
        .await?;
        Ok(code)
    }

    /// Joins this library to the synced library `recovery_code` names, on the server at
    /// `server_url`. Rows here are kept and sent by the first [`Engine::sync_now`], which also
    /// brings in the library's; rows both hold as the same book or highlight become one.
    pub async fn join_sync_library(
        &self,
        server_url: &str,
        recovery_code: &str,
        device_label: &str,
    ) -> Result<()> {
        let origin = Origin::parse(server_url)?;
        let transport = HttpTransport::new(origin.clone())?;
        self.join_sync_library_via(origin, recovery_code, device_label, &transport)
            .await
    }

    pub(crate) async fn join_sync_library_via<T: Transport>(
        &self,
        origin: Origin,
        recovery_code: &str,
        device_label: &str,
        transport: &T,
    ) -> Result<()> {
        self.refuse_in_memory()?;
        let device_label = label(device_label)?;
        let mut held = self.sync.held.lock().await;
        if self.sync_installed().await? {
            return Err(SyncError::AlreadyOn.into());
        }
        let credentials = fongo_sync_client::join_library(transport, recovery_code, now)
            .await
            .map_err(engine_error)?;
        self.install_identity(
            &mut held,
            &Identity {
                origin,
                device_label,
                credentials,
            },
        )
        .await
    }

    /// Stores the identity, then installs change capture; an install that fails takes the
    /// identity back out, so the library is left as it was.
    async fn install_identity(&self, held: &mut Held, identity: &Identity) -> Result<()> {
        identity::write(&self.config.sync_identity, identity)?;
        let installed = async {
            let conn = self.sync_connection(held).await?;
            replica()?.install(conn).await.map_err(kernel_error)
        }
        .await;
        if installed.is_err() {
            identity::remove(&self.config.sync_identity).ok();
        }
        installed
    }

    /// The recovery code of the library this one syncs with.
    pub async fn sync_recovery_code(&self) -> Result<RecoveryCode> {
        if !self.sync_installed().await? {
            return Err(SyncError::Off.into());
        }
        match identity::read(&self.config.sync_identity)? {
            Stored::Found(identity) => Ok(RecoveryCode(Zeroizing::new(
                identity.credentials.recovery_code(),
            ))),
            Stored::Missing | Stored::Unreadable => Err(SyncError::NeedsCredentials.into()),
        }
    }

    /// Sends what was written here since the last sync and applies what other devices sent,
    /// until the server's log is read to its end. One runs at a time; a second waits for the
    /// first. A failure leaves whatever was committed, and the next sync carries on from it.
    pub async fn sync_now(&self) -> Result<SyncOutcome> {
        self.sync_now_with(HttpTransport::new, Receiver::default())
            .await
    }

    pub(crate) async fn sync_now_with<T: Transport>(
        &self,
        transport: impl FnOnce(Origin) -> Result<T>,
        mut receiver: Receiver,
    ) -> Result<SyncOutcome> {
        let mut held = self.sync.held.lock().await;
        if !self.sync_installed().await? {
            return Err(SyncError::Off.into());
        }
        let Stored::Found(identity) = identity::read(&self.config.sync_identity)? else {
            return Err(SyncError::NeedsCredentials.into());
        };
        let transport = transport(identity.origin.clone())?;
        let result = self
            .sync_rounds(&mut held, &identity, &transport, &mut receiver)
            .await;
        let mut problem = self
            .sync
            .problem
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &result {
            Ok(_) => *problem = None,
            Err(EngineError::Sync(e)) if e.is_a_problem_to_report() => *problem = Some(e.clone()),
            Err(_) => {}
        }
        result
    }

    async fn sync_rounds<T: Transport>(
        &self,
        held: &mut Held,
        identity: &Identity,
        transport: &T,
        receiver: &mut Receiver,
    ) -> Result<SyncOutcome> {
        let replica = replica()?;
        let mut outcome = SyncOutcome::default();
        let mut busy = 0;
        loop {
            let conn = self.sync_connection(held).await?;
            let round = async {
                prepare(conn, &replica).await?;
                fongo_sync_client::sync_with(
                    conn,
                    &replica,
                    transport,
                    &identity.credentials,
                    now,
                    receiver,
                )
                .await
            }
            .await;
            match round {
                Ok(report) => {
                    let more = report.more;
                    add(&mut outcome, report);
                    busy = 0;
                    if !more {
                        break;
                    }
                }
                Err(ClientError::ConnectionUnsound { .. }) => {
                    if let Some(conn) = held.conn.take() {
                        conn.close().await.ok();
                    }
                    return Err(SyncError::Interrupted.into());
                }
                Err(e) if busy < BUSY_RETRIES && is_busy(&e) => {
                    tokio::time::sleep(BUSY_BACKOFF * 2u32.pow(busy)).await;
                    busy += 1;
                    #[cfg(test)]
                    {
                        held.busy_retries += 1;
                    }
                }
                Err(e) => return Err(engine_error(e)),
            }
        }
        let conn = self.sync_connection(held).await?;
        outcome.covers_to_fetch = take_after(conn).await?;
        Ok(outcome)
    }

    /// The connection syncs run on: the library's file opened again, never one of the pool's, so
    /// one that a sync leaves unsound is closed without touching what the rest of the engine
    /// uses.
    async fn sync_connection<'h>(&self, held: &'h mut Held) -> Result<&'h mut SqliteConnection> {
        if held.conn.is_none() {
            let options = (*self.storage.pool().connect_options())
                .clone()
                .busy_timeout(BUSY_TIMEOUT);
            held.conn = Some(options.connect().await?);
            #[cfg(test)]
            {
                held.opened += 1;
            }
        }
        Ok(held.conn.as_mut().expect("just opened"))
    }
}

/// Adopts registry changes, and puts change capture back if an open could not: a sync with
/// capture off would miss what was written meanwhile.
async fn prepare(
    conn: &mut SqliteConnection,
    replica: &Replica,
) -> std::result::Result<(), ClientError> {
    let capture_on = crate::storage::capture_installed(conn)
        .await
        .map_err(|e| ClientError::Application(Box::new(e)))?;
    if capture_on {
        replica.install(conn).await?;
    } else {
        replica.resume(conn, Resume::Send).await?;
    }
    Ok(())
}

fn add(outcome: &mut SyncOutcome, report: SyncReport) {
    let row = |collection: String, key: Option<String>| SyncRow { collection, key };
    outcome.received |= report.applied.accepted > 0;
    outcome.sent |= report.sent > 0;
    outcome.blocked = report.blocked.is_some();
    outcome.held = report
        .applied
        .held
        .into_iter()
        .map(|h| row(h.collection, h.key))
        .collect();
    outcome
        .conflicts
        .extend(report.applied.conflicts.into_iter().map(|c| SyncConflict {
            row: row(c.collection, c.key),
            field: c.field,
            remote_won: c.remote_won,
        }));
    if report.refused_total > 0 {
        tracing::warn!(
            refused = report.refused_total,
            "sync skipped envelopes it could not open"
        );
    }
}

fn sqlite_busy(code: i32) -> bool {
    matches!(code & 0xff, 5 | 6)
}

fn sqlx_busy(e: &sqlx::Error) -> bool {
    e.as_database_error()
        .and_then(|d| d.code())
        .and_then(|c| c.parse::<i32>().ok())
        .is_some_and(sqlite_busy)
}

fn kernel_busy(e: &fongo_sync_sqlite::Error) -> bool {
    match e {
        fongo_sync_sqlite::Error::Sql(e) => sqlx_busy(e),
        fongo_sync_sqlite::Error::Sqlite { code, .. } => sqlite_busy(*code),
        _ => false,
    }
}

/// Whether the sync failed because another connection held the database.
fn is_busy(e: &ClientError) -> bool {
    match e {
        ClientError::Database(e) => kernel_busy(e),
        ClientError::Application(e) => e
            .downcast_ref::<EngineError>()
            .is_some_and(|e| matches!(e, EngineError::Db(e) if sqlx_busy(e))),
        _ => false,
    }
}

fn kernel_error(e: fongo_sync_sqlite::Error) -> EngineError {
    match e {
        e if kernel_busy(&e) => SyncError::Interrupted.into(),
        fongo_sync_sqlite::Error::Corrupt(what) if what.contains("layout") => {
            SyncError::SchemaMismatch.into()
        }
        e => kernel(e),
    }
}

/// What a client error means to a frontend. The client's messages carry no secret, but a
/// recovery code's error can quote a character of the code, so that one is never shown.
fn engine_error(e: ClientError) -> EngineError {
    if is_busy(&e) {
        return SyncError::Interrupted.into();
    }
    let e = match unwrap_application(e) {
        Ok(engine) => return engine,
        Err(e) => e,
    };
    match e {
        ClientError::Transport(_) => SyncError::Unreachable.into(),
        ClientError::Server { status, code } => SyncError::Refused {
            status,
            code: code.as_str().to_owned(),
        }
        .into(),
        ClientError::ClockSkew => SyncError::ClockSkew.into(),
        ClientError::NewerProtocol { .. } => SyncError::SchemaMismatch.into(),
        e @ (ClientError::MalformedResponse(_)
        | ClientError::ChangeTooLarge { .. }
        | ClientError::MaxBatchTooSmall { .. }) => SyncError::ServerFault(e.to_string()).into(),
        ClientError::RecoveryCode(_) => SyncError::CodeInvalid.into(),
        ClientError::Database(e) => kernel_error(e),
        ClientError::TransactionEnded | ClientError::ConnectionUnsound { .. } => {
            SyncError::Interrupted.into()
        }
        e => EngineError::Other(format!("sync: {e}")),
    }
}
