//! Turning sync on for a library, and syncing it: [`Engine`]'s side of the kernel client.

use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fongo_sync_client::{Apply, Error as ClientError, SyncReport, Transport, merge_applied};
use fongo_sync_sqlite::{Applied, Collected, Conflict, Replica, Resume};
use sqlx::sqlite::SqliteConnection;
use sqlx::{ConnectOptions, Connection};
use zeroize::Zeroizing;

use super::http::{HttpTransport, Origin};
use super::identity::{self, Identity, Stored};
use super::{PACE_COLLECT, Receiver, Step, kernel, replica, step_and_repair, unwrap_application};
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
    /// The library needs a schema update, and change capture could not be paused for it, so
    /// the update was not made and the library did not open. `0` says why.
    #[error("this library needs an update, and sync could not be paused for it: {0}")]
    CannotPause(String),
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
    /// The server's origin, `scheme://host[:port]`, `https` or `http`.
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
    /// The sync wrote rows here, besides collecting this device's own edits: changes another
    /// device sent, or rows it could write only now, such as one held back until an edit here
    /// freed it, or a merge of rows found to be one.
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
    /// Notes from another device whose file path is not a markdown file inside the vault. They
    /// are kept as rows but never read from or written to disk, and are not searchable.
    pub refused_notes: Vec<i64>,
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

/// The turn of the one sync, create or join running at a time. It holds no connection between
/// them: each attempt opens its own and closes it before the next, so nothing that might hold
/// the write lock outlives the attempt that took it.
#[derive(Default)]
pub(crate) struct Held {
    /// How long the last create, join or sync held the write lock since it last paused, and
    /// when it ended: a sync that follows within the pace's pause starts from it.
    last: Option<(Duration, std::time::Instant)>,
    #[cfg(test)]
    pub(crate) opened: usize,
    #[cfg(test)]
    pub(crate) busy_retries: usize,
    /// Each step of installing a create or a join took, and when it ended.
    #[cfg(test)]
    pub(crate) install_steps: Vec<(Duration, std::time::Instant)>,
    /// Stop installing after this many steps, as a process that stops part-way would.
    #[cfg(test)]
    pub(crate) stop_after_steps: Option<usize>,
    /// Rows each step of installing takes, in place of [`PACE_COLLECT`].
    #[cfg(test)]
    pub(crate) step_rows: Option<usize>,
    /// Told in each gap between steps of installing, which waits until the test answers.
    #[cfg(test)]
    pub(crate) gaps: Option<tokio::sync::mpsc::UnboundedSender<tokio::sync::oneshot::Sender<()>>>,
    /// How installing paces its steps, in place of the engine's pace.
    #[cfg(test)]
    pub(crate) pace: Option<fongo_sync_client::Pace>,
}

impl Held {
    /// [`Held::last`], as the [`Receiver::held`] of a sync about to begin.
    fn carried(&self, pace: &fongo_sync_client::Pace) -> Duration {
        self.last
            .filter(|(_, ended)| ended.elapsed() < pace.pause)
            .map_or(Duration::ZERO, |(held, _)| held)
    }
}

/// A connection of a sync's own, opened from the pool's options and never one of the pool's,
/// fresh for every attempt, so it never carries a transaction in. [`SyncConnection::close`]
/// closes it and waits until it is closed, which is what lets the next writer in at once.
/// Dropped unclosed, as when the future running a sync is dropped or panics, sqlx closes it on
/// its worker thread straight away, ending any transaction it held; nothing waits for that.
struct SyncConnection(Option<SqliteConnection>);

impl SyncConnection {
    async fn open(engine: &Engine, held: &mut Held) -> Result<SyncConnection> {
        let options = (*engine.storage.pool().connect_options())
            .clone()
            .busy_timeout(BUSY_TIMEOUT);
        let conn = options.connect().await?;
        #[cfg(test)]
        {
            held.opened += 1;
        }
        #[cfg(not(test))]
        let _ = held;
        Ok(SyncConnection(Some(conn)))
    }

    fn get(&mut self) -> &mut SqliteConnection {
        self.0
            .as_mut()
            .expect("a sync connection is used only until it is closed")
    }

    async fn close(mut self) {
        if let Some(conn) = self.0.take() {
            conn.close().await.ok();
        }
    }
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
    /// identity back out, so the library is left as it was. The install is a first step that
    /// records the rows already here, then steps that adopt them, each a transaction of its own
    /// paced as a sync paces its own, so the rest of the app keeps using the database meanwhile.
    /// Once the first step has committed the library syncs: a later step that fails leaves the
    /// rest to the next sync, which carries on from there.
    async fn install_identity(&self, held: &mut Held, identity: &Identity) -> Result<()> {
        identity::write(&self.config.sync_identity, identity)?;
        let replica = replica()?;
        let mut receiver = Receiver {
            vault: Some(self.config.vault_dir.clone()),
            ..Receiver::default()
        };
        #[cfg(test)]
        {
            receiver.faults.pace = held.pace;
        }
        let mut conn = match SyncConnection::open(self, held).await {
            Ok(conn) => conn,
            Err(e) => {
                identity::remove(&self.config.sync_identity).ok();
                return Err(e);
            }
        };
        let began = std::time::Instant::now();
        let first = match install_step(conn.get(), &replica, Step::Install, held, &receiver).await {
            Ok(first) => first,
            Err(e) => {
                conn.close().await;
                identity::remove(&self.config.sync_identity).ok();
                return Err(engine_error(e));
            }
        };
        receiver.held = began.elapsed();
        if first.more
            && let Err(error) = adopt_found(conn.get(), &replica, held, &mut receiver).await
        {
            tracing::warn!(
                error = %engine_error(error),
                "sync is on, but adopting this library's rows stopped part-way; the next sync carries on"
            );
        }
        conn.close().await;
        held.last = Some((receiver.held, std::time::Instant::now()));
        Ok(())
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
        receiver.vault = Some(self.config.vault_dir.clone());
        let mut held = self.sync.held.lock().await;
        if !self.sync_installed().await? {
            return Err(SyncError::Off.into());
        }
        let Stored::Found(identity) = identity::read(&self.config.sync_identity)? else {
            return Err(SyncError::NeedsCredentials.into());
        };
        let transport = transport(identity.origin.clone())?;
        receiver.held = held.carried(&receiver.pace());
        held.last = None;
        let result = self
            .sync_rounds(&mut held, &identity, &transport, &mut receiver)
            .await;
        if result.is_ok() {
            held.last = Some((receiver.held, std::time::Instant::now()));
        }
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
        let mut conflicts = Vec::new();
        let mut busy = 0;
        let mut prepared_wrote = false;
        loop {
            let mut conn = SyncConnection::open(self, held).await?;
            #[cfg(test)]
            if let Some(paces) = &receiver.faults.paces {
                paces.lock().unwrap().push(receiver.held);
            }
            let round = async {
                let pace = receiver.pace();
                if !pace.pause.is_zero() && !receiver.held.is_zero() && receiver.held >= pace.hold {
                    tokio::time::sleep(pace.pause).await;
                    receiver.held = Duration::ZERO;
                }
                let began = std::time::Instant::now();
                let prepared = prepare(conn.get(), &replica, receiver).await?;
                receiver.held += began.elapsed();
                prepared_wrote |= !prepared.touched.is_empty();
                fongo_sync_client::sync_with(
                    conn.get(),
                    &replica,
                    transport,
                    &identity.credentials,
                    now,
                    receiver,
                )
                .await
            }
            .await;
            conn.close().await;
            match round {
                Ok(mut report) => {
                    let more = report.more;
                    receiver.held = report.held;
                    conflicts.push(std::mem::take(&mut report.applied.conflicts));
                    add(&mut outcome, report);
                    busy = 0;
                    if !more {
                        break;
                    }
                }
                Err(ClientError::ConnectionUnsound { .. }) => {
                    return Err(SyncError::Interrupted.into());
                }
                Err(e) if busy < BUSY_RETRIES && is_busy(&e) => {
                    let backoff = BUSY_BACKOFF * 2u32.pow(busy);
                    tokio::time::sleep(backoff).await;
                    // How long the failed round held the lock is unknown, so a backoff shorter
                    // than the pace's pause is followed by that pause.
                    let pace = receiver.pace();
                    receiver.held = if backoff >= pace.pause {
                        Duration::ZERO
                    } else {
                        pace.hold
                    };
                    busy += 1;
                    #[cfg(test)]
                    {
                        held.busy_retries += 1;
                    }
                }
                Err(e) => return Err(engine_error(e)),
            }
        }
        outcome.received |= prepared_wrote;
        outcome.conflicts = self.conflicts_of(&replica, conflicts).await?;
        let after = super::after_sync(
            &self.storage,
            &self.config.images_dir,
            &self.config.vault_dir,
        )
        .await?;
        outcome.covers_to_fetch = after.covers_to_fetch;
        outcome.refused_notes = after.refused_notes;
        Ok(outcome)
    }
}

impl Engine {
    /// The conflicts of every round of one [`Engine::sync_now`], merged as one sync merges those
    /// of its applies: each looked up as the last round left it, and a local edit reported again
    /// by a later round counted once, with the later remote change.
    async fn conflicts_of(
        &self,
        replica: &Replica,
        rounds: Vec<Vec<Conflict>>,
    ) -> Result<Vec<SyncConflict>> {
        let mut merged = Applied::default();
        let last = rounds.len().saturating_sub(1);
        for (at, mut conflicts) in rounds.into_iter().enumerate() {
            if at < last && !conflicts.is_empty() {
                let mut conn = self.storage.pool().acquire().await?;
                for c in &mut conflicts {
                    if let Ok((gid, key)) = replica.group_of(&mut conn, &c.collection, c.gid).await
                    {
                        c.gid = gid;
                        c.key = key;
                    }
                }
            }
            merge_applied(
                &mut merged,
                Applied {
                    conflicts,
                    ..Applied::default()
                },
            );
        }
        Ok(merged
            .conflicts
            .into_iter()
            .map(|c| SyncConflict {
                row: SyncRow {
                    collection: c.collection,
                    key: c.key,
                },
                field: c.field,
                remote_won: c.remote_won,
            })
            .collect())
    }
}

/// Adopts registry changes, and puts change capture back if an open could not: a sync with
/// capture off would miss what was written meanwhile. Installing is a bounded step, so rows a
/// first install recorded and has not adopted yet are adopted in steps, by this and the sync's
/// own steps of collecting, never whole.
async fn prepare(
    conn: &mut SqliteConnection,
    replica: &Replica,
    receiver: &Receiver,
) -> std::result::Result<Collected, ClientError> {
    let capture_on = crate::storage::capture_installed(conn)
        .await
        .map_err(|e| ClientError::Application(Box::new(e)))?;
    if !capture_on {
        replica.resume(conn, Resume::Send).await?;
        return Ok(Collected::default());
    }
    let rows = receiver.collect_rows();
    let mut tx = conn.begin_with("BEGIN IMMEDIATE").await?;
    let (collected, timing) = step_and_repair(
        &mut tx,
        replica,
        Step::Install,
        rows,
        receiver.vault.as_deref(),
    )
    .await?;
    tx.commit().await?;
    #[cfg(test)]
    if let Some(steps) = &receiver.faults.steps {
        steps.lock().unwrap().push((timing, collected.clone()));
    }
    #[cfg(not(test))]
    let _ = timing;
    Ok(collected)
}

/// One step of installing or collecting in a transaction of its own, which takes the write lock
/// with its first statement.
async fn install_step(
    conn: &mut SqliteConnection,
    replica: &Replica,
    step: Step,
    held: &mut Held,
    receiver: &Receiver,
) -> std::result::Result<Collected, ClientError> {
    #[cfg(test)]
    let rows = held.step_rows.unwrap_or(PACE_COLLECT);
    #[cfg(not(test))]
    let rows = PACE_COLLECT;
    let began = std::time::Instant::now();
    let mut tx = conn.begin_with("BEGIN IMMEDIATE").await?;
    let (collected, _) =
        step_and_repair(&mut tx, replica, step, rows, receiver.vault.as_deref()).await?;
    tx.commit().await?;
    #[cfg(test)]
    held.install_steps
        .push((began.elapsed(), std::time::Instant::now()));
    #[cfg(not(test))]
    let _ = (began, held);
    Ok(collected)
}

/// Adopts what a first install step recorded, in steps paced as a sync paces its transactions:
/// once they have held the write lock for the pace's `hold`, the lock is left free for its
/// `pause`. Takes no more rows than waited when it began, so writes made meanwhile cannot keep
/// it going; the next sync collects those.
async fn adopt_found(
    conn: &mut SqliteConnection,
    replica: &Replica,
    held: &mut Held,
    receiver: &mut Receiver,
) -> std::result::Result<(), ClientError> {
    let pace = receiver.pace();
    let mut left = replica.uncollected_rows(conn, usize::MAX).await?;
    while left > 0 {
        #[cfg(test)]
        if held
            .stop_after_steps
            .is_some_and(|stop| held.install_steps.len() >= stop)
        {
            return Ok(());
        }
        if !pace.pause.is_zero() && receiver.held >= pace.hold {
            tokio::time::sleep(pace.pause).await;
            receiver.held = Duration::ZERO;
        }
        receiver.between(conn).await?;
        #[cfg(test)]
        if let Some(gaps) = &held.gaps {
            let (done, wait) = tokio::sync::oneshot::channel();
            if gaps.send(done).is_ok() {
                wait.await.ok();
            }
        }
        let began = std::time::Instant::now();
        let step = install_step(conn, replica, Step::Collect, held, receiver).await?;
        receiver.held += began.elapsed();
        left = left.saturating_sub(step.rows.max(1));
        if !step.more {
            break;
        }
    }
    Ok(())
}

fn add(outcome: &mut SyncOutcome, report: SyncReport) {
    let row = |collection: String, key: Option<String>| SyncRow { collection, key };
    outcome.received |= report.applied.accepted > 0 || !report.applied.touched.is_empty();
    outcome.sent |= report.sent > 0;
    outcome.blocked = report.blocked.is_some();
    outcome.held = report
        .applied
        .held
        .into_iter()
        .map(|h| row(h.collection, h.key))
        .collect();
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A sync whose applies or steps wrote rows here, though none of what they received was
    /// taken in, reports something received.
    #[test]
    fn rows_a_sync_wrote_count_as_received() {
        let mut outcome = SyncOutcome::default();
        add(&mut outcome, SyncReport::default());
        assert!(!outcome.received);
        let mut report = SyncReport::default();
        report.applied.touched.push(fongo_sync_sqlite::Touched {
            collection: "notes".into(),
            key: "[1]".into(),
            gid: fongo_sync_sqlite::core::Gid([7; 16]),
            deleted: false,
        });
        add(&mut outcome, report);
        assert!(outcome.received);
    }

    /// A sync gives way to other writers sooner than they give way to it.
    #[tokio::test]
    async fn a_sync_waits_for_the_write_lock_less_than_the_rest_of_the_engine() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(crate::config::EngineConfig::rooted_at(dir.path()))
            .await
            .unwrap();
        let mut held = Held::default();
        let mut conn = SyncConnection::open(&engine, &mut held).await.unwrap();
        let waits: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
            .fetch_one(conn.get())
            .await
            .unwrap();
        conn.close().await;
        assert_eq!(waits, 1_000);
        assert!(Duration::from_millis(waits.unsigned_abs()) < crate::storage::BUSY_TIMEOUT);
    }
}
