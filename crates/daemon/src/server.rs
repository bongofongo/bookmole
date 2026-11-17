//! Unix-socket transport, one JSON object per line. Never branches on `Request`,
//! and never accepts LAN peers: those use the engine's own wireless listener.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bookmole_api::{Api, ApiError, Call, Reply};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// Per-line read cap; a longer line closes the connection.
pub const MAX_LINE: usize = 8 * 1024 * 1024;

/// Binds with mode 0600, refusing a live socket at `path` and removing a stale one.
pub async fn bind(path: &Path) -> anyhow::Result<UnixListener> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    if path.exists() {
        match UnixStream::connect(path).await {
            Ok(_) => anyhow::bail!(
                "a bookmoled is already listening on {} — stop it, or pass --socket",
                path.display()
            ),
            Err(_) => {
                tracing::warn!(socket = %path.display(), "removing a stale socket");
                std::fs::remove_file(path)?;
            }
        }
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Removes the socket file on drop, including on a panic.
pub struct SocketGuard(pub PathBuf);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        std::fs::remove_file(&self.0).ok();
    }
}

pub async fn serve<S>(api: Api, listener: UnixListener, shutdown: S) -> anyhow::Result<()>
where
    S: std::future::Future<Output = ()>,
{
    let api = Arc::new(api);
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            // Biased: shutdown wins over queued connections.
            biased;
            () = &mut shutdown => {
                tracing::info!("shutting down");
                return Ok(());
            }
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _)) => {
                        let api = Arc::clone(&api);
                        tokio::spawn(async move {
                            if let Err(e) = handle(&api, stream).await {
                                tracing::warn!(error = %e, "connection ended badly");
                            }
                        });
                    }
                    // A failed accept (e.g. EMFILE) is transient; keep serving.
                    Err(e) => tracing::warn!(error = %e, "accept failed"),
                }
            }
        }
    }
}

/// Answers the calls on one connection strictly in order, one at a time.
async fn handle(api: &Api, stream: UnixStream) -> io::Result<()> {
    let (rx, mut tx) = stream.into_split();
    let mut reader = BufReader::new(rx);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match read_line_capped(&mut reader, &mut buf, MAX_LINE).await? {
            Line::Eof => return Ok(()),
            Line::TooLong => {
                let reply = Reply::err(
                    0,
                    ApiError::bad_request(format!("request exceeded {MAX_LINE} bytes")),
                );
                tx.write_all(reply.to_line().as_bytes()).await?;
                tx.flush().await?;
                // The rest of the line is still unread, so the connection cannot be resynchronised.
                return Ok(());
            }
            Line::Read => {}
        }
        let line = String::from_utf8_lossy(&buf);
        if line.trim().is_empty() {
            continue;
        }

        let reply = match serde_json::from_str::<Call>(&line) {
            Ok(call) => api.call(call).await,
            // Malformed lines get id 0 and the connection stays open.
            Err(e) => Reply::err(0, ApiError::bad_request(e.to_string())),
        };
        tx.write_all(reply.to_line().as_bytes()).await?;
        tx.flush().await?;
    }
}

enum Line {
    Read,
    /// The peer closed with nothing pending.
    Eof,
    TooLong,
}

/// `read_until(b'\n')` with a cap; a final line with no newline is still delivered.
/// Not `Take`: that caps the reader, not the line, and would split an overlong line in two.
async fn read_line_capped<R>(reader: &mut R, buf: &mut Vec<u8>, max: usize) -> io::Result<Line>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(if buf.is_empty() {
                Line::Eof
            } else {
                Line::Read
            });
        }
        match available.iter().position(|&b| b == b'\n') {
            Some(i) => {
                buf.extend_from_slice(&available[..=i]);
                reader.consume(i + 1);
                return Ok(if buf.len() > max {
                    Line::TooLong
                } else {
                    Line::Read
                });
            }
            None => {
                let n = available.len();
                buf.extend_from_slice(available);
                reader.consume(n);
                if buf.len() > max {
                    return Ok(Line::TooLong);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use bookmole::{Engine, EngineConfig};
    use bookmole_api::{Request, Response};
    use tokio::io::AsyncReadExt;

    async fn test_api(root: &Path) -> Api {
        let config = EngineConfig::in_memory(root);
        Api::new(Arc::new(Engine::open(config).await.expect("engine")))
    }

    struct Client {
        reader: BufReader<tokio::net::unix::OwnedReadHalf>,
        writer: tokio::net::unix::OwnedWriteHalf,
        next_id: u64,
    }

    impl Client {
        async fn connect(path: &Path) -> Client {
            let (rx, writer) = UnixStream::connect(path)
                .await
                .expect("connect")
                .into_split();
            Client {
                reader: BufReader::new(rx),
                writer,
                next_id: 1,
            }
        }

        async fn send_raw(&mut self, line: &str) -> String {
            self.writer.write_all(line.as_bytes()).await.unwrap();
            self.writer.flush().await.unwrap();
            let mut out = String::new();
            self.reader.read_line(&mut out).await.unwrap();
            out
        }

        async fn call(&mut self, request: Request) -> Reply {
            let id = self.next_id;
            self.next_id += 1;
            let line = serde_json::to_string(&Call { id, request }).unwrap() + "\n";
            let raw = self.send_raw(&line).await;
            serde_json::from_str(raw.trim_end()).expect("a reply")
        }
    }

    async fn spawn(root: &Path) -> (PathBuf, tokio::sync::oneshot::Sender<()>) {
        let sock = root.join("rb.sock");
        let listener = bind(&sock).await.expect("bind");
        let api = test_api(root).await;
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            serve(api, listener, async {
                stop_rx.await.ok();
            })
            .await
            .expect("serve");
        });
        (sock, stop_tx)
    }

    #[tokio::test]
    async fn a_call_comes_back_with_its_own_id() {
        let tmp = tempfile::tempdir().unwrap();
        let (sock, _stop) = spawn(tmp.path()).await;
        let mut c = Client::connect(&sock).await;

        let reply = c.call(Request::ApiVersion).await;
        assert_eq!(reply.id, 1);
        assert_eq!(reply.api_version, bookmole_api::API_VERSION);
        match reply.outcome {
            bookmole_api::Outcome::Ok {
                response: Response::Version { api, .. },
            } => assert_eq!(api, bookmole_api::API_VERSION),
            other => panic!("{other:?}"),
        }

        let reply = c
            .call(Request::ListBooks {
                limit: 10,
                sort: Default::default(),
                offset: 0,
                filter: None,
            })
            .await;
        assert_eq!(reply.id, 2);
    }

    #[tokio::test]
    async fn a_book_written_over_the_socket_reads_back_over_it() {
        let tmp = tempfile::tempdir().unwrap();
        let (sock, _stop) = spawn(tmp.path()).await;
        let mut c = Client::connect(&sock).await;

        let reply = c
            .call(Request::SaveBook {
                book: bookmole_api::BookDto {
                    title: Some("Station Eleven".into()),
                    authors: vec!["Emily St. John Mandel".into()],
                    isbn_13: Some("9781447268963".into()),
                    ..Default::default()
                },
            })
            .await;
        let id = match reply.outcome {
            bookmole_api::Outcome::Ok {
                response: Response::Book(Some(b)),
            } => b.id.expect("saved book has an id"),
            other => panic!("{other:?}"),
        };

        let reply = c.call(Request::GetBook { id }).await;
        match reply.outcome {
            bookmole_api::Outcome::Ok {
                response: Response::Book(Some(b)),
            } => {
                assert_eq!(b.title.as_deref(), Some("Station Eleven"));
                assert_eq!(b.isbn_13.as_deref(), Some("9781447268963"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn an_engine_error_arrives_as_a_code() {
        let tmp = tempfile::tempdir().unwrap();
        let (sock, _stop) = spawn(tmp.path()).await;
        let mut c = Client::connect(&sock).await;

        let reply = c
            .call(Request::UpdateProgress {
                book_id: 4242,
                page: Some(10),
                finished: None,
            })
            .await;
        match reply.outcome {
            bookmole_api::Outcome::Error { error } => {
                assert_eq!(error.code, bookmole_api::ErrorCode::NotFound);
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn a_malformed_line_is_answered_and_the_connection_survives() {
        let tmp = tempfile::tempdir().unwrap();
        let (sock, _stop) = spawn(tmp.path()).await;
        let mut c = Client::connect(&sock).await;

        let raw = c.send_raw("{not json at all\n").await;
        let reply: Reply = serde_json::from_str(raw.trim_end()).unwrap();
        match reply.outcome {
            bookmole_api::Outcome::Error { error } => {
                assert_eq!(error.code, bookmole_api::ErrorCode::BadRequest)
            }
            other => panic!("{other:?}"),
        }
        let raw = c
            .send_raw("{\"id\":7,\"request\":{\"method\":\"invented_later\"}}\n")
            .await;
        let reply: Reply = serde_json::from_str(raw.trim_end()).unwrap();
        assert!(matches!(reply.outcome, bookmole_api::Outcome::Error { .. }));

        assert_eq!(c.call(Request::ApiVersion).await.id, 1);
    }

    #[tokio::test]
    async fn an_overlong_line_is_refused_and_the_connection_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let (sock, _stop) = spawn(tmp.path()).await;
        let (rx, mut tx) = UnixStream::connect(&sock).await.unwrap().into_split();
        let mut reader = BufReader::new(rx);

        // Exactly one byte over the cap, then silence: overshooting leaves unread data,
        // and Linux then resets the connection and discards the reply.
        let payload = vec![b' '; MAX_LINE + 1];
        tx.write_all(&payload).await.unwrap();
        tx.flush().await.unwrap();

        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let reply: Reply = serde_json::from_str(line.trim_end()).unwrap();
        match reply.outcome {
            bookmole_api::Outcome::Error { error } => {
                assert_eq!(error.code, bookmole_api::ErrorCode::BadRequest);
                assert!(error.message.contains("exceeded"), "{}", error.message);
            }
            other => panic!("{other:?}"),
        }
        let mut rest = Vec::new();
        reader.read_to_end(&mut rest).await.unwrap();
        assert!(rest.is_empty(), "expected the connection to be closed");
    }

    /// Asserts on a second client, not the flooder: Linux resets a socket closed with unread data.
    #[tokio::test]
    async fn a_flood_costs_the_flooder_and_nobody_else() {
        let tmp = tempfile::tempdir().unwrap();
        let (sock, _stop) = spawn(tmp.path()).await;
        let (_rx, mut tx) = UnixStream::connect(&sock).await.unwrap().into_split();

        let chunk = vec![b' '; 1024 * 1024];
        for _ in 0..12 {
            if tx.write_all(&chunk).await.is_err() {
                break;
            }
        }

        let mut good = Client::connect(&sock).await;
        assert!(matches!(
            good.call(Request::ApiVersion).await.outcome,
            bookmole_api::Outcome::Ok { .. }
        ));
    }

    #[tokio::test]
    async fn a_second_daemon_refuses_a_live_socket() {
        let tmp = tempfile::tempdir().unwrap();
        let (sock, _stop) = spawn(tmp.path()).await;
        let err = bind(&sock).await.expect_err("must refuse");
        assert!(err.to_string().contains("already listening"), "{err}");
    }

    #[tokio::test]
    async fn a_stale_socket_is_cleared() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("rb.sock");
        drop(UnixListener::bind(&sock).unwrap());
        assert!(sock.exists());
        let listener = bind(&sock).await.expect("stale socket should be cleared");
        drop(listener);
    }

    #[tokio::test]
    async fn the_socket_is_not_world_readable() {
        let tmp = tempfile::tempdir().unwrap();
        let (sock, _stop) = spawn(tmp.path()).await;
        let mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "got {mode:o}");
    }

    #[tokio::test]
    async fn the_guard_takes_the_socket_with_it() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("rb.sock");
        {
            let _l = bind(&sock).await.unwrap();
            let _guard = SocketGuard(sock.clone());
            assert!(sock.exists());
        }
        assert!(!sock.exists());
    }
}
