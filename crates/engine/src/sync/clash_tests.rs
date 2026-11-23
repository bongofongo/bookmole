//! Rows from several devices that clash on a partial unique index. A device receives another's
//! changes page by page through [`receive`], in order, and calls [`settle_clashes`] once it has
//! received everything sent so far, as a pull does.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock};

use fongo_sync_sqlite::Replica;
use fongo_sync_sqlite::core::{ALIAS, Change};
use rand_chacha::ChaCha8Rng;
use rand_core::{RngCore, SeedableRng};
use sqlx::SqliteConnection;

use super::{Received, receive, replica, settle_clashes};
use crate::storage::{NewNoteMeta, Storage};

struct Device {
    replica: Replica,
    storage: Storage,
    /// Every change this device has sent, in order.
    sent: Vec<Change>,
    /// Where each of its sends ends in `sent`.
    sends: Vec<usize>,
}

/// Devices that each send what they write and receive any part of what another sent.
struct Net {
    devices: Vec<Device>,
    /// `got[to][from]`: how much of what `from` sent `to` has received, in order.
    got: Vec<Vec<usize>>,
    /// How much each device had sent when the devices were set up; only what follows is received
    /// again to check that receiving twice changes nothing.
    setup: Vec<usize>,
    /// Where this net's databases live.
    dir: tempfile::TempDir,
}

impl Net {
    async fn new(n: usize) -> Net {
        let mut net = Net {
            devices: Vec::new(),
            got: vec![vec![0; n]; n],
            setup: vec![0; n],
            dir: tempfile::tempdir().unwrap(),
        };
        for d in 0..n {
            let storage = Storage::open(&net.url(d)).await.unwrap();
            net.join(storage).await;
        }
        net
    }

    fn file(&self, d: usize) -> std::path::PathBuf {
        self.dir.path().join(format!("{d}.db"))
    }

    fn url(&self, d: usize) -> String {
        format!("sqlite://{}", self.file(d).display())
    }

    async fn join(&mut self, storage: Storage) {
        let replica = replica().unwrap();
        replica
            .install(&mut storage.pool().acquire().await.unwrap())
            .await
            .unwrap();
        self.add(storage, replica);
    }

    fn add(&mut self, storage: Storage, replica: Replica) {
        self.devices.push(Device {
            replica,
            storage,
            sent: Vec::new(),
            sends: Vec::new(),
        });
    }

    fn s(&self, d: usize) -> &Storage {
        &self.devices[d].storage
    }

    /// Sends what device `d` wrote since it last sent. Returns how many changes that was.
    async fn publish(&mut self, d: usize) -> usize {
        let device = &mut self.devices[d];
        let mut conn = device.storage.pool().acquire().await.unwrap();
        device.replica.collect(&mut conn).await.unwrap();
        let out = device.replica.outbox(&mut conn, 100_000).await.unwrap();
        device
            .replica
            .acknowledge(&mut conn, out.through)
            .await
            .unwrap();
        device.sent.extend(out.changes.iter().cloned());
        if !out.changes.is_empty() {
            device.sends.push(device.sent.len());
        }
        out.changes.len()
    }

    /// Sends what device `d` wrote since it last sent, and counts what of it is not the kernel
    /// recording that two entities are one.
    async fn decisions(&mut self, d: usize) -> usize {
        let before = self.devices[d].sent.len();
        self.publish(d).await;
        self.devices[d].sent[before..]
            .iter()
            .filter(|c| c.field != ALIAS)
            .count()
    }

    /// How much of what `from` sent and `to` has not received is a decision, not the kernel
    /// recording that two entities are one.
    fn undelivered_decisions(&self, from: usize, to: usize) -> usize {
        self.devices[from].sent[self.got[to][from]..]
            .iter()
            .filter(|c| c.field != ALIAS)
            .count()
    }

    async fn publish_all(&mut self) {
        for d in 0..self.devices.len() {
            self.publish(d).await;
        }
    }

    fn pending(&self, from: usize, to: usize) -> usize {
        self.devices[from].sent.len() - self.got[to][from]
    }

    /// Whether `to` has received everything every other device has sent.
    fn caught_up(&self, to: usize) -> bool {
        (0..self.devices.len()).all(|from| from == to || self.pending(from, to) == 0)
    }

    /// `to` receives the next `n` changes `from` sent, as one page.
    async fn deliver(&mut self, from: usize, to: usize, n: usize) -> Option<Received> {
        let start = self.got[to][from];
        self.got[to][from] = start + n;
        self.redeliver(from, to, start, start + n).await
    }

    /// `to` receives `from`'s changes `start..end`, again or for the first time.
    async fn redeliver(
        &self,
        from: usize,
        to: usize,
        start: usize,
        end: usize,
    ) -> Option<Received> {
        let changes = &self.devices[from].sent[start..end];
        if changes.is_empty() {
            return None;
        }
        let t = &self.devices[to];
        Some(receive(&t.storage, &t.replica, changes).await.unwrap())
    }

    async fn deliver_all(&mut self, from: usize, to: usize) -> Option<Received> {
        let n = self.pending(from, to);
        self.deliver(from, to, n).await
    }

    async fn settle(&self, d: usize) -> Received {
        let device = &self.devices[d];
        settle_clashes(&device.storage, &device.replica)
            .await
            .unwrap()
    }

    /// `to` receives everything sent so far and settles.
    async fn pull(&mut self, to: usize) -> Received {
        for from in (0..self.devices.len()).filter(|&f| f != to) {
            self.deliver_all(from, to).await;
        }
        self.settle(to).await
    }

    /// Whether `cut` changes into what `from` has not yet sent `to` fall where one of `from`'s
    /// sends ends.
    fn between_sends(&self, from: usize, to: usize, cut: usize) -> bool {
        self.devices[from]
            .sends
            .contains(&(self.got[to][from] + cut))
    }

    /// `to` receives what `from` sent, cut into two pages at `cut`, settling between them when
    /// `between_sends`, as a pull that ended there would, and after them.
    async fn pull_cut(&mut self, from: usize, to: usize, cut: usize, between_sends: bool) {
        assert!(!between_sends || self.between_sends(from, to, cut));
        self.deliver(from, to, cut).await;
        if between_sends {
            self.settle(to).await;
        }
        self.pull(to).await;
    }

    /// Every device sends and pulls until none has anything left to send. Returns what each
    /// device's last settle still held back.
    async fn sync(&mut self) -> Vec<Vec<fongo_sync_sqlite::Held>> {
        self.publish_all().await;
        for _ in 0..8 {
            let mut held = Vec::new();
            for to in 0..self.devices.len() {
                held.push(self.pull(to).await.applied.held);
            }
            let mut sent = 0;
            for d in 0..self.devices.len() {
                sent += self.publish(d).await;
            }
            if sent == 0 {
                return held;
            }
        }
        panic!("the devices kept sending changes");
    }

    /// Sends and pulls until nothing is left to send, then checks that every device shows the
    /// same rows and holds nothing back.
    async fn converge(&mut self) -> Vec<String> {
        let held = self.sync().await;
        let first = view(&mut self.s(0).pool().acquire().await.unwrap()).await;
        for (d, held) in held.iter().enumerate() {
            assert!(held.is_empty(), "device {d} holds back {held:?}");
            let seen = view(&mut self.s(d).pool().acquire().await.unwrap()).await;
            assert_eq!(seen, first, "device {d} differs");
        }
        first
    }

    /// Ends the setup: what was sent so far is not received again by [`Net::quiet`].
    async fn set_up(&mut self) {
        self.sync().await;
        self.setup = self.devices.iter().map(|d| d.sent.len()).collect();
    }

    /// Sends and pulls until nothing is left to send, then checks that every device shows the
    /// same rows, holds nothing back and has nothing more to send, and that receiving everything
    /// since the setup again changes none of that.
    async fn quiet(&mut self) -> Vec<String> {
        let seen = self.converge().await;
        assert_eq!(self.settled().await, seen);
        let n = self.devices.len();
        for to in 0..n {
            for from in (0..n).filter(|&f| f != to) {
                let all = self.devices[from].sent.len();
                self.redeliver(from, to, self.setup[from], all).await;
            }
            self.settle(to).await;
        }
        assert_eq!(self.settled().await, seen, "receiving again changed rows");
        seen
    }

    /// [`Net::quiet`] when `again`, else [`Net::converge`]: receiving everything again costs as
    /// much as the run, so runs that differ only in where pages are cut check it once.
    async fn converge_or_quiet(&mut self, again: bool) -> Vec<String> {
        if again {
            self.quiet().await
        } else {
            self.converge().await
        }
    }

    async fn settled(&mut self) -> Vec<String> {
        let first = view(&mut self.s(0).pool().acquire().await.unwrap()).await;
        for d in 0..self.devices.len() {
            assert_eq!(self.publish(d).await, 0, "device {d} has more to send");
            let device = &self.devices[d];
            let mut conn = device.storage.pool().acquire().await.unwrap();
            let held = device.replica.inspect(&mut conn).await.unwrap().held;
            assert!(held.is_empty(), "device {d} holds back {held:?}");
            assert_eq!(view(&mut conn).await, first, "device {d} differs");
        }
        first
    }

    /// Delivers everything sent in a random order and in random pages, repeating some pages,
    /// settling some devices that have received everything, and sending what devices wrote as it
    /// goes.
    async fn shuffle(&mut self, rng: &mut ChaCha8Rng) {
        let n = self.devices.len();
        loop {
            let pairs: Vec<(usize, usize)> = (0..n)
                .flat_map(|from| (0..n).map(move |to| (from, to)))
                .filter(|&(from, to)| from != to && self.pending(from, to) > 0)
                .collect();
            if pairs.is_empty() {
                return;
            }
            let (from, to) = pairs[pick(rng, pairs.len())];
            let page = 1 + pick(rng, self.pending(from, to));
            self.deliver(from, to, page).await;
            if pick(rng, 4) == 0 {
                let done = self.got[to][from];
                self.redeliver(from, to, pick(rng, done), done).await;
            }
            if self.caught_up(to) && pick(rng, 2) == 0 {
                self.settle(to).await;
            }
            if pick(rng, 3) == 0 {
                self.publish(pick(rng, n)).await;
            }
        }
    }

    /// Closes the databases and keeps them as files, for each run to start from a copy.
    async fn freeze(self) -> Template {
        let sent = self
            .devices
            .iter()
            .map(|d| (d.sent.clone(), d.sends.clone()))
            .collect();
        for d in &self.devices {
            d.storage.pool().close().await;
        }
        Template {
            dir: self.dir,
            sent,
        }
    }
}

/// Devices as a setup left them, every device having received all the others sent.
struct Template {
    dir: tempfile::TempDir,
    sent: Vec<(Vec<Change>, Vec<usize>)>,
}

impl Template {
    async fn thaw(&self) -> Net {
        let n = self.sent.len();
        let lengths: Vec<usize> = self.sent.iter().map(|(s, _)| s.len()).collect();
        let mut net = Net {
            devices: Vec::new(),
            got: vec![lengths.clone(); n],
            setup: lengths,
            dir: tempfile::tempdir().unwrap(),
        };
        for (d, (sent, sends)) in self.sent.iter().enumerate() {
            std::fs::copy(self.dir.path().join(format!("{d}.db")), net.file(d)).unwrap();
            let storage = Storage::open(&net.url(d)).await.unwrap();
            net.add(storage, replica().unwrap());
            net.devices[d].sent.clone_from(sent);
            net.devices[d].sends.clone_from(sends);
        }
        net
    }
}

type Templates = Mutex<HashMap<(&'static str, usize), Arc<Template>>>;

/// `n` devices set up by `build`, which runs about once per `name` and `n` in this process.
async fn set_up<F, Fut>(name: &'static str, n: usize, build: F) -> Net
where
    F: FnOnce(usize) -> Fut,
    Fut: Future<Output = Net>,
{
    static TEMPLATES: OnceLock<Templates> = OnceLock::new();
    let all = TEMPLATES.get_or_init(Templates::default);
    let found = all.lock().unwrap().get(&(name, n)).cloned();
    let template = match found {
        Some(t) => t,
        None => {
            let mut net = build(n).await;
            net.set_up().await;
            let built = Arc::new(net.freeze().await);
            all.lock()
                .unwrap()
                .entry((name, n))
                .or_insert(built)
                .clone()
        }
    };
    template.thaw().await
}

fn pick(rng: &mut ChaCha8Rng, below: usize) -> usize {
    (rng.next_u64() % below as u64) as usize
}

/// How many random orders each three-device test tries: 2, or `BOOKMOLE_SYNC_ORDERS`.
fn random_orders() -> u64 {
    std::env::var("BOOKMOLE_SYNC_ORDERS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(2)
}

/// What a reader sees of the rows these rules touch, without local ids.
async fn view(conn: &mut SqliteConnection) -> Vec<String> {
    let mut seen = Vec::new();
    for sql in [
        "SELECT 'book ' || title || ' ' || COALESCE(isbn_10, '-') || ' ' || COALESCE(isbn_13, '-')
           FROM books",
        "SELECT 'reading ' || b.title || ' began ' || COALESCE(r.started_at, '-') || ' '
                || r.status || ' finished ' || COALESCE(r.finished_at, '-') || ' put down '
                || COALESCE(r.put_down_at, '-') || ' made ' || r.created_at
           FROM readings r JOIN books b ON b.id = r.book_id",
        "SELECT 'note ' || n.file_path || ' ' || n.kind || ' on the reading begun '
                || COALESCE(r.started_at, '-')
           FROM notes n LEFT JOIN readings r ON r.id = n.reading_id",
        "SELECT 'scale ' || name || ' ' || is_default FROM rating_scales",
    ] {
        let mut rows: Vec<String> = sqlx::query_scalar(sql).fetch_all(&mut *conn).await.unwrap();
        rows.sort();
        seen.extend(rows);
    }
    seen
}

fn middlemarch() -> crate::book::Book {
    crate::book::Book {
        title: Some("Middlemarch".into()),
        authors: vec!["George Eliot".into()],
        isbn_13: Some("9780141439549".into()),
        ..Default::default()
    }
}

async fn book_id(s: &Storage) -> i64 {
    sqlx::query_scalar("SELECT id FROM books WHERE title = 'Middlemarch'")
        .fetch_one(s.pool())
        .await
        .unwrap()
}

/// `n` devices that all hold *Middlemarch*.
async fn sharing_a_book(n: usize) -> Net {
    set_up("book", n, |n| async move {
        let net = Net::new(n).await;
        net.s(0).upsert_book(&middlemarch(), None).await.unwrap();
        net
    })
    .await
}

/// Opens a reading of *Middlemarch* begun at `began` and made, and last touched, at `made`.
async fn open_reading(s: &Storage, began: i64, made: i64) -> i64 {
    open_reading_of(s, book_id(s).await, began, made).await
}

async fn open_reading_of(s: &Storage, book: i64, began: i64, made: i64) -> i64 {
    let id = s.open_reading(book, Some(began), "manual").await.unwrap();
    sqlx::query("UPDATE readings SET created_at = ?1, last_modified = ?1 WHERE id = ?2")
        .bind(made)
        .bind(id)
        .execute(s.pool())
        .await
        .unwrap();
    id
}

async fn reading_id(s: &Storage, began: i64) -> i64 {
    sqlx::query_scalar("SELECT id FROM readings WHERE started_at = ?")
        .bind(began)
        .fetch_one(s.pool())
        .await
        .unwrap()
}

fn rows<'v>(seen: &'v [String], prefix: &str) -> Vec<&'v str> {
    seen.iter()
        .filter(|r| r.starts_with(prefix))
        .map(String::as_str)
        .collect()
}

fn open_readings(seen: &[String]) -> Vec<&str> {
    seen.iter()
        .filter(|r| r.starts_with("reading ") && r.contains(" finished - "))
        .map(String::as_str)
        .collect()
}

fn reading_begun(seen: &[String], began: i64) -> &str {
    let prefix = format!("reading Middlemarch began {began} ");
    rows(seen, &prefix)
        .first()
        .copied()
        .unwrap_or_else(|| panic!("no reading begun {began}: {seen:#?}"))
}

/// Device `i` of `n` opens a reading of *Middlemarch* begun at `100 * (i + 1)` and made at
/// `1000 * (n - i)`: device 0's began first and was made last.
async fn open_readings_everywhere(n: usize) -> Net {
    let mut net = sharing_a_book(n).await;
    for d in 0..n {
        open_reading(net.s(d), 100 * (d as i64 + 1), 1000 * (n - d) as i64).await;
    }
    net.publish_all().await;
    net
}

/// The reading made last stays open; every other is put down where it was last touched.
fn the_reading_made_last_stays_open(seen: &[String], n: usize) {
    assert_eq!(open_readings(seen), [reading_begun(seen, 100)], "{seen:#?}");
    for d in 1..n as i64 {
        let made = 1000 * (n as i64 - d);
        let row = reading_begun(seen, 100 * (d + 1));
        assert!(
            row.ends_with(&format!(
                " abandoned finished {made} put down {made} made {made}"
            )),
            "{row}"
        );
    }
}

/// `n` devices sharing *Middlemarch* and one reading of it begun at 100. Device `i` writes
/// `review-i.md`, made at `1000 + i`, and `reflection-i.md`, made at `2000 - i`.
async fn reviews_everywhere(n: usize) -> Net {
    notes_everywhere(n, &["review", "reflection"]).await
}

async fn notes_everywhere(n: usize, kinds: &[&str]) -> Net {
    notes_made_everywhere(
        n,
        kinds,
        |kind, d| {
            if kind == "review" { 1000 + d } else { 2000 - d }
        },
    )
    .await
}

/// As [`notes_everywhere`], each note made at `made(kind, device)`.
async fn notes_made_everywhere(n: usize, kinds: &[&str], made: fn(&str, i64) -> i64) -> Net {
    let mut net = set_up("reading", n, |n| async move {
        let net = sharing_a_book(n).await;
        open_reading(net.s(0), 100, 100).await;
        net
    })
    .await;
    for d in 0..n {
        let s = net.s(d);
        let (book, reading) = (book_id(s).await, reading_id(s, 100).await);
        for &kind in kinds {
            let made = made(kind, d as i64);
            let path = format!("Middlemarch/{kind}-{d}.md");
            let note = s
                .insert_note(
                    NewNoteMeta {
                        book_id: Some(book),
                        reading_id: Some(reading),
                        highlight_id: None,
                        page: None,
                        location: None,
                        file_path: &path,
                        title: kind,
                        kind,
                    },
                    "A fine book.",
                    &[],
                )
                .await
                .unwrap();
            sqlx::query("UPDATE notes SET created_at = ? WHERE id = ?")
                .bind(made)
                .bind(note)
                .execute(s.pool())
                .await
                .unwrap();
        }
    }
    net.publish_all().await;
    net
}

/// The first review made and the first reflection made keep their kind; the others are notes.
fn the_first_review_and_reflection_stand(seen: &[String], n: usize) {
    let notes = rows(seen, "note ");
    assert!(
        !notes.is_empty() && notes.len().is_multiple_of(n),
        "{seen:#?}"
    );
    for note in notes {
        let path = note.split(' ').nth(1).unwrap();
        let kind = note.split(' ').nth(2).unwrap();
        let stands = path == "Middlemarch/review-0.md"
            || path == format!("Middlemarch/reflection-{}.md", n - 1);
        let written = path
            .trim_start_matches("Middlemarch/")
            .split('-')
            .next()
            .unwrap();
        assert_eq!(kind, if stands { written } else { "note" }, "{seen:#?}");
    }
}

/// Device `i` of `n` makes the scale `own-i`, made at `1000 + i`, which becomes its default.
async fn new_scales_everywhere(n: usize) -> Net {
    new_scales_made_everywhere(n, |d| 1000 + d).await
}

/// As [`new_scales_everywhere`], device `i`'s scale made at `made(i)`.
async fn new_scales_made_everywhere(n: usize, made: fn(i64) -> i64) -> Net {
    let mut net = set_up("bare", n, Net::new).await;
    for d in 0..n {
        let name = format!("own-{d}");
        let s = net.s(d);
        s.put_rating_scale(&name, 0.0, 10.0, 1.0).await.unwrap();
        sqlx::query("UPDATE rating_scales SET created_at = ? WHERE name = ?")
            .bind(made(d as i64))
            .bind(&name)
            .execute(s.pool())
            .await
            .unwrap();
    }
    net.publish_all().await;
    net
}

fn the_scale_made_last_is_the_default(seen: &[String], n: usize) {
    assert_eq!(
        rows(seen, "scale ")
            .into_iter()
            .filter(|r| r.ends_with(" 1"))
            .collect::<Vec<_>>(),
        [format!("scale own-{} 1", n - 1)],
        "{seen:#?}"
    );
}

/// Runs `scenario` on two devices once for every point at which one device's changes can be cut
/// into two pages, each direction, and once for every cut of what the losing device sends in
/// answer, checking that only one device answers. A device writes nothing until it settles after
/// the last page.
async fn at_every_cut<F, Fut>(scenario: F, check: fn(&[String], usize))
where
    F: Fn(usize) -> Fut,
    Fut: Future<Output = Net>,
{
    cut_everywhere(scenario, check, true).await;
}

/// [`at_every_cut`], where both devices may answer: each answer is cut in turn.
async fn cut_everywhere<F, Fut>(scenario: F, check: fn(&[String], usize), one_answers: bool)
where
    F: Fn(usize) -> Fut,
    Fut: Future<Output = Net>,
{
    for (from, to) in [(0, 1), (1, 0)] {
        let n = scenario(2).await.pending(from, to);
        assert!(n > 0);
        for cut in 0..=n {
            let mut net = scenario(2).await;
            net.deliver(from, to, cut).await;
            assert_eq!(net.decisions(to).await, 0, "a page alone decided");
            net.pull(from).await;
            net.pull(to).await;
            check(&net.converge_or_quiet(cut == 0).await, 2);
        }
    }
    let mut answered = false;
    for cut in 0.. {
        let mut net = scenario(2).await;
        net.deliver_all(0, 1).await;
        net.deliver_all(1, 0).await;
        net.settle(0).await;
        net.settle(1).await;
        net.publish_all().await;
        let n = [net.pending(0, 1), net.pending(1, 0)];
        let decided = [
            net.undelivered_decisions(0, 1),
            net.undelivered_decisions(1, 0),
        ];
        assert!(
            !one_answers || decided[0] == 0 || decided[1] == 0,
            "both devices answered"
        );
        answered |= decided.iter().any(|&d| d > 0);
        for a in [0, 1] {
            if n[a] > 0 {
                net.deliver(a, 1 - a, cut.min(n[a])).await;
                assert_eq!(net.decisions(1 - a).await, 0, "a page alone decided");
            }
        }
        check(&net.converge_or_quiet(cut == 0).await, 2);
        if cut >= n[0].max(n[1]) {
            break;
        }
    }
    assert!(answered, "nobody answered");
}

/// Runs `scenario` on three devices with each delivery of its first changes going first and
/// the rest whole after it, each device settling once it has everything, and with everything
/// delivered in random orders and pages, some pages twice. Every random order ends by receiving
/// everything again.
async fn in_any_order<F, Fut>(scenario: F, check: fn(&[String], usize))
where
    F: Fn(usize) -> Fut,
    Fut: Future<Output = Net>,
{
    let pairs = [(0, 1), (0, 2), (1, 0), (1, 2), (2, 0), (2, 1)];
    for first in 0..pairs.len() {
        let mut net = scenario(3).await;
        for i in first..first + pairs.len() {
            let (from, to) = pairs[i % pairs.len()];
            net.deliver_all(from, to).await;
            if net.caught_up(to) {
                net.settle(to).await;
            }
            net.publish_all().await;
        }
        check(&net.converge_or_quiet(first == 0).await, 3);
    }
    for seed in 0..random_orders() {
        let mut net = scenario(3).await;
        net.shuffle(&mut ChaCha8Rng::seed_from_u64(seed)).await;
        check(&net.quiet().await, 3);
    }
}

/// How many changes `from` has sent that `to` has not received, and for each cut of them whether
/// to settle between the two pages: never, and also always where one of `from`'s sends ends.
fn cuts(net: Net, from: usize, to: usize) -> (usize, impl Fn(usize) -> Vec<bool>) {
    let n = net.pending(from, to);
    let ends: Vec<bool> = (0..=n)
        .map(|cut| net.between_sends(from, to, cut))
        .collect();
    (n, move |cut| {
        let mut settle = vec![false];
        if ends[cut] {
            settle.push(true);
        }
        settle
    })
}

#[tokio::test]
async fn of_two_open_readings_the_one_made_first_is_put_down_by_the_device_that_holds_it() {
    let mut net = open_readings_everywhere(2).await;
    net.deliver_all(1, 0).await;
    let got = net.settle(0).await;
    assert_eq!(got.applied.held.len(), 1, "{:?}", got.applied.held);
    assert_eq!(net.publish(0).await, 0, "the newer reading's device wrote");
    net.deliver_all(0, 1).await;
    let got = net.settle(1).await;
    assert!(got.applied.held.is_empty(), "{:?}", got.applied.held);
    assert!(
        net.publish(1).await > 0,
        "the older reading's device did not write"
    );
    the_reading_made_last_stays_open(&net.quiet().await, 2);
}

#[tokio::test]
async fn open_readings_settle_whichever_part_of_either_device_arrives_first() {
    at_every_cut(open_readings_everywhere, the_reading_made_last_stays_open).await;
}

#[tokio::test]
async fn open_readings_on_three_devices_settle_in_any_order() {
    in_any_order(open_readings_everywhere, the_reading_made_last_stays_open).await;
}

/// Device 1 records a reading it has already finished, made after device 0's open one.
async fn a_finished_reading_and_an_open_one() -> Net {
    let mut net = sharing_a_book(2).await;
    open_reading(net.s(0), 100, 1000).await;
    let s = net.s(1);
    let id = s
        .record_reading(book_id(s).await, Some(200), Some(300), "finished", "manual")
        .await
        .unwrap();
    sqlx::query("UPDATE readings SET created_at = 2000, last_modified = 2000 WHERE id = ?")
        .bind(id)
        .execute(s.pool())
        .await
        .unwrap();
    net.publish_all().await;
    net
}

/// Settled after any page, which a pull never does, a rule still never decides on half an edit.
#[tokio::test]
async fn a_reading_that_arrives_finished_never_puts_an_open_one_down() {
    let n = a_finished_reading_and_an_open_one().await.pending(1, 0);
    for cut in 0..=n {
        let mut net = a_finished_reading_and_an_open_one().await;
        net.deliver(1, 0, cut).await;
        net.settle(0).await;
        assert_eq!(net.publish(0).await, 0, "cut {cut}");
        let seen = net.converge_or_quiet(cut == 0).await;
        assert_eq!(open_readings(&seen), [reading_begun(&seen, 100)]);
    }
}

#[tokio::test]
async fn the_later_review_or_reflection_becomes_a_note_on_the_device_that_holds_it() {
    let mut net = reviews_everywhere(2).await;
    net.pull(0).await;
    net.pull(1).await;
    // Device 1 made the later review, device 0 the later reflection.
    assert!(net.publish(0).await > 0);
    assert!(net.publish(1).await > 0);
    the_first_review_and_reflection_stand(&net.quiet().await, 2);
}

#[tokio::test]
async fn reviews_settle_whichever_part_of_either_device_arrives_first() {
    at_every_cut(
        |n| notes_everywhere(n, &["review"]),
        the_first_review_and_reflection_stand,
    )
    .await;
}

#[tokio::test]
async fn reflections_settle_whichever_part_of_either_device_arrives_first() {
    at_every_cut(
        |n| notes_everywhere(n, &["reflection"]),
        the_first_review_and_reflection_stand,
    )
    .await;
}

#[tokio::test]
async fn reviews_and_reflections_on_three_devices_settle_in_any_order() {
    in_any_order(reviews_everywhere, the_first_review_and_reflection_stand).await;
}

#[tokio::test]
async fn of_two_new_default_scales_the_older_stops_being_the_default() {
    let mut net = new_scales_everywhere(2).await;
    net.pull(0).await;
    assert!(net.publish(0).await > 0);
    net.pull(1).await;
    assert_eq!(net.publish(1).await, 0);
    the_scale_made_last_is_the_default(&net.quiet().await, 2);
}

#[tokio::test]
async fn default_scales_settle_whichever_part_of_either_device_arrives_first() {
    at_every_cut(new_scales_everywhere, the_scale_made_last_is_the_default).await;
}

#[tokio::test]
async fn default_scales_on_three_devices_settle_in_any_order() {
    in_any_order(new_scales_everywhere, the_scale_made_last_is_the_default).await;
}

/// The seeded `default` scale is made with each library, so a device set up today has one newer
/// than a scale a reader made last year on another. Which device's seeded flag the merge keeps
/// is arbitrary when both were there before sync was.
#[tokio::test]
async fn a_scale_a_reader_made_stays_the_default_over_a_newer_device_s_seeded_one() {
    for _ in 0..6 {
        let mut net = Net::new(1).await;
        let older = Storage::open("sqlite::memory:").await.unwrap();
        older
            .put_rating_scale("tenths", 0.0, 10.0, 1.0)
            .await
            .unwrap();
        sqlx::query("UPDATE rating_scales SET created_at = 1000 WHERE name = 'tenths'")
            .execute(older.pool())
            .await
            .unwrap();
        net.join(older).await;
        net.got = vec![vec![0; 2]; 2];
        net.setup = vec![0; 2];
        let seen = net.quiet().await;
        assert_eq!(
            rows(&seen, "scale ")
                .into_iter()
                .filter(|r| r.ends_with(" 1"))
                .collect::<Vec<_>>(),
            ["scale tenths 1"],
            "{seen:#?}"
        );
    }
}

/// Device 1 puts its reading, made first, down on pulling device 0's, and before sending that
/// the reader there finishes device 0's reading and opens its own again.
async fn the_reader_opens_the_put_down_reading_again() -> Net {
    let mut net = open_readings_everywhere(2).await;
    net.pull(1).await;
    assert!(net.publish(1).await > 0);
    let s = net.s(1);
    s.finish_reading(book_id(s).await).await.unwrap();
    sqlx::query(
        "UPDATE readings SET finished_at = NULL, status = 'reading', put_down_at = NULL
          WHERE started_at = 200",
    )
    .execute(s.pool())
    .await
    .unwrap();
    net.publish(1).await;
    net
}

fn the_reading_opened_again_stays_open(seen: &[String]) {
    assert_eq!(open_readings(seen), [reading_begun(seen, 200)], "{seen:#?}");
    assert!(
        reading_begun(seen, 100).contains(" finished finished "),
        "{seen:#?}"
    );
}

#[tokio::test]
async fn a_reading_the_reader_opens_again_after_it_was_put_down_stays_open() {
    let (n, ends) = cuts(the_reader_opens_the_put_down_reading_again().await, 1, 0);
    for cut in 0..=n {
        for between_sends in ends(cut) {
            let mut net = the_reader_opens_the_put_down_reading_again().await;
            net.pull_cut(1, 0, cut, between_sends).await;
            the_reading_opened_again_stays_open(&net.converge_or_quiet(cut == 0).await);
        }

        // The reader's edit first, then the decision it overrode.
        let mut net = the_reader_opens_the_put_down_reading_again().await;
        let start = net.got[0][1];
        net.redeliver(1, 0, start + cut, start + n).await;
        net.redeliver(1, 0, start, start + cut).await;
        net.got[0][1] = start + n;
        net.settle(0).await;
        the_reading_opened_again_stays_open(&net.converge().await);
    }
}

/// Device 1 makes its later review a note on pulling device 0's, and before sending that the
/// reader there makes device 0's a note and its own a review again.
async fn the_reader_makes_the_later_review_a_review_again() -> Net {
    let mut net = notes_everywhere(2, &["review"]).await;
    net.pull(1).await;
    assert!(net.publish(1).await > 0);
    let s = net.s(1);
    for (kind, path) in [("note", "review-0"), ("review", "review-1")] {
        sqlx::query("UPDATE notes SET kind = ? WHERE file_path = ?")
            .bind(kind)
            .bind(format!("Middlemarch/{path}.md"))
            .execute(s.pool())
            .await
            .unwrap();
    }
    net.publish(1).await;
    net
}

fn the_review_made_again_stands(seen: &[String]) {
    let notes = rows(seen, "note ");
    assert_eq!(notes.len(), 2, "{seen:#?}");
    assert!(
        notes[0].starts_with("note Middlemarch/review-0.md note "),
        "{seen:#?}"
    );
    assert!(
        notes[1].starts_with("note Middlemarch/review-1.md review "),
        "{seen:#?}"
    );
}

#[tokio::test]
async fn a_note_the_reader_makes_a_review_again_after_it_was_made_a_note_stays_a_review() {
    let (n, ends) = cuts(
        the_reader_makes_the_later_review_a_review_again().await,
        1,
        0,
    );
    for cut in 0..=n {
        for between_sends in ends(cut) {
            let mut net = the_reader_makes_the_later_review_a_review_again().await;
            net.pull_cut(1, 0, cut, between_sends).await;
            the_review_made_again_stands(&net.converge_or_quiet(cut == 0).await);
        }

        let mut net = the_reader_makes_the_later_review_a_review_again().await;
        let start = net.got[0][1];
        net.redeliver(1, 0, start + cut, start + n).await;
        net.redeliver(1, 0, start, start + cut).await;
        net.got[0][1] = start + n;
        net.settle(0).await;
        the_review_made_again_stands(&net.converge().await);
    }
}

/// Settled after a page that ends before the reading's last required field, which a pull never
/// does, the reading stays held and reported and nothing is written.
#[tokio::test]
async fn a_row_held_for_another_reason_is_left_alone_and_still_reported() {
    let mut net = sharing_a_book(2).await;
    open_reading(net.s(0), 100, 1000).await;
    net.publish(0).await;
    let start = net.got[1][0];
    let required = net.devices[0].sent[start..]
        .iter()
        .position(|c| c.collection == "readings" && c.field == "last_modified")
        .unwrap();
    net.deliver(0, 1, required).await;
    let got = net.settle(1).await;
    assert_eq!(got.applied.held.len(), 1, "{:?}", got.applied.held);
    assert_eq!(got.applied.held[0].collection, "readings");
    assert_eq!(net.publish(1).await, 0);
    let seen = net.quiet().await;
    assert_eq!(open_readings(&seen), [reading_begun(&seen, 100)]);
}

/// Device 1 opens a reading made after device 0's, sends it, then finishes it and sends that.
async fn a_reading_finished_after_it_was_sent() -> Net {
    let mut net = sharing_a_book(2).await;
    open_reading(net.s(0), 100, 1000).await;
    net.publish(0).await;
    open_reading(net.s(1), 200, 2000).await;
    net.publish(1).await;
    let s = net.s(1);
    s.finish_reading(book_id(s).await).await.unwrap();
    net.publish(1).await;
    net
}

#[tokio::test]
async fn a_reading_finished_in_a_later_send_puts_nothing_down_when_both_sends_are_pulled() {
    let n = a_reading_finished_after_it_was_sent().await.pending(1, 0);
    for cut in 0..=n {
        let mut net = a_reading_finished_after_it_was_sent().await;
        net.pull_cut(1, 0, cut, false).await;
        assert_eq!(net.publish(0).await, 0, "cut {cut}");
        let seen = net.converge_or_quiet(cut == 0).await;
        assert_eq!(open_readings(&seen), [reading_begun(&seen, 100)]);
        assert!(reading_begun(&seen, 200).contains(" finished finished "));
    }
}

/// Pulled between the two sends, the clash was real when device 0 settled: it puts its older
/// reading down, and when the finish arrives neither reading is open.
#[tokio::test]
async fn a_reading_finished_in_a_later_send_pulled_between_the_sends_leaves_none_open() {
    let mut net = a_reading_finished_after_it_was_sent().await;
    let first = net.devices[1].sends[net.devices[1].sends.len() - 2] - net.got[0][1];
    net.pull_cut(1, 0, first, true).await;
    let seen = net.quiet().await;
    assert!(open_readings(&seen).is_empty(), "{seen:#?}");
    assert!(
        reading_begun(&seen, 100).contains(" abandoned finished 1000 put down 1000 "),
        "{seen:#?}"
    );
    assert!(reading_begun(&seen, 200).contains(" finished finished "));
}

/// Device 0 pulls device 1's reading, then device 1's reader moves its start before device 0's
/// and device 1 pulls device 0's. Had the order been by start, device 0 would put its reading
/// down on the start it had and device 1 its own on the moved one; by when each was made, which
/// no one moves, only device 1's goes.
#[tokio::test]
async fn readings_whose_start_moves_while_they_cross_end_with_one_open() {
    async fn scenario() -> Net {
        let mut net = open_readings_everywhere(2).await;
        net.pull(0).await;
        net.publish(0).await;
        sqlx::query("UPDATE readings SET started_at = 50 WHERE started_at = 200")
            .execute(net.s(1).pool())
            .await
            .unwrap();
        net.publish(1).await;
        net
    }
    let (n, ends) = cuts(scenario().await, 0, 1);
    for cut in 0..=n {
        for between_sends in ends(cut) {
            let mut net = scenario().await;
            net.pull_cut(0, 1, cut, between_sends).await;
            let seen = net.converge_or_quiet(cut == 0).await;
            assert_eq!(
                open_readings(&seen),
                [reading_begun(&seen, 100)],
                "cut {cut}"
            );
        }
    }
}

/// A third device that has device 0's reading and then device 1's, made first, puts device 1's
/// down itself. When device 1 has already done so, and its reader has since opened it again
/// without yet sending, the third device's decision is the later write and undoes the reader's.
#[tokio::test]
#[ignore = "a device that only received the losing row also gives it up; only the device that made it should, and the kernel does not say which that is"]
async fn a_third_device_deciding_late_leaves_a_later_edit_alone() {
    let mut net = sharing_a_book(3).await;
    open_reading(net.s(0), 100, 2000).await;
    open_reading(net.s(1), 200, 1000).await;
    net.publish_all().await;
    net.deliver_all(0, 1).await;
    net.settle(1).await;
    let s = net.s(1);
    s.finish_reading(book_id(s).await).await.unwrap();
    sqlx::query(
        "UPDATE readings SET finished_at = NULL, status = 'reading', put_down_at = NULL
          WHERE started_at = 200",
    )
    .execute(s.pool())
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    net.deliver_all(1, 2).await;
    net.pull(2).await;
    the_reading_opened_again_stays_open(&net.quiet().await);
}

/// A device sends a row's fields in registry order, so a rule that waits for one field can read
/// the fields declared before it even when it settles on part of a send.
#[tokio::test]
async fn each_clash_rule_reads_only_fields_sent_before_the_one_it_waits_for() {
    let registry = super::registry().unwrap();
    for (collection, waits_for, reads) in [
        ("readings", "created_at", &["book_id", "finished_at"][..]),
        ("notes", "created_at", &["reading_id", "kind"][..]),
        ("rating_scales", "is_default", &["name", "created_at"][..]),
    ] {
        let c = registry
            .collections()
            .iter()
            .find(|c| c.name == collection)
            .unwrap();
        let at = |f: &str| c.fields.iter().position(|x| x.name == f).unwrap();
        for read in reads {
            assert!(at(read) < at(waits_for), "{collection}.{read}");
        }
    }
}

/// Device 0 holds *Middlemarch* as one row with both ISBNs, and when `reading_here` an open
/// reading begun at 100 and made at 3000. Device 1 holds it as a row for each ISBN, each with an
/// open reading: begun at 200 and made at 1000, begun at 300 and made at 2000. Any other device
/// holds no book.
async fn book_rows_to_become_one(n: usize, reading_here: bool) -> Net {
    let mut net = set_up("bare", n, Net::new).await;
    let book = |isbn_10: Option<&str>, isbn_13: Option<&str>| crate::book::Book {
        title: Some("Middlemarch".into()),
        isbn_10: isbn_10.map(Into::into),
        isbn_13: isbn_13.map(Into::into),
        ..Default::default()
    };
    let (isbn_10, isbn_13) = (Some("0141439548"), Some("9780141439549"));
    let one = net
        .s(0)
        .upsert_book(&book(isbn_10, isbn_13), None)
        .await
        .unwrap();
    if reading_here {
        open_reading_of(net.s(0), one, 100, 3000).await;
    }
    for (i, row) in [book(isbn_10, None), book(None, isbn_13)]
        .iter()
        .enumerate()
    {
        let s = net.s(1);
        let id = s.upsert_book(row, None).await.unwrap();
        open_reading_of(s, id, 200 + 100 * i as i64, 1000 + 1000 * i as i64).await;
    }
    net.publish_all().await;
    net
}

/// One book row is left, and of the readings only the one made last is open.
fn one_book_and_the_reading_made_last_open(seen: &[String], made_last: i64, readings: usize) {
    assert_eq!(
        rows(seen, "book "),
        ["book Middlemarch 0141439548 9780141439549"],
        "{seen:#?}"
    );
    let open = open_readings(seen);
    assert_eq!(open.len(), 1, "{seen:#?}");
    assert!(
        open[0].ends_with(&format!(" made {made_last}")),
        "{seen:#?}"
    );
    let all = rows(seen, "reading ");
    assert_eq!(all.len(), readings, "{seen:#?}");
    for row in all.into_iter().filter(|r| !open.contains(r)) {
        assert!(row.contains(" abandoned finished "), "{seen:#?}");
    }
}

fn with_a_reading_here(seen: &[String], _: usize) {
    one_book_and_the_reading_made_last_open(seen, 3000, 3);
}

fn with_no_reading_here(seen: &[String], _: usize) {
    one_book_and_the_reading_made_last_open(seen, 2000, 2);
}

#[tokio::test]
async fn book_rows_that_become_one_keep_only_the_reading_made_last_open() {
    at_every_cut(|n| book_rows_to_become_one(n, true), with_a_reading_here).await;
}

/// The device with one row receives both readings at once and puts the older down itself, as
/// the device that holds both rows does.
#[tokio::test]
async fn book_rows_that_become_one_with_no_reading_elsewhere_keep_the_later_open() {
    cut_everywhere(
        |n| book_rows_to_become_one(n, false),
        with_no_reading_here,
        false,
    )
    .await;
}

#[tokio::test]
async fn book_rows_that_become_one_settle_on_three_devices_in_any_order() {
    in_any_order(|n| book_rows_to_become_one(n, true), with_a_reading_here).await;
    in_any_order(|n| book_rows_to_become_one(n, false), with_no_reading_here).await;
}

/// Which row of the two the kernel keeps, and so which way the readings move, depends on random
/// ids, so the plain exchange runs several more times than the cuts above.
#[tokio::test]
async fn book_rows_that_become_one_settle_whichever_row_is_kept() {
    for _ in 0..8 {
        for (reading_here, check) in [
            (true, with_a_reading_here as fn(&[String], usize)),
            (false, with_no_reading_here),
        ] {
            let mut net = book_rows_to_become_one(2, reading_here).await;
            check(&net.converge().await, 2);
        }
    }
}

fn exactly_one(seen: &[String], prefix: &str, standing: &str, n: usize) {
    let all = rows(seen, prefix);
    assert_eq!(all.len(), n, "{seen:#?}");
    let stand: Vec<_> = all.iter().filter(|r| r.contains(standing)).collect();
    assert_eq!(stand.len(), 1, "{seen:#?}");
}

/// Made in the same second on every device, the readings are ordered by their ids alone.
#[tokio::test]
async fn open_readings_made_in_the_same_second_settle_by_id() {
    async fn scenario(n: usize) -> Net {
        let mut net = sharing_a_book(n).await;
        for d in 0..n {
            open_reading(net.s(d), 100 * (d as i64 + 1), 1000).await;
        }
        net.publish_all().await;
        net
    }
    fn check(seen: &[String], n: usize) {
        exactly_one(seen, "reading ", " finished - ", n);
        for row in rows(seen, "reading ") {
            assert!(
                row.contains(" finished - ") || row.contains(" abandoned finished 1000 "),
                "{seen:#?}"
            );
        }
    }
    at_every_cut(scenario, check).await;
}

#[tokio::test]
async fn reviews_made_in_the_same_second_settle_by_id() {
    fn check(seen: &[String], n: usize) {
        exactly_one(seen, "note ", " review ", n);
    }
    at_every_cut(
        |n| notes_made_everywhere(n, &["review"], |_, _| 1000),
        check,
    )
    .await;
}

#[tokio::test]
async fn default_scales_made_in_the_same_second_settle_by_id() {
    fn check(seen: &[String], n: usize) {
        let own: Vec<_> = rows(seen, "scale own-");
        assert_eq!(own.len(), n, "{seen:#?}");
        assert_eq!(
            own.iter().filter(|r| r.ends_with(" 1")).count(),
            1,
            "{seen:#?}"
        );
        assert!(rows(seen, "scale default 0").len() == 1, "{seen:#?}");
    }
    at_every_cut(|n| new_scales_made_everywhere(n, |_| 1000), check).await;
}

/// A device that puts its own default down on receiving two newer ones lets one in, and then
/// decides between that one and the other in the same settle where it can. What it cannot decide
/// waits for the device that holds it, and is reported held.
#[tokio::test]
async fn one_settle_decides_every_clash_it_can() {
    for _ in 0..10 {
        let mut net = new_scales_everywhere(3).await;
        let got = net.pull(0).await;
        let device = &net.devices[0];
        let mut conn = device.storage.pool().acquire().await.unwrap();
        let here: i64 =
            sqlx::query_scalar("SELECT created_at FROM rating_scales WHERE is_default = 1")
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        for h in &got.applied.held {
            let facts = device
                .replica
                .merged(&mut conn, &h.collection, h.gid)
                .await
                .unwrap();
            let made = facts.get("created_at");
            assert!(
                matches!(made, Some(fongo_sync_sqlite::core::Value::Int(m)) if *m < here),
                "a newer default {made:?} than this device's ({here}) is still held"
            );
        }
        drop(conn);
        the_scale_made_last_is_the_default(&net.quiet().await, 3);
    }
}

/// Two entities that become one by their identity leave one row, so they never clash with each
/// other: two reviews of one reading at one path, two default scales of one name.
#[tokio::test]
async fn rows_that_become_one_by_identity_do_not_clash() {
    for _ in 0..4 {
        let mut net = notes_made_everywhere(2, &[], |_, _| 0).await;
        for d in 0..2 {
            let s = net.s(d);
            let reading = reading_id(s, 100).await;
            let note = s
                .insert_note(
                    NewNoteMeta {
                        book_id: Some(book_id(s).await),
                        reading_id: Some(reading),
                        highlight_id: None,
                        page: None,
                        location: None,
                        file_path: "Middlemarch/review.md",
                        title: "review",
                        kind: "review",
                    },
                    "A fine book.",
                    &[],
                )
                .await
                .unwrap();
            sqlx::query("UPDATE notes SET created_at = ? WHERE id = ?")
                .bind(1000 + d as i64)
                .bind(note)
                .execute(s.pool())
                .await
                .unwrap();
        }
        let seen = net.quiet().await;
        assert_eq!(
            rows(&seen, "note "),
            ["note Middlemarch/review.md review on the reading begun 100"],
            "{seen:#?}"
        );

        let mut net = new_scales_made_everywhere(2, |d| 1000 + d).await;
        for d in 0..2 {
            net.s(d)
                .put_rating_scale("tenths", 0.0, 10.0, 1.0)
                .await
                .unwrap();
        }
        let seen = net.quiet().await;
        let defaults: Vec<_> = rows(&seen, "scale ")
            .into_iter()
            .filter(|r| r.ends_with(" 1"))
            .collect();
        assert_eq!(defaults, ["scale tenths 1"], "{seen:#?}");
    }
}
