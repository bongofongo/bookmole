//! The kernel client's requests, carried to a sync server over HTTP.
//!
//! A request's target carries the library's id and the log position being read, and its headers
//! a signature, so neither appears in an error or a log line: those name the endpoint only. Nor
//! does anything of a response's body.

use std::time::Duration;

use fongo_sync_client::protocol::api::Route;
use fongo_sync_client::{Method, Request, Response, Transport, TransportError};
use url::Url;

use crate::error::{EngineError, Result};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// A server that stops sending mid-answer is given up on after this long without a byte.
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// A whole request, answer included: long enough for a page at [`PAGE_LIMIT`] over a slow link,
/// short enough that a server trickling bytes cannot hold a sync for ever.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

/// The largest answer to a read of the log. A page's size is bounded only by capabilities a
/// server may leave out (`max_page_batches`, `max_batch_bytes`, `max_page_bytes`; absent means
/// unlimited), so no legitimate maximum is knowable here: this is a fixed ceiling, far above what
/// a page of a real library reaches. A page over it fails the sync rather than exhaust memory.
const PAGE_LIMIT: usize = 128 << 20;
/// The largest answer to anything else: capabilities, a device list, an append's sequence
/// number, an error. All are small JSON.
const JSON_LIMIT: usize = 1 << 20;

#[derive(Debug, Clone, Copy)]
struct Limits {
    page: usize,
    json: usize,
}

const LIMITS: Limits = Limits {
    page: PAGE_LIMIT,
    json: JSON_LIMIT,
};

/// The server's origin, `scheme://host[:port]`, which the client's targets are appended to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Origin(String);

impl Origin {
    /// Accepts an `http` or `https` address with nothing after the host but an optional `/`:
    /// requests are signed over their target as written, so a server under a further path
    /// cannot verify them. The error never repeats the address, which may hold anything typed.
    pub(crate) fn parse(server_url: &str) -> Result<Origin> {
        let refuse = |why: &str| {
            Err(EngineError::InvalidInput(format!(
                "the sync server's address {why}"
            )))
        };
        let Ok(url) = Url::parse(server_url.trim()) else {
            return refuse("is not a web address");
        };
        if !matches!(url.scheme(), "http" | "https") {
            return refuse("must start with https:// or http://");
        }
        if url.host_str().is_none_or(str::is_empty) {
            return refuse("names no host");
        }
        if !url.username().is_empty() || url.password().is_some() {
            return refuse("must not carry a user name or password");
        }
        if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
            return refuse("must be the server's root, with nothing after the host");
        }
        Ok(Origin(url.origin().ascii_serialization()))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// No retries: the client decides what a failed request means, and the next sync tries again.
pub(crate) struct HttpTransport {
    client: reqwest::Client,
    origin: Origin,
    limits: Limits,
}

impl HttpTransport {
    pub(crate) fn new(origin: Origin) -> Result<HttpTransport> {
        let client = builder(CONNECT_TIMEOUT, READ_TIMEOUT, REQUEST_TIMEOUT).build()?;
        Ok(HttpTransport {
            client,
            origin,
            limits: LIMITS,
        })
    }

    #[cfg(test)]
    fn with_limits(origin: Origin, page: usize, json: usize) -> HttpTransport {
        HttpTransport {
            limits: Limits { page, json },
            ..HttpTransport::with_timeout(origin, Duration::from_secs(5))
        }
    }

    #[cfg(test)]
    fn with_timeout(origin: Origin, timeout: Duration) -> HttpTransport {
        let client = builder(timeout, timeout, timeout)
            .no_proxy()
            .build()
            .unwrap();
        HttpTransport {
            client,
            origin,
            limits: LIMITS,
        }
    }
}

/// A redirect would carry the signed headers to a target they were not signed for, perhaps on
/// another host, so none is followed: a 3xx is handed to the client as the answer. Responses are
/// never decompressed (reqwest is built without gzip), so a body's size on the wire is its size.
fn builder(connect: Duration, read: Duration, request: Duration) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .user_agent(concat!("bookmole/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(connect)
        .read_timeout(read)
        .timeout(request)
        .redirect(reqwest::redirect::Policy::none())
}

/// Why a request got no response. Says nothing of where it was going.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum HttpFailure {
    #[error("the sync server did not answer in time")]
    Timeout,
    #[error("could not connect to the sync server")]
    Connect,
    #[error("the request to the sync server failed")]
    Request,
    #[error("the sync server's answer could not be read")]
    Body,
    #[error("the sync server's answer is larger than any it should send")]
    TooLarge,
}

impl HttpFailure {
    fn of(e: &reqwest::Error) -> HttpFailure {
        if e.is_timeout() {
            HttpFailure::Timeout
        } else if e.is_connect() {
            HttpFailure::Connect
        } else if e.is_body() || e.is_decode() {
            HttpFailure::Body
        } else {
            HttpFailure::Request
        }
    }
}

/// The endpoint a target names, for logs: never its library id or query.
fn endpoint(target: &str) -> &'static str {
    match Route::parse(target) {
        Some((Route::Capabilities, _)) => "capabilities",
        Some((Route::Libraries, _)) => "libraries",
        Some((Route::Devices(_), _)) => "devices",
        Some((Route::Batches(_), _)) => "batches",
        Some((Route::Notifications(_), _)) => "notifications",
        None => "unknown",
    }
}

/// The body, refused once it passes `limit` bytes whatever its `Content-Length` said.
async fn read_body(
    mut response: reqwest::Response,
    limit: usize,
) -> std::result::Result<Vec<u8>, HttpFailure> {
    if response
        .content_length()
        .is_some_and(|len| len > limit as u64)
    {
        return Err(HttpFailure::TooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| HttpFailure::of(&e))? {
        if chunk.len() > limit - body.len() {
            return Err(HttpFailure::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

impl Transport for HttpTransport {
    async fn send(&self, request: Request) -> std::result::Result<Response, TransportError> {
        let endpoint = endpoint(&request.target);
        let method = match request.method {
            Method::Get => reqwest::Method::GET,
            Method::Post => reqwest::Method::POST,
        };
        let mut builder = self.client.request(
            method,
            format!("{}{}", self.origin.as_str(), request.target),
        );
        for (name, value) in &request.headers {
            builder = builder.header(*name, value);
        }
        if request.method == Method::Post {
            builder = builder.body(request.body);
        }
        let fail = |e: reqwest::Error| {
            let failure = HttpFailure::of(&e);
            tracing::debug!(endpoint, %failure, "sync request got no response");
            TransportError::new(failure)
        };
        let response = builder.send().await.map_err(fail)?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                Some((name.as_str().to_owned(), value.to_str().ok()?.to_owned()))
            })
            .collect();
        let limit = if endpoint == "batches" && request.method == Method::Get {
            self.limits.page
        } else {
            self.limits.json
        };
        let body = read_body(response, limit).await.map_err(|failure| {
            tracing::debug!(endpoint, status, %failure, "sync server's answer was not read");
            TransportError::new(failure)
        })?;
        if !(200..300).contains(&status) {
            tracing::debug!(endpoint, status, "sync server refused a request");
        }
        Ok(Response {
            status,
            headers,
            body,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tracing::field::{Field, Visit};
    use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
    use wiremock::matchers::{body_bytes, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    const LIBRARY: &str = "0123456789abcdef0123456789abcdef";

    fn transport(server: &MockServer, timeout: Duration) -> HttpTransport {
        HttpTransport::with_timeout(Origin::parse(&server.uri()).unwrap(), timeout)
    }

    fn read(after: u64) -> Request {
        Request {
            method: Method::Get,
            target: format!("/v1/libraries/{LIBRARY}/batches?after={after}&limit=4"),
            headers: vec![
                ("x-fongo-device", "d1".into()),
                ("x-fongo-signature", "c2lnbmVk".into()),
            ],
            body: Vec::new(),
        }
    }

    #[test]
    fn a_server_address_is_an_origin_and_a_refusal_never_repeats_it() {
        for (given, origin) in [
            ("https://sync.example.test", "https://sync.example.test"),
            ("https://sync.example.test/", "https://sync.example.test"),
            (
                "  https://Sync.Example.Test:8443/ ",
                "https://sync.example.test:8443",
            ),
            ("http://192.0.2.7:8080", "http://192.0.2.7:8080"),
        ] {
            assert_eq!(Origin::parse(given).unwrap().as_str(), origin, "{given}");
        }
        for given in [
            "sync.example.test",
            "ftp://sync.example.test",
            "https://reader:hunter2@sync.example.test",
            "https://sync.example.test/bookmole",
            "https://sync.example.test/?token=hunter2",
            "https://sync.example.test/#hunter2",
            "file:///hunter2",
        ] {
            let refused = Origin::parse(given).unwrap_err().to_string();
            assert!(matches!(
                Origin::parse(given),
                Err(EngineError::InvalidInput(_))
            ));
            assert!(
                !refused.contains("hunter2") && !refused.contains("example.test"),
                "{refused}"
            );
        }
    }

    #[tokio::test]
    async fn a_request_reaches_the_server_as_the_client_wrote_it() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/v1/libraries/{LIBRARY}/batches")))
            .and(query_param("after", "3"))
            .and(query_param("limit", "4"))
            .and(header("x-fongo-signature", "c2lnbmVk"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/octet-stream")
                    .set_body_bytes(vec![1, 2, 3]),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/v1/libraries/{LIBRARY}/batches")))
            .and(header("content-type", "application/octet-stream"))
            .and(body_bytes(vec![9, 8, 7]))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"seq":5}"#))
            .mount(&server)
            .await;

        let t = transport(&server, Duration::from_secs(5));
        let got = t.send(read(3)).await.unwrap();
        assert_eq!(got.status, 200);
        assert_eq!(got.body, [1, 2, 3]);
        assert!(
            got.headers
                .contains(&("content-type".into(), "application/octet-stream".into()))
        );
        let posted = t
            .send(Request {
                method: Method::Post,
                target: format!("/v1/libraries/{LIBRARY}/batches"),
                headers: vec![("content-type", "application/octet-stream".into())],
                body: vec![9, 8, 7],
            })
            .await
            .unwrap();
        assert_eq!(posted.body, br#"{"seq":5}"#);

        let seen = server.received_requests().await.unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(
            (seen[0].url.path(), seen[0].url.query()),
            (
                format!("/v1/libraries/{LIBRARY}/batches").as_str(),
                Some("after=3&limit=4")
            )
        );
        let agent = seen[0].headers.get("user-agent").unwrap().to_str().unwrap();
        assert_eq!(agent, concat!("bookmole/", env!("CARGO_PKG_VERSION")));
        assert!(seen[0].body.is_empty());
        for request in &seen {
            assert!(request.headers.get("cookie").is_none());
            assert!(request.headers.get("authorization").is_none());
        }
    }

    #[tokio::test]
    async fn a_refusal_is_a_response_and_a_redirect_is_not_followed() {
        let server = MockServer::start().await;
        Mock::given(path("/v1/capabilities"))
            .respond_with(
                ResponseTemplate::new(409).set_body_string(r#"{"code":"library_exists"}"#),
            )
            .mount(&server)
            .await;
        Mock::given(path(format!("/v1/libraries/{LIBRARY}/batches")))
            .respond_with(
                ResponseTemplate::new(307).insert_header("location", "https://elsewhere.test/"),
            )
            .mount(&server)
            .await;
        let t = transport(&server, Duration::from_secs(5));
        let refused = t
            .send(Request {
                method: Method::Get,
                target: "/v1/capabilities".into(),
                headers: Vec::new(),
                body: Vec::new(),
            })
            .await
            .unwrap();
        assert_eq!(refused.status, 409);
        assert_eq!(refused.body, br#"{"code":"library_exists"}"#);
        assert_eq!(t.send(read(0)).await.unwrap().status, 307);
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }

    const PAGE: usize = 64 << 10;
    const JSON: usize = 1 << 10;

    fn capabilities() -> Request {
        Request {
            method: Method::Get,
            target: "/v1/capabilities".into(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    fn too_large(result: std::result::Result<Response, TransportError>) -> bool {
        result.is_err_and(|e| {
            let text = e.to_string();
            assert!(!text.contains("after=") && !text.contains("xxxx"), "{text}");
            e.into_inner().downcast_ref::<HttpFailure>() == Some(&HttpFailure::TooLarge)
        })
    }

    /// A server that answers one request with `response` exactly as given, whatever it says.
    fn answering_with(response: Vec<u8>) -> Origin {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = Origin::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut seen = Vec::new();
            let mut buf = [0u8; 1024];
            while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                match socket.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => seen.extend_from_slice(&buf[..n]),
                }
            }
            let _ = socket.write_all(&response);
        });
        origin
    }

    fn chunked(total: usize) -> Vec<u8> {
        let mut out =
            b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n".to_vec();
        let mut left = total;
        while left > 0 {
            let n = left.min(1000);
            out.extend_from_slice(format!("{n:x}\r\n").as_bytes());
            out.extend(std::iter::repeat_n(b'x', n));
            out.extend_from_slice(b"\r\n");
            left -= n;
        }
        out.extend_from_slice(b"0\r\n\r\n");
        out
    }

    #[tokio::test]
    async fn an_answer_at_its_limit_is_read_and_one_past_it_is_refused() {
        let server = MockServer::start().await;
        for (size, at) in [(PAGE, 1), (PAGE + 1, 2)] {
            Mock::given(method("GET"))
                .and(path(format!("/v1/libraries/{LIBRARY}/batches")))
                .and(query_param("after", at.to_string()))
                .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'x'; size]))
                .mount(&server)
                .await;
        }
        Mock::given(path("/v1/capabilities"))
            .respond_with(ResponseTemplate::new(500).set_body_bytes(vec![b'x'; JSON + 1]))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'x'; JSON]))
            .mount(&server)
            .await;
        let t = HttpTransport::with_limits(Origin::parse(&server.uri()).unwrap(), PAGE, JSON);
        assert_eq!(t.send(read(1)).await.unwrap().body.len(), PAGE);
        assert!(too_large(t.send(read(2)).await));
        assert!(too_large(t.send(capabilities()).await));
        let appended = t
            .send(Request {
                method: Method::Post,
                target: format!("/v1/libraries/{LIBRARY}/batches"),
                headers: Vec::new(),
                body: vec![1],
            })
            .await
            .unwrap();
        assert_eq!(appended.body.len(), JSON);
    }

    #[tokio::test]
    async fn an_answer_with_no_length_is_refused_once_it_passes_its_limit() {
        let t = |origin| HttpTransport::with_limits(origin, PAGE, JSON);
        assert!(too_large(
            t(answering_with(chunked(PAGE + 1))).send(read(0)).await
        ));
        assert!(too_large(
            t(answering_with(chunked(JSON + 1)))
                .send(capabilities())
                .await
        ));
        let at_limit = t(answering_with(chunked(PAGE))).send(read(0)).await;
        assert_eq!(at_limit.unwrap().body.len(), PAGE);
    }

    #[tokio::test]
    async fn an_answer_whose_length_understates_is_read_no_further_than_it_says() {
        let mut lying = b"HTTP/1.1 200 OK\r\ncontent-length: 10\r\n\r\n".to_vec();
        lying.extend(std::iter::repeat_n(b'x', PAGE * 4));
        let got = HttpTransport::with_limits(answering_with(lying), PAGE, JSON)
            .send(read(0))
            .await
            .unwrap();
        assert_eq!(got.body.len(), 10);

        let mut overstating = b"HTTP/1.1 200 OK\r\ncontent-length: 1000000000\r\n\r\n".to_vec();
        overstating.extend(std::iter::repeat_n(b'x', 10));
        let refused = HttpTransport::with_limits(answering_with(overstating), PAGE, JSON)
            .send(read(0))
            .await;
        assert!(too_large(refused));
    }

    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<String>>>);

    struct Collector<'a>(&'a Captured, &'a str);

    impl Visit for Collector<'_> {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            let line = format!("{} {}={value:?}", self.1, field.name());
            self.0.0.lock().unwrap().push(line);
        }
    }

    impl<S: tracing::Subscriber> Layer<S> for Captured {
        fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
            event.record(&mut Collector(self, event.metadata().target()));
        }
    }

    #[test]
    fn a_failed_request_names_no_address_or_query_in_its_error_or_log() {
        let captured = Captured::default();
        let subscriber = tracing_subscriber::registry().with(captured.clone());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let text = tracing::subscriber::with_default(subscriber, || runtime.block_on(fail_twice()));
        let lines = captured.0.lock().unwrap().clone();
        assert!(lines.len() >= 2, "{lines:?}");
        for line in lines.iter().chain(&text) {
            for secret in ["after=", LIBRARY, "c2lnbmVk"] {
                assert!(!line.contains(secret), "{secret} in {line}");
            }
        }
        let ours: Vec<&String> = lines.iter().filter(|l| l.starts_with("bookmole")).collect();
        for line in ours.iter().copied().chain(&text) {
            assert!(!line.contains("127.0.0.1"), "{line}");
        }
        assert!(ours.iter().any(|l| l.contains("batches")), "{lines:?}");
    }

    /// A request that times out and one with no server to reach; their errors' text.
    async fn fail_twice() -> Vec<String> {
        let slow = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
            .mount(&slow)
            .await;
        let timed_out = transport(&slow, Duration::from_millis(100))
            .send(read(41))
            .await
            .unwrap_err();
        let timed_out_text = timed_out.to_string();
        assert_eq!(
            timed_out.into_inner().downcast_ref::<HttpFailure>(),
            Some(&HttpFailure::Timeout)
        );

        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = Origin::parse(&format!("http://{}", closed.local_addr().unwrap())).unwrap();
        drop(closed);
        let refused = HttpTransport::with_timeout(origin, Duration::from_secs(5))
            .send(read(42))
            .await
            .unwrap_err();
        let text = refused.to_string();
        assert_eq!(
            refused.into_inner().downcast_ref::<HttpFailure>(),
            Some(&HttpFailure::Connect)
        );

        vec![timed_out_text, text]
    }
}
