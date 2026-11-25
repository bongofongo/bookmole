//! `bookmole sync` as two processes of the binary, each with its own data directory, syncing
//! through the engine's in-memory sync server served on a loopback port.

mod sandbox;

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use bookmole::test_server::TestServer;
use bookmole::{Book, Engine, EngineConfig};

struct Device {
    root: tempfile::TempDir,
}

struct Out {
    ok: bool,
    stdout: String,
    stderr: String,
}

impl Out {
    fn text(&self) -> String {
        format!(
            "--- stdout ---\n{}\n--- stderr ---\n{}",
            self.stdout, self.stderr
        )
    }
}

const PROXIES: &[&str] = &[
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
];

impl Device {
    fn new() -> Device {
        Device {
            root: tempfile::tempdir().unwrap(),
        }
    }

    fn data_dir(&self) -> PathBuf {
        self.root.path().join("data")
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = sandbox::command(self.root.path());
        for proxy in PROXIES {
            cmd.env_remove(proxy);
        }
        cmd.arg("--data-dir").arg(self.data_dir()).args(args);
        cmd
    }

    fn try_run(&self, args: &[&str]) -> Out {
        out(self.command(args).output().unwrap())
    }

    fn run(&self, args: &[&str]) -> Out {
        let out = self.try_run(args);
        assert!(
            out.ok,
            "`bookmole {}` failed\n{}",
            args.join(" "),
            out.text()
        );
        out
    }

    fn try_run_with_stdin(&self, args: &[&str], stdin: &str) -> Out {
        let mut child = self
            .command(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        out(child.wait_with_output().unwrap())
    }

    /// Opens the library in this process, for what the CLI cannot do offline or does not show.
    fn engine<T>(&self, f: impl AsyncFnOnce(&Engine) -> T) -> T {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let engine = Engine::open(EngineConfig::rooted_at(self.data_dir()))
                .await
                .unwrap();
            f(&engine).await
        })
    }

    fn add(&self, title: &str, isbn_13: &str) {
        self.engine(async |e| {
            e.save_book(&Book {
                title: Some(title.into()),
                authors: vec!["George Eliot".into()],
                isbn_13: Some(isbn_13.into()),
                ..Default::default()
            })
            .await
            .unwrap();
        });
    }

    fn titles(&self) -> Vec<String> {
        self.engine(async |e| {
            let mut titles: Vec<String> = e
                .resolve_books("")
                .await
                .unwrap()
                .into_iter()
                .filter_map(|b| b.title)
                .collect();
            titles.sort();
            titles
        })
    }

    fn page_count(&self, title: &str) -> Option<i64> {
        self.engine(async |e| {
            let found = e.resolve_books(title).await.unwrap();
            assert_eq!(found.len(), 1, "{title}");
            found[0].page_count
        })
    }
}

fn out(o: std::process::Output) -> Out {
    Out {
        ok: o.status.success(),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

/// The one line `create` and `recovery-code` print, checked to be a code and nothing else.
fn the_code(out: &Out) -> String {
    let code = out.stdout.strip_suffix('\n').unwrap_or(&out.stdout);
    assert!(
        !code.contains('\n'),
        "more than the code on stdout\n{}",
        out.text()
    );
    assert!(
        code.len() > 80
            && code
                .chars()
                .all(|c| c == '-' || c.is_ascii_digit() || c.is_ascii_uppercase()),
        "not a recovery code on stdout\n{}",
        out.text()
    );
    code.to_owned()
}

fn hides(out: &Out, code: &str) {
    for group in code.split('-') {
        assert!(
            !out.stdout.contains(group) && !out.stderr.contains(group),
            "the code is shown\n{}",
            out.text()
        );
    }
}

fn sync_all(devices: &[&Device]) {
    for _ in 0..2 {
        for d in devices {
            d.run(&["sync", "now"]);
        }
    }
}

#[test]
fn two_processes_sync_a_library_and_one_leaves_and_joins_again() {
    let server = TestServer::default();
    let http = server.serve().unwrap();
    let url = http.url();
    let (a, b) = (Device::new(), Device::new());
    a.add("Middlemarch", "9780141439549");
    a.add("Romola", "9780140434705");
    b.add("Silas Marner", "9780141439754");

    let off = a.run(&["sync", "status"]);
    assert!(off.stdout.contains("sync is off"), "{}", off.text());
    assert!(
        off.stdout.contains("bookmole sync create"),
        "{}",
        off.text()
    );

    let created = a.run(&["sync", "create", &url, "--label", "Study"]);
    let code = the_code(&created);
    assert!(
        created.stderr.contains("recovery code"),
        "{}",
        created.text()
    );
    let shown = a.run(&["sync", "recovery-code"]);
    assert_eq!(the_code(&shown), code);
    assert!(
        shown.stderr.contains("keep it somewhere safe"),
        "{}",
        shown.text()
    );
    let status = a.run(&["sync", "status"]);
    assert!(
        status
            .stdout
            .contains(&format!("syncs with {url} as \"Study\"")),
        "{}",
        status.text()
    );
    hides(&status, &code);

    let joined = b.try_run_with_stdin(
        &["sync", "join", &url, "--label", "Reading chair"],
        &format!("{code}\n"),
    );
    assert!(joined.ok, "{}", joined.text());
    hides(&joined, &code);

    let first = a.run(&["sync", "now"]);
    assert!(
        first.stdout.contains("sent what changed here"),
        "{}",
        first.text()
    );
    hides(&first, &code);
    sync_all(&[&b, &a]);
    for d in [&a, &b] {
        assert_eq!(d.titles(), ["Middlemarch", "Romola", "Silas Marner"]);
    }

    a.run(&["set", "Middlemarch", "--page-count", "880"]);
    b.run(&["set", "Silas Marner", "--page-count", "300"]);
    sync_all(&[&a, &b]);
    for d in [&a, &b] {
        assert_eq!(d.page_count("Middlemarch"), Some(880));
        assert_eq!(d.page_count("Silas Marner"), Some(300));
    }
    let settled = a.run(&["sync", "now"]);
    assert_eq!(
        settled.stdout,
        "nothing to send or bring in.\n",
        "{}",
        settled.text()
    );

    let asked = b.run(&["sync", "leave"]);
    assert!(asked.stdout.contains("still syncing"), "{}", asked.text());
    assert!(asked.stdout.contains("duplicates"), "{}", asked.text());
    assert!(b.run(&["sync", "status"]).stdout.contains("syncs with"));

    let requests = server.requests();
    let left = b.run(&["sync", "leave", "--yes"]);
    assert!(left.stdout.contains("no longer syncs"), "{}", left.text());
    assert!(left.stdout.contains("stays"), "{}", left.text());
    assert!(left.stderr.contains("duplicates"), "{}", left.text());
    assert!(!left.stdout.contains("join"), "{}", left.text());
    assert_eq!(server.requests(), requests);
    assert!(b.run(&["sync", "status"]).stdout.contains("sync is off"));
    let refused = b.try_run(&["sync", "now"]);
    assert!(!refused.ok);
    assert!(refused.stdout.is_empty(), "{}", refused.text());
    assert!(
        refused.stderr.contains("bookmole sync join"),
        "{}",
        refused.text()
    );
    assert_eq!(b.titles(), ["Middlemarch", "Romola", "Silas Marner"]);
    let again = b.run(&["sync", "leave"]);
    assert!(
        again.stdout.contains("nothing to leave"),
        "{}",
        again.text()
    );

    a.run(&["set", "Romola", "--page-count", "592"]);
    a.run(&["sync", "now"]);
    assert_eq!(b.page_count("Romola"), None);

    let rejoined = b.try_run_with_stdin(&["sync", "join", &url], &format!("{code}\n"));
    assert!(rejoined.ok, "{}", rejoined.text());
    assert_eq!(server.devices(), 3);
    sync_all(&[&b, &a]);
    assert_eq!(b.page_count("Romola"), Some(592));
    for d in [&a, &b] {
        assert_eq!(d.titles(), ["Middlemarch", "Romola", "Silas Marner"]);
    }
}

/// The code is read only from stdin or a prompt, and no refusal repeats it or what was typed
/// as a server address.
#[test]
fn the_recovery_code_never_goes_where_it_could_be_kept() {
    let server = TestServer::default();
    let http = server.serve().unwrap();
    let url = http.url();
    let (a, b) = (Device::new(), Device::new());

    let off = a.try_run(&["sync", "recovery-code"]);
    assert!(!off.ok);
    assert!(off.stdout.is_empty(), "{}", off.text());
    assert!(
        off.stderr.contains("bookmole sync create"),
        "{}",
        off.text()
    );

    let refused = a.try_run(&["sync", "create", "https://reader:hunter2@sync.example.test"]);
    assert!(!refused.ok);
    assert!(!refused.stderr.contains("hunter2"), "{}", refused.text());

    let quiet = a.run(&["--quiet", "sync", "create", &url]);
    let code = the_code(&quiet);
    assert!(quiet.stderr.is_empty(), "{}", quiet.text());

    assert!(!b.try_run(&["sync", "join", &url, &code]).ok);

    let nothing = b.try_run_with_stdin(&["sync", "join", &url], "");
    assert!(!nothing.ok);
    assert!(
        nothing.stderr.contains("no recovery code given"),
        "{}",
        nothing.text()
    );

    let groups: Vec<&str> = code.split('-').collect();
    let mistyped = code.replacen(groups[2], "UUUUU", 1);
    let wrong = b.try_run_with_stdin(&["sync", "join", &url], &format!("{mistyped}\n"));
    assert!(!wrong.ok);
    hides(&wrong, &mistyped);
    assert!(wrong.stderr.contains("check the code"), "{}", wrong.text());
    assert!(b.run(&["sync", "status"]).stdout.contains("sync is off"));
}

/// A sync holds the data directory's lock for as long as its process runs: a second process gives
/// way at once and changes nothing, and killing the first frees the lock.
#[test]
fn a_sync_in_another_process_holds_the_lock_until_that_process_ends() {
    let server = TestServer::default();
    let http = server.serve().unwrap();
    let url = http.url();
    let a = Device::new();
    a.add("Middlemarch", "9780141439549");
    a.run(&["sync", "create", &url]);

    let arrived = server.arrived();
    server.hang_at(Some(1));
    let mut first = a
        .command(&["sync", "now"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let began = std::time::Instant::now();
    while server.arrived() == arrived {
        assert!(
            began.elapsed().as_secs() < 60,
            "the first sync never reached the server"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    for args in [&["sync", "now"][..], &["sync", "leave", "--yes"]] {
        let refused = a.try_run(args);
        assert!(!refused.ok, "{}", refused.text());
        assert!(
            refused.stderr.contains("another Bookmole is syncing"),
            "{}",
            refused.text()
        );
    }
    assert!(a.run(&["sync", "status"]).stdout.contains("syncs with"));

    first.kill().unwrap();
    first.wait().unwrap();
    server.hang_at(None);
    let after = a.run(&["sync", "now"]);
    assert!(
        after.stdout.contains("sent what changed here"),
        "{}",
        after.text()
    );
}
