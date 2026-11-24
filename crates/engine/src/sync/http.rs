//! The kernel client's requests, carried to a sync server over HTTP.
//!
//! A request's target carries the library's id and the log position being read, and its headers
//! a signature, so neither appears in an error or a log line: those name the endpoint only.

use std::time::Duration;

use fongo_sync_client::protocol::api::Route;
use fongo_sync_client::{Method, Request, Response, Transport, TransportError};
use url::Url;

use crate::error::{EngineError, Result};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// A page of the log can be large, and a slow link should finish it.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

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
}

impl HttpTransport {
    pub(crate) fn new(origin: Origin) -> Result<HttpTransport> {
        let client = builder(CONNECT_TIMEOUT, REQUEST_TIMEOUT).build()?;
        Ok(HttpTransport { client, origin })
    }

    #[cfg(test)]
    fn with_timeout(origin: Origin, timeout: Duration) -> HttpTransport {
        let client = builder(timeout, timeout).no_proxy().build().unwrap();
        HttpTransport { client, origin }
    }
}

/// A redirect would send a signed request to a target it was not signed for, so none is followed.
fn builder(connect: Duration, request: Duration) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .user_agent(concat!("bookmole/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(connect)
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
        let body = response.bytes().await.map_err(fail)?.to_vec();
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
