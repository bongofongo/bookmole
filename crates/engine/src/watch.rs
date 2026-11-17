//! Mount and vault watchers, driven by injected event channels so the debounce is testable.
//! `MountWatcher` never writes to a device and holds no `Storage`; `VaultWatcher` writes only the
//! database, never the vault. Neither spawns a task: work happens only when `next()` is polled.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::device::{mount_roots, offers_reader};
use crate::error::{EngineError, Result};
use crate::notes;
use crate::storage::Storage;

/// How long a volume holds still before it is announced: a mounting reader may hand over a sidecar
/// mid-write.
/// A const, not a setting, so the tested value is the shipped one (`MountWatcher::quiet_for`
/// collapses it in tests).
pub const MOUNT_QUIET: Duration = Duration::from_secs(2);

/// Dropping the oldest raw stirs is safe here: a stir only means "look again", and a settle
/// re-reads the filesystem.
const STIR_CAPACITY: usize = 256;

/// A mount or a mount root (`/Volumes`), never a file inside a volume; normalising is the adapter's
/// job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountStir(pub PathBuf);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MountEvent {
    Arrived(PathBuf),
    Departed(PathBuf),
}

impl MountEvent {
    pub fn path(&self) -> &Path {
        match self {
            MountEvent::Arrived(p) | MountEvent::Departed(p) => p,
        }
    }
}

/// Cancel-safe: `next` holds no state of its own, so dropping it mid-wait (as the TUI does on every
/// keypress) loses nothing.
pub struct MountWatcher {
    stirs: mpsc::Receiver<MountStir>,
    quiet: Duration,
    settling: HashMap<PathBuf, Instant>,
    /// Mounts already announced: a repeat stir about a plugged-in volume decides nothing.
    present: HashSet<PathBuf>,
    /// Decided but not yet handed out: one settle can decide several, and `next` returns one at a time.
    decided: VecDeque<MountEvent>,
    /// A stir at a root cannot name the changed volume, so it is expanded against the filesystem on
    /// settle.
    roots: Vec<PathBuf>,
    /// Keeps the platform watcher alive; dropping it closes the channel.
    _source: Option<notify::RecommendedWatcher>,
}

impl MountWatcher {
    pub fn from_stirs(stirs: mpsc::Receiver<MountStir>) -> Self {
        MountWatcher {
            stirs,
            quiet: MOUNT_QUIET,
            settling: HashMap::new(),
            present: HashSet::new(),
            decided: VecDeque::new(),
            roots: Vec::new(),
            _source: None,
        }
    }

    /// Seeded, not announced: the watcher reports transitions only, and `candidate_mounts` covers
    /// volumes already present.
    pub fn already_here(mut self, mounts: impl IntoIterator<Item = PathBuf>) -> Self {
        self.present.extend(mounts);
        self
    }

    pub fn under_roots(mut self, roots: impl IntoIterator<Item = PathBuf>) -> Self {
        self.roots.extend(roots);
        self
    }

    /// Shortens the quiet period; for tests only.
    pub fn quiet_for(mut self, quiet: Duration) -> Self {
        self.quiet = quiet;
        self
    }

    pub fn is_present(&self, mount: &Path) -> bool {
        self.present.contains(mount)
    }

    pub async fn next(&mut self) -> Option<MountEvent> {
        loop {
            if let Some(event) = self.decided.pop_front() {
                return Some(event);
            }
            let Some(deadline) = self.settling.values().min().copied() else {
                let stir = self.stirs.recv().await?;
                self.stir(stir);
                continue;
            };
            //  `timeout_at`, not `select!`: the deadline belongs to the watcher, so a cancelled call loses
            //  nothing.
            match tokio::time::timeout_at(deadline, self.stirs.recv()).await {
                Ok(Some(stir)) => self.stir(stir),
                //  A volume that arrived just before the source died still arrived: wait out the burst and decide
                //  it.
                Ok(None) => {
                    tokio::time::sleep_until(deadline).await;
                    self.settle();
                }
                Err(_) => self.settle(),
            }
        }
    }

    fn stir(&mut self, MountStir(path): MountStir) {
        self.settling.insert(path, Instant::now() + self.quiet);
    }

    fn settle(&mut self) {
        let now = Instant::now();
        //  Ordered by when each burst finished, so devices are announced in plug-in order; ties break on
        //  path.
        let mut due: Vec<(Instant, PathBuf)> = self
            .settling
            .iter()
            .filter(|(_, at)| **at <= now)
            .map(|(path, at)| (*at, path.clone()))
            .collect();
        due.sort();

        for (_, path) in due {
            self.settling.remove(&path);
            if self.roots.contains(&path) {
                for volume in self.volumes_under(&path) {
                    self.decide(volume);
                }
            } else {
                self.decide(path);
            }
        }
    }

    fn decide(&mut self, mount: PathBuf) {
        match (offers_reader(&mount), self.present.contains(&mount)) {
            (true, false) => {
                tracing::info!(mount = %mount.display(), "reader mounted");
                self.present.insert(mount.clone());
                self.decided.push_back(MountEvent::Arrived(mount));
            }
            (false, true) => {
                tracing::info!(mount = %mount.display(), "reader unmounted");
                self.present.remove(&mount);
                self.decided.push_back(MountEvent::Departed(mount));
            }
            _ => {}
        }
    }

    /// Includes announced volumes: an unmount stir at the root settles after `read_dir` can no longer
    /// see them.
    fn volumes_under(&self, root: &Path) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = std::fs::read_dir(root)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .collect();
        out.extend(
            self.present
                .iter()
                .filter(|mount| mount.parent() == Some(root))
                .cloned(),
        );
        out.sort();
        out.dedup();
        out
    }
}

/// Seeded with what is already mounted. Fails when the platform has no notification service;
/// callers should degrade, not abort.
pub fn watch_mounts() -> Result<MountWatcher> {
    let roots = mount_roots();
    if roots.is_empty() {
        return Err(EngineError::Watch(
            "no mount directory exists to watch on this machine".into(),
        ));
    }
    let (tx, rx) = mpsc::channel(STIR_CAPACITY);
    let watched = roots.clone();

    let mut source = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        let Ok(event) = result else {
            return;
        };
        for path in event.paths {
            let Some(stir) = normalize(&path, &watched) else {
                continue;
            };
            // A full queue means a burst is already pending; see `STIR_CAPACITY`.
            let _ = tx.try_send(MountStir(stir));
        }
    })
    .map_err(|e| EngineError::Watch(e.to_string()))?;

    for root in &roots {
        //  Non-recursive is load-bearing: recursing on `/Volumes` would watch every file on every mounted
        //  disk.
        notify::Watcher::watch(&mut source, root, notify::RecursiveMode::NonRecursive)
            .map_err(|e| EngineError::Watch(format!("{}: {e}", root.display())))?;
    }

    tracing::info!(roots = ?roots, "watching for readers");
    Ok(MountWatcher {
        _source: Some(source),
        ..MountWatcher::from_stirs(rx)
            .already_here(crate::device::candidate_mounts())
            .under_roots(roots)
    })
}

/// Reduces an event path to its volume or the root: a page turn rewrites a sidecar and must not
/// rescan the reader.
fn normalize(path: &Path, roots: &[PathBuf]) -> Option<PathBuf> {
    for root in roots {
        if path == root {
            return Some(root.clone());
        }
        if let Ok(rest) = path.strip_prefix(root)
            && let Some(first) = rest.components().next()
        {
            return Some(root.join(first));
        }
    }
    None
}

/// Much shorter than `MOUNT_QUIET`: a save is a temp file, rename and touch, and a search box is
/// waiting on it.
/// A const, not a setting, for the same reason.
pub const VAULT_QUIET: Duration = Duration::from_millis(400);

/// A path to a file, unlike `MountStir`: a vault is our own directory, so there is no reduction to
/// a volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultStir(pub PathBuf);

/// Carries the note id, not the path: vault paths are private reading and must not tempt a frontend
/// to log them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultEvent {
    Reindexed { note_id: i64 },
    /// Nothing was written: absence is never destructive.
    Vanished { note_id: i64 },
}

impl VaultEvent {
    pub fn note_id(&self) -> i64 {
        match self {
            VaultEvent::Reindexed { note_id } | VaultEvent::Vanished { note_id } => *note_id,
        }
    }
}

/// Follows the vault so a note edited elsewhere stays findable; writes only the unwatched database.
/// A vanished file writes nothing: row, FTS entry and edges stay (deletion is `delete_note`).
/// Cancel-safe: deadlines live in the struct, and a path leaves `settling` only after its
/// refresh commits.
pub struct VaultWatcher {
    stirs: mpsc::Receiver<VaultStir>,
    quiet: Duration,
    settling: HashMap<PathBuf, Instant>,
    decided: VecDeque<VaultEvent>,
    vault: PathBuf,
    storage: Storage,
    _source: Option<notify::RecommendedWatcher>,
}

impl VaultWatcher {
    pub fn from_stirs(
        vault: impl Into<PathBuf>,
        storage: Storage,
        stirs: mpsc::Receiver<VaultStir>,
    ) -> Self {
        VaultWatcher {
            stirs,
            quiet: VAULT_QUIET,
            settling: HashMap::new(),
            decided: VecDeque::new(),
            vault: vault.into(),
            storage,
            _source: None,
        }
    }

    pub fn quiet_for(mut self, quiet: Duration) -> Self {
        self.quiet = quiet;
        self
    }

    /// The work happens here, on the caller's task, when polled; there is no background thread.
    pub async fn next(&mut self) -> Option<VaultEvent> {
        loop {
            if let Some(event) = self.decided.pop_front() {
                return Some(event);
            }
            let Some(deadline) = self.settling.values().min().copied() else {
                let stir = self.stirs.recv().await?;
                self.stir(stir);
                continue;
            };
            match tokio::time::timeout_at(deadline, self.stirs.recv()).await {
                Ok(Some(stir)) => self.stir(stir),
                Ok(None) => {
                    tokio::time::sleep_until(deadline).await;
                    self.settle().await;
                }
                Err(_) => self.settle().await,
            }
        }
    }

    fn stir(&mut self, VaultStir(path): VaultStir) {
        self.settling.insert(path, Instant::now() + self.quiet);
    }

    async fn settle(&mut self) {
        let now = Instant::now();
        let mut due: Vec<(Instant, PathBuf)> = self
            .settling
            .iter()
            .filter(|(_, at)| **at <= now)
            .map(|(path, at)| (*at, path.clone()))
            .collect();
        due.sort();

        for (_, path) in due {
            match self.decide(&path).await {
                Ok(Decision::Settled(event)) => {
                    self.settling.remove(&path);
                    if let Some(event) = event {
                        self.decided.push_back(event);
                    }
                }
                Ok(Decision::StillWriting) => {
                    self.settling.insert(path, Instant::now() + self.quiet);
                }
                Err(e) => {
                    //  Dropped, not re-armed: a path failing for a durable reason would spin. Log the error, never the
                    //  path.
                    tracing::warn!(error = %e, "could not re-index a note from the vault");
                    self.settling.remove(&path);
                }
            }
        }
    }

    async fn decide(&self, path: &Path) -> Result<Decision> {
        //  Unclaimed files are not adopted: a stray file is not a note bookmole owns, and no row is
        //  invented for it.
        let Some(rel) = notes::vault_relative(&self.vault, path) else {
            return Ok(Decision::Settled(None));
        };
        let Some(note) = self.storage.note_by_path(&rel).await? else {
            return Ok(Decision::Settled(None));
        };

        let body = match settled_read(path)? {
            Settled::Gone => {
                tracing::trace!(note = note.id, "a note's file is not there");
                return Ok(Decision::Settled(Some(VaultEvent::Vanished {
                    note_id: note.id,
                })));
            }
            Settled::Moving => return Ok(Decision::StillWriting),
            Settled::Content(content) => content,
        };
        let (_, body) = notes::frontmatter_and_body(&body);

        if notes::reindex_from_body(&self.storage, note.id, &note.title, body).await? {
            Ok(Decision::Settled(Some(VaultEvent::Reindexed {
                note_id: note.id,
            })))
        } else {
            Ok(Decision::Settled(None))
        }
    }
}

enum Decision {
    Settled(Option<VaultEvent>),
    StillWriting,
}

enum Settled {
    Gone,
    Moving,
    Content(String),
}

fn settled_read(path: &Path) -> Result<Settled> {
    settled_read_with(path, |p| std::fs::read_to_string(p))
}

fn settled_read_with<R>(path: &Path, read: R) -> Result<Settled>
where
    R: FnOnce(&Path) -> std::io::Result<String>,
{
    let before = stamp(path);
    let content = match read(path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Settled::Gone),
        Err(e) => return Err(e.into()),
    };
    // `before` is `None` when the file appeared between stat and read: no version has held still yet.
    if before.is_none() || before != stamp(path) {
        return Ok(Settled::Moving);
    }
    Ok(Settled::Content(content))
}

fn stamp(path: &Path) -> Option<(u64, std::time::SystemTime)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

/// Recursive, unlike `watch_mounts`: a non-recursive watch would see book directories but none of
/// the notes.
/// Non-markdown paths are dropped in the callback so noise like `.obsidian/workspace.json` cannot
/// fill the channel.
pub fn watch_vault(vault: &Path, storage: Storage) -> Result<VaultWatcher> {
    if !vault.is_dir() {
        return Err(EngineError::Watch(format!(
            "{}: no vault directory to watch",
            vault.display()
        )));
    }
    let (tx, rx) = mpsc::channel(STIR_CAPACITY);

    let mut source = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        let Ok(event) = result else {
            return;
        };
        for path in event.paths {
            if !path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("md"))
            {
                continue;
            }
            //  A full queue means a burst is pending; the start-up sweep recovers a dropped stir. See
            //  `STIR_CAPACITY`.
            let _ = tx.try_send(VaultStir(path));
        }
    })
    .map_err(|e| EngineError::Watch(e.to_string()))?;

    notify::Watcher::watch(&mut source, vault, notify::RecursiveMode::Recursive)
        .map_err(|e| EngineError::Watch(format!("{}: {e}", vault.display())))?;

    //  Log only the vault root; nothing logs a path inside it above `trace!`, since note paths are
    //  prose.
    tracing::info!(vault = %vault.display(), "watching the vault");
    Ok(VaultWatcher {
        _source: Some(source),
        ..VaultWatcher::from_stirs(vault, storage, rx)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUIET: Duration = Duration::from_millis(100);

    fn install(mount: &Path) {
        let dir = mount.join("koreader");
        std::fs::create_dir_all(dir.join("frontend")).unwrap();
        std::fs::create_dir_all(dir.join("plugins")).unwrap();
        std::fs::write(dir.join("reader.lua"), "-- entry point\n").unwrap();
    }

    fn watcher() -> (mpsc::Sender<MountStir>, MountWatcher) {
        let (tx, rx) = mpsc::channel(16);
        (tx, MountWatcher::from_stirs(rx).quiet_for(QUIET))
    }

    #[tokio::test(start_paused = true)]
    async fn a_burst_of_events_becomes_one_arrival_and_not_before_it_stops() {
        let tmp = tempfile::tempdir().unwrap();
        let mount = tmp.path().join("KOBOeReader");
        std::fs::create_dir_all(&mount).unwrap();
        install(&mount);

        let (tx, mut watcher) = watcher();
        for _ in 0..12 {
            tx.send(MountStir(mount.clone())).await.unwrap();
        }

        assert!(
            tokio::time::timeout(QUIET / 2, watcher.next())
                .await
                .is_err(),
            "announced before the burst had finished"
        );

        assert_eq!(
            tokio::time::timeout(QUIET * 2, watcher.next())
                .await
                .unwrap(),
            Some(MountEvent::Arrived(mount.clone())),
        );
        assert!(
            tokio::time::timeout(QUIET * 2, watcher.next())
                .await
                .is_err(),
            "one mount, one arrival"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_device_that_keeps_writing_keeps_the_scan_waiting() {
        let tmp = tempfile::tempdir().unwrap();
        let mount = tmp.path().join("Kindle");
        std::fs::create_dir_all(&mount).unwrap();
        install(&mount);

        let (tx, mut watcher) = watcher();
        for _ in 0..5 {
            tx.send(MountStir(mount.clone())).await.unwrap();
            assert!(
                tokio::time::timeout(QUIET * 2 / 3, watcher.next())
                    .await
                    .is_err()
            );
        }
        assert_eq!(
            tokio::time::timeout(QUIET * 2, watcher.next())
                .await
                .unwrap(),
            Some(MountEvent::Arrived(mount)),
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_mount_that_is_still_there_is_never_announced_twice() {
        let tmp = tempfile::tempdir().unwrap();
        let mount = tmp.path().join("PocketBook");
        std::fs::create_dir_all(&mount).unwrap();
        install(&mount);

        let (tx, mut watcher) = watcher();
        tx.send(MountStir(mount.clone())).await.unwrap();
        assert_eq!(
            tokio::time::timeout(QUIET * 2, watcher.next())
                .await
                .unwrap(),
            Some(MountEvent::Arrived(mount.clone())),
        );

        tokio::time::advance(Duration::from_secs(60)).await;
        for _ in 0..3 {
            tx.send(MountStir(mount.clone())).await.unwrap();
        }
        assert!(
            tokio::time::timeout(QUIET * 4, watcher.next())
                .await
                .is_err(),
            "the same reader was announced twice"
        );
        assert!(watcher.is_present(&mount));
    }

    #[tokio::test(start_paused = true)]
    async fn unplugging_is_announced_and_replugging_arrives_again() {
        let tmp = tempfile::tempdir().unwrap();
        let mount = tmp.path().join("KOBOeReader");
        std::fs::create_dir_all(&mount).unwrap();
        install(&mount);

        let (tx, mut watcher) = watcher();
        tx.send(MountStir(mount.clone())).await.unwrap();
        assert_eq!(
            tokio::time::timeout(QUIET * 2, watcher.next())
                .await
                .unwrap(),
            Some(MountEvent::Arrived(mount.clone())),
        );

        std::fs::remove_dir_all(&mount).unwrap();
        tx.send(MountStir(mount.clone())).await.unwrap();
        assert_eq!(
            tokio::time::timeout(QUIET * 2, watcher.next())
                .await
                .unwrap(),
            Some(MountEvent::Departed(mount.clone())),
        );
        assert!(!watcher.is_present(&mount));

        std::fs::create_dir_all(&mount).unwrap();
        install(&mount);
        tx.send(MountStir(mount.clone())).await.unwrap();
        assert_eq!(
            tokio::time::timeout(QUIET * 2, watcher.next())
                .await
                .unwrap(),
            Some(MountEvent::Arrived(mount)),
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_volume_without_koreader_on_it_is_never_announced() {
        let tmp = tempfile::tempdir().unwrap();
        let stick = tmp.path().join("BACKUP");
        std::fs::create_dir_all(stick.join("koreader")).unwrap();
        std::fs::write(stick.join("koreader/notes.txt"), "mine").unwrap();

        let (tx, mut watcher) = watcher();
        tx.send(MountStir(stick.clone())).await.unwrap();
        assert!(
            tokio::time::timeout(QUIET * 4, watcher.next())
                .await
                .is_err(),
            "a plain volume was offered as a reader"
        );
        assert!(!watcher.is_present(&stick));
    }

    #[tokio::test(start_paused = true)]
    async fn a_reader_that_was_already_here_is_not_an_arrival() {
        let tmp = tempfile::tempdir().unwrap();
        let mount = tmp.path().join("Kindle");
        std::fs::create_dir_all(&mount).unwrap();
        install(&mount);

        let (tx, rx) = mpsc::channel(16);
        let mut watcher = MountWatcher::from_stirs(rx)
            .quiet_for(QUIET)
            .already_here([mount.clone()]);

        tx.send(MountStir(mount.clone())).await.unwrap();
        assert!(
            tokio::time::timeout(QUIET * 4, watcher.next())
                .await
                .is_err()
        );
        std::fs::remove_dir_all(&mount).unwrap();
        tx.send(MountStir(mount.clone())).await.unwrap();
        assert_eq!(
            tokio::time::timeout(QUIET * 2, watcher.next())
                .await
                .unwrap(),
            Some(MountEvent::Departed(mount)),
        );
    }

    #[tokio::test(start_paused = true)]
    async fn two_readers_are_announced_in_the_order_they_settled() {
        let tmp = tempfile::tempdir().unwrap();
        let first = tmp.path().join("Kindle");
        let second = tmp.path().join("KOBOeReader");
        for mount in [&first, &second] {
            std::fs::create_dir_all(mount).unwrap();
            install(mount);
        }

        let (tx, mut watcher) = watcher();
        tx.send(MountStir(first.clone())).await.unwrap();
        //  Polled, so the first burst is armed before the second is sent: an unread stir has not started
        //  its quiet period.
        assert!(
            tokio::time::timeout(QUIET / 4, watcher.next())
                .await
                .is_err()
        );
        tx.send(MountStir(second.clone())).await.unwrap();

        assert_eq!(
            tokio::time::timeout(QUIET * 2, watcher.next())
                .await
                .unwrap(),
            Some(MountEvent::Arrived(first)),
        );
        assert_eq!(
            tokio::time::timeout(QUIET * 2, watcher.next())
                .await
                .unwrap(),
            Some(MountEvent::Arrived(second)),
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_stir_at_the_root_finds_both_the_arrival_and_the_departure() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let mount = root.join("KOBOeReader");
        std::fs::create_dir_all(&mount).unwrap();
        install(&mount);

        let (tx, rx) = mpsc::channel(16);
        let mut watcher = MountWatcher::from_stirs(rx)
            .quiet_for(QUIET)
            .under_roots([root.clone()]);

        tx.send(MountStir(root.clone())).await.unwrap();
        assert_eq!(
            tokio::time::timeout(QUIET * 2, watcher.next())
                .await
                .unwrap(),
            Some(MountEvent::Arrived(mount.clone())),
        );

        std::fs::remove_dir_all(&mount).unwrap();
        tx.send(MountStir(root)).await.unwrap();
        assert_eq!(
            tokio::time::timeout(QUIET * 2, watcher.next())
                .await
                .unwrap(),
            Some(MountEvent::Departed(mount)),
        );
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_the_wait_does_not_lose_the_arrival() {
        let tmp = tempfile::tempdir().unwrap();
        let mount = tmp.path().join("Kindle");
        std::fs::create_dir_all(&mount).unwrap();
        install(&mount);

        let (tx, mut watcher) = watcher();
        tx.send(MountStir(mount.clone())).await.unwrap();
        for _ in 0..8 {
            //  Eight polls come to half the quiet period, so the arrival is the debounce firing, not the last
            //  cancellation.
            assert!(
                tokio::time::timeout(QUIET / 16, watcher.next())
                    .await
                    .is_err()
            );
        }
        assert_eq!(
            tokio::time::timeout(QUIET * 2, watcher.next())
                .await
                .unwrap(),
            Some(MountEvent::Arrived(mount)),
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_dead_source_decides_what_it_saw_and_then_ends() {
        let tmp = tempfile::tempdir().unwrap();
        let mount = tmp.path().join("Kindle");
        std::fs::create_dir_all(&mount).unwrap();
        install(&mount);

        let (tx, mut watcher) = watcher();
        tx.send(MountStir(mount.clone())).await.unwrap();
        drop(tx);

        assert_eq!(
            tokio::time::timeout(QUIET * 2, watcher.next())
                .await
                .unwrap(),
            Some(MountEvent::Arrived(mount)),
        );
        assert_eq!(
            tokio::time::timeout(QUIET * 2, watcher.next())
                .await
                .unwrap(),
            None,
            "a closed source must end the stream rather than spin"
        );
    }

    #[test]
    fn an_event_is_reduced_to_the_volume_it_happened_on() {
        let roots = vec![PathBuf::from("/Volumes"), PathBuf::from("/media/me")];
        assert_eq!(
            normalize(
                Path::new("/Volumes/KOBOeReader/books/x.sdr/metadata.epub.lua"),
                &roots
            ),
            Some(PathBuf::from("/Volumes/KOBOeReader")),
        );
        assert_eq!(
            normalize(Path::new("/media/me/Kindle"), &roots),
            Some(PathBuf::from("/media/me/Kindle")),
        );
        assert_eq!(
            normalize(Path::new("/Volumes"), &roots),
            Some(PathBuf::from("/Volumes")),
        );
        assert_eq!(normalize(Path::new("/home/me/books"), &roots), None);
    }
}

#[cfg(test)]
mod vault_tests {
    use super::*;
    use crate::NoteScope;
    use crate::notes::{NewNoteInput, create_note};

    /// Real time, not `start_paused`: settles await SQLite on another thread, and an idle paused
    /// runtime auto-advances its clock.
    const QUIET: Duration = Duration::from_millis(120);
    const EVENTUALLY: Duration = Duration::from_secs(5);

    struct Vault {
        _dir: tempfile::TempDir,
        root: PathBuf,
        storage: Storage,
    }

    impl Vault {
        async fn new() -> Vault {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("vault");
            std::fs::create_dir_all(&root).unwrap();
            //  A file database, not `sqlite::memory:`: these tests drop futures mid-query, after which sqlx
            //  discards the
            // connection and an in-memory pool would open a fresh, unmigrated database.
            let db = dir.path().join("notes.db");
            let storage = Storage::connect(&format!("sqlite:{}", db.display()))
                .await
                .unwrap();
            Vault {
                _dir: dir,
                root,
                storage,
            }
        }

        async fn note(&self, body: &str) -> (i64, PathBuf) {
            self.titled(None, body).await
        }

        /// Pins the title: a derived title is indexed beside the body, so a body test needs a title without
        /// the body's words.
        async fn titled(&self, title: Option<&str>, body: &str) -> (i64, PathBuf) {
            let created = create_note(
                &self.storage,
                &self.root,
                None,
                NewNoteInput {
                    title: title.map(str::to_string),
                    body: body.to_string(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            (created.id, created.file)
        }

        fn watcher(&self) -> (mpsc::Sender<VaultStir>, VaultWatcher) {
            let (tx, rx) = mpsc::channel(16);
            (
                tx,
                VaultWatcher::from_stirs(&self.root, self.storage.clone(), rx).quiet_for(QUIET),
            )
        }

        async fn finds(&self, query: &str) -> Vec<i64> {
            self.storage
                .search_marks(query, Some(crate::SearchSource::Note), None, 10)
                .await
                .unwrap()
                .iter()
                .filter_map(|h| h.as_note())
                .map(|n| n.id)
                .collect()
        }
    }

    #[tokio::test]
    async fn an_edit_made_behind_our_back_is_searchable_afterwards() {
        let v = Vault::new().await;
        let (id, path) = v
            .titled(
                Some("Middlemarch, chapter 4"),
                "Dorothea's dignity under pressure.",
            )
            .await;
        assert_eq!(v.finds("dignity").await, vec![id]);

        let content = std::fs::read_to_string(&path).unwrap();
        let (header, _) = crate::notes::frontmatter_and_body(&content);
        std::fs::write(&path, format!("{header}Casaubon's calculation instead.\n")).unwrap();

        let (tx, mut watcher) = v.watcher();
        tx.send(VaultStir(path)).await.unwrap();
        assert_eq!(
            tokio::time::timeout(EVENTUALLY, watcher.next())
                .await
                .unwrap(),
            Some(VaultEvent::Reindexed { note_id: id }),
        );

        assert_eq!(v.finds("calculation").await, vec![id], "the new text");
        assert!(v.finds("dignity").await.is_empty(), "the old text lingered");
    }

    #[tokio::test]
    async fn our_own_write_is_not_re_indexed() {
        let v = Vault::new().await;
        let (_, path) = v.note("A thought that we saved ourselves.").await;

        let (tx, mut watcher) = v.watcher();
        for _ in 0..6 {
            tx.send(VaultStir(path.clone())).await.unwrap();
        }
        assert!(
            tokio::time::timeout(QUIET * 8, watcher.next())
                .await
                .is_err(),
            "the watcher re-indexed a file the engine had just written"
        );
    }

    #[tokio::test]
    async fn a_no_op_save_is_not_an_edit() {
        let v = Vault::new().await;
        let (_, path) = v.note("Unchanged prose.").await;
        let content = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, &content).unwrap();

        let (tx, mut watcher) = v.watcher();
        tx.send(VaultStir(path)).await.unwrap();
        assert!(
            tokio::time::timeout(QUIET * 8, watcher.next())
                .await
                .is_err(),
        );
    }

    #[tokio::test]
    async fn a_trailing_newline_is_not_an_edit() {
        let v = Vault::new().await;
        let (_, path) = v.note("Prose with an opinionated editor.").await;
        let content = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, format!("{content}\n\n")).unwrap();

        let (tx, mut watcher) = v.watcher();
        tx.send(VaultStir(path)).await.unwrap();
        assert!(
            tokio::time::timeout(QUIET * 8, watcher.next())
                .await
                .is_err(),
        );
    }

    #[tokio::test]
    async fn a_burst_of_saves_becomes_one_reindex_and_not_before_it_stops() {
        let v = Vault::new().await;
        let (id, path) = v.note("first draft").await;

        let (tx, mut watcher) = v.watcher();
        let content = std::fs::read_to_string(&path).unwrap();
        let (header, _) = crate::notes::frontmatter_and_body(&content);
        std::fs::write(&path, format!("{header}second draft\n")).unwrap();
        for _ in 0..12 {
            tx.send(VaultStir(path.clone())).await.unwrap();
        }

        assert!(
            tokio::time::timeout(QUIET / 3, watcher.next())
                .await
                .is_err(),
            "indexed before the writer had finished"
        );
        assert_eq!(
            tokio::time::timeout(EVENTUALLY, watcher.next())
                .await
                .unwrap(),
            Some(VaultEvent::Reindexed { note_id: id }),
        );
        assert!(
            tokio::time::timeout(QUIET * 8, watcher.next())
                .await
                .is_err(),
            "one save, one re-index"
        );
    }

    #[tokio::test]
    async fn a_vanished_file_does_not_delete_the_note() {
        let v = Vault::new().await;
        let (id, path) = v.note("A note somebody moved out of the way.").await;
        std::fs::remove_file(&path).unwrap();

        let (tx, mut watcher) = v.watcher();
        tx.send(VaultStir(path)).await.unwrap();
        assert_eq!(
            tokio::time::timeout(EVENTUALLY, watcher.next())
                .await
                .unwrap(),
            Some(VaultEvent::Vanished { note_id: id }),
        );

        assert_eq!(v.finds("somebody").await, vec![id], "an absence deleted it");
        assert!(v.storage.get_note(id).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn a_file_moved_out_and_back_finds_its_note_again() {
        let v = Vault::new().await;
        let (id, path) = v.note("Original wording.").await;
        let content = std::fs::read_to_string(&path).unwrap();
        let (header, _) = crate::notes::frontmatter_and_body(&content);

        let (tx, mut watcher) = v.watcher();
        std::fs::remove_file(&path).unwrap();
        tx.send(VaultStir(path.clone())).await.unwrap();
        assert_eq!(
            tokio::time::timeout(EVENTUALLY, watcher.next())
                .await
                .unwrap(),
            Some(VaultEvent::Vanished { note_id: id }),
        );

        std::fs::write(&path, format!("{header}Rewritten while it was away.\n")).unwrap();
        tx.send(VaultStir(path)).await.unwrap();
        assert_eq!(
            tokio::time::timeout(EVENTUALLY, watcher.next())
                .await
                .unwrap(),
            Some(VaultEvent::Reindexed { note_id: id }),
        );
        assert_eq!(v.finds("Rewritten").await, vec![id]);
    }

    #[tokio::test]
    async fn never_writes_to_the_vault() {
        let v = Vault::new().await;
        let mut paths = Vec::new();
        for i in 0..4 {
            let (_, path) = v.note(&format!("Note number {i} about something.")).await;
            paths.push(path);
        }
        let stray = v.root.join("unsorted/stray.md");
        std::fs::write(&stray, "not ours\n").unwrap();
        paths.push(stray);
        let content = std::fs::read_to_string(&paths[0]).unwrap();
        let (header, _) = crate::notes::frontmatter_and_body(&content);
        std::fs::write(&paths[0], format!("{header}Edited elsewhere.\n")).unwrap();

        let before = snapshot(&v.root);
        let (tx, mut watcher) = v.watcher();
        for path in &paths {
            tx.send(VaultStir(path.clone())).await.unwrap();
        }
        while tokio::time::timeout(QUIET * 8, watcher.next())
            .await
            .is_ok_and(|e| e.is_some())
        {}

        assert_eq!(before, snapshot(&v.root), "the watcher wrote to the vault");
    }

    #[tokio::test]
    async fn an_unclaimed_markdown_file_is_not_adopted() {
        let v = Vault::new().await;
        let stray = v.root.join("someone-elses.md");
        std::fs::write(&stray, "# Written in Obsidian, never here\n").unwrap();

        let (tx, mut watcher) = v.watcher();
        tx.send(VaultStir(stray)).await.unwrap();
        assert!(
            tokio::time::timeout(QUIET * 8, watcher.next())
                .await
                .is_err(),
        );
        assert!(
            v.storage
                .list_notes(NoteScope::All, None)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_wikilink_added_from_outside_lands_in_the_graph() {
        let v = Vault::new().await;
        let (target, _) = v.note("Vocation").await;
        let (id, path) = v.note("A thought with no links yet.").await;
        assert!(v.storage.outgoing_links(id).await.unwrap().is_empty());

        let content = std::fs::read_to_string(&path).unwrap();
        let (header, _) = crate::notes::frontmatter_and_body(&content);
        std::fs::write(&path, format!("{header}Now it points at [[Vocation]].\n")).unwrap();

        let (tx, mut watcher) = v.watcher();
        tx.send(VaultStir(path)).await.unwrap();
        tokio::time::timeout(EVENTUALLY, watcher.next())
            .await
            .unwrap()
            .unwrap();

        let out = v.storage.outgoing_links(id).await.unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].to.as_ref().map(|n| n.id), Some(target));
    }

    #[tokio::test]
    async fn cancelling_the_wait_does_not_lose_the_edit() {
        let v = Vault::new().await;
        let (id, path) = v.note("before").await;
        let content = std::fs::read_to_string(&path).unwrap();
        let (header, _) = crate::notes::frontmatter_and_body(&content);
        std::fs::write(&path, format!("{header}afterwards\n")).unwrap();

        let (tx, mut watcher) = v.watcher();
        tx.send(VaultStir(path)).await.unwrap();
        for _ in 0..40 {
            if tokio::time::timeout(QUIET / 10, watcher.next())
                .await
                .is_ok()
            {
                break;
            }
        }
        assert_eq!(v.finds("afterwards").await, vec![id]);
    }

    #[tokio::test]
    async fn a_dead_source_decides_what_it_saw_and_then_ends() {
        let v = Vault::new().await;
        let (id, path) = v.note("draft").await;
        let content = std::fs::read_to_string(&path).unwrap();
        let (header, _) = crate::notes::frontmatter_and_body(&content);
        std::fs::write(&path, format!("{header}final\n")).unwrap();

        let (tx, mut watcher) = v.watcher();
        tx.send(VaultStir(path)).await.unwrap();
        drop(tx);

        assert_eq!(
            tokio::time::timeout(EVENTUALLY, watcher.next())
                .await
                .unwrap(),
            Some(VaultEvent::Reindexed { note_id: id }),
        );
        assert_eq!(
            tokio::time::timeout(EVENTUALLY, watcher.next())
                .await
                .unwrap(),
            None,
            "a closed source must end the stream rather than spin"
        );
    }

    #[test]
    fn a_file_that_moves_while_we_read_it_is_not_believed() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("note.md");
        std::fs::write(&path, "half a sen").unwrap();

        let mid_write = |p: &Path| {
            let got = std::fs::read_to_string(p)?;
            std::fs::write(p, "half a sentence, then the rest of it")?;
            Ok(got)
        };
        assert!(matches!(
            settled_read_with(&path, mid_write).unwrap(),
            Settled::Moving
        ));

        assert!(matches!(
            settled_read(&path).unwrap(),
            Settled::Content(c) if c == "half a sentence, then the rest of it"
        ));

        std::fs::remove_file(&path).unwrap();
        assert!(matches!(settled_read(&path).unwrap(), Settled::Gone));
    }

    /// `notes.file_path` uses `/` separators; reducing with the platform's would find nothing on
    /// Windows.
    #[test]
    fn a_path_is_reduced_to_what_the_row_says() {
        let vault = Path::new("/data/vault");
        assert_eq!(
            crate::notes::vault_relative(vault, Path::new("/data/vault/middlemarch/2026-a.md")),
            Some("middlemarch/2026-a.md".to_string()),
        );
        assert_eq!(
            crate::notes::vault_relative(vault, Path::new("/data/vault/.obsidian/workspace.json")),
            None,
        );
        assert_eq!(
            crate::notes::vault_relative(vault, Path::new("/elsewhere/note.md")),
            None,
        );
        assert_eq!(crate::notes::vault_relative(vault, vault), None);
    }

    fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    out.push((path.clone(), std::fs::read(&path).unwrap()));
                }
            }
        }
        out.sort();
        out
    }
}
