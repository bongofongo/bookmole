//! Fetching a cover from its URL. A cover URL can come from a provider, from the reader, or from
//! another device by sync, so every fetch is held to one policy: `https` only, to hosts whose
//! every address is public, checked on the addresses the connection is actually made to (the
//! resolver hands the connector only vetted addresses) and again on every redirect, with a small
//! redirect limit, a size cap and a check that the bytes are an image. No proxy, cookie or
//! credential is ever used.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use url::{Host, Url};

use super::{CoverFile, store_cover};
use crate::error::{EngineError, Result};

/// Larger than any cover a provider serves.
pub(crate) const COVER_LIMIT: usize = 10 << 20;
const REDIRECTS: usize = 5;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Why a cover URL was not fetched. Never names the URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CoverRefusal {
    #[error("a cover is fetched over https only")]
    NotHttps,
    #[error("a cover's host is not on the public internet")]
    NotPublic,
    #[error("a cover's address redirects too many times")]
    TooManyRedirects,
    #[error("the cover is larger than any cover should be")]
    TooLarge,
    #[error("what the cover's address returned is not an image")]
    NotAnImage,
}

impl From<CoverRefusal> for EngineError {
    fn from(refusal: CoverRefusal) -> EngineError {
        EngineError::InvalidInput(refusal.to_string())
    }
}

/// Whether `ip` is an address on the public internet: not loopback, private, shared,
/// link-local, unique-local, multicast, unspecified, broadcast, documentation, benchmarking or
/// reserved, in IPv4 or IPv6, including an IPv4 address carried inside an IPv6 one.
pub(crate) fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => public_v4(v4),
        IpAddr::V6(v6) => public_v6(v6),
    }
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || a == 0
        || a >= 240
        || (a == 100 && (64..128).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 198 && (18..20).contains(&b)))
}

fn public_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return false;
    }
    if let Some(v4) = ip.to_ipv4_mapped() {
        return public_v4(v4);
    }
    let embedded = |hi: u16, lo: u16| Ipv4Addr::from((u32::from(hi) << 16) | u32::from(lo));
    match s[0] {
        // IPv4-compatible (::a.b.c.d), long deprecated: judged by the address it carries.
        0 if s[1..6] == [0; 5] => public_v4(embedded(s[6], s[7])),
        // NAT64, which reaches the IPv4 address it carries.
        0x64 if s[1] == 0xff9b && s[2..6] == [0; 4] => public_v4(embedded(s[6], s[7])),
        // 6to4, which carries an IPv4 address in its second and third segments.
        0x2002 => public_v4(embedded(s[1], s[2])),
        // Teredo and documentation.
        0x2001 if s[1] == 0 || s[1] == 0x0db8 => false,
        // Unique-local, link-local and the old site-local.
        x if (x & 0xfe00) == 0xfc00 || (x & 0xffc0) == 0xfe80 || (x & 0xffc0) == 0xfec0 => false,
        _ => true,
    }
}

type Lookup = dyn Fn(&str) -> std::io::Result<Vec<IpAddr>> + Send + Sync;

/// What a fetch may reach beyond the policy above; nothing, except in tests, which serve over
/// plain HTTP on loopback.
#[derive(Clone, Default)]
struct Allowance {
    http: bool,
    addresses: Vec<IpAddr>,
}

impl Allowance {
    fn allows(&self, ip: IpAddr) -> bool {
        is_public(ip) || self.addresses.contains(&ip)
    }

    /// Whether `url` may be asked for: its scheme, and its host when the host is an address,
    /// which the resolver never sees.
    fn check(&self, url: &Url) -> std::result::Result<(), CoverRefusal> {
        match url.scheme() {
            "https" => {}
            "http" if self.http => {}
            _ => return Err(CoverRefusal::NotHttps),
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(CoverRefusal::NotPublic);
        }
        match url.host() {
            Some(Host::Domain(_)) => Ok(()),
            Some(Host::Ipv4(ip)) if self.allows(IpAddr::V4(ip)) => Ok(()),
            Some(Host::Ipv6(ip)) if self.allows(IpAddr::V6(ip)) => Ok(()),
            _ => Err(CoverRefusal::NotPublic),
        }
    }
}

/// Resolves a host and hands on its addresses only when every one is allowed, so the connection
/// is made to an address that was checked.
struct PublicOnly {
    lookup: Arc<Lookup>,
    allowance: Allowance,
}

impl Resolve for PublicOnly {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_owned();
        let lookup = self.lookup.clone();
        let allowance = self.allowance.clone();
        Box::pin(async move {
            let found = tokio::task::spawn_blocking(move || lookup(&host)).await??;
            if found.is_empty() || !found.iter().all(|ip| allowance.allows(*ip)) {
                return Err(Box::new(CoverRefusal::NotPublic) as Box<_>);
            }
            let addrs: Addrs = Box::new(found.into_iter().map(|ip| SocketAddr::new(ip, 0)));
            Ok(addrs)
        })
    }
}

fn system_lookup(host: &str) -> std::io::Result<Vec<IpAddr>> {
    Ok((host, 0).to_socket_addrs()?.map(|a| a.ip()).collect())
}

/// Fetches covers under the policy above.
pub(crate) struct CoverClient {
    client: reqwest::Client,
    allowance: Allowance,
}

impl CoverClient {
    pub(crate) fn new() -> Result<CoverClient> {
        CoverClient::build(Arc::new(system_lookup), Allowance::default())
    }

    fn build(lookup: Arc<Lookup>, allowance: Allowance) -> Result<CoverClient> {
        let hops = allowance.clone();
        let redirects = reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() > REDIRECTS {
                return attempt.error(CoverRefusal::TooManyRedirects);
            }
            match hops.check(attempt.url()) {
                Ok(()) => attempt.follow(),
                Err(refusal) => attempt.error(refusal),
            }
        });
        let client = reqwest::Client::builder()
            .user_agent(concat!("bookmole/", env!("CARGO_PKG_VERSION")))
            .no_proxy()
            .redirect(redirects)
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .dns_resolver(Arc::new(PublicOnly {
                lookup,
                allowance: allowance.clone(),
            }))
            .build()?;
        Ok(CoverClient { client, allowance })
    }

    /// Fetches the cover at `url` and stores it in `images_dir`.
    pub(crate) async fn fetch(&self, url: &str, images_dir: &Path) -> Result<CoverFile> {
        let bytes = self.bytes(url).await?;
        store_cover(&bytes, images_dir)
    }

    async fn bytes(&self, url: &str) -> Result<Vec<u8>> {
        let url = Url::parse(url)?;
        self.allowance.check(&url)?;
        let mut response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(refused_or_failed)?
            .error_for_status()
            .map_err(reqwest::Error::without_url)?;
        if response
            .content_length()
            .is_some_and(|len| len > COVER_LIMIT as u64)
        {
            return Err(CoverRefusal::TooLarge.into());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(refused_or_failed)? {
            if chunk.len() > COVER_LIMIT - bytes.len() {
                return Err(CoverRefusal::TooLarge.into());
            }
            bytes.extend_from_slice(&chunk);
        }
        if image::guess_format(&bytes).is_err() {
            return Err(CoverRefusal::NotAnImage.into());
        }
        Ok(bytes)
    }
}

/// A refusal of the resolver or the redirect policy as itself; anything else without its URL.
fn refused_or_failed(e: reqwest::Error) -> EngineError {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(&e);
    while let Some(err) = source {
        if let Some(refusal) = err.downcast_ref::<CoverRefusal>() {
            return (*refusal).into();
        }
        source = err.source();
    }
    EngineError::Http(e.without_url())
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use wiremock::matchers::path;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn png() -> Vec<u8> {
        let img = image::RgbImage::from_pixel(4, 6, image::Rgb([200, 30, 30]));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    fn v4(octets: [u8; 4]) -> IpAddr {
        IpAddr::from(octets)
    }

    /// Private, shared and reserved IPv4 addresses, built from octets.
    fn inward_v4() -> Vec<Ipv4Addr> {
        [
            [127, 0, 0, 1],
            [10, 0, 0, 1],
            [172, 16, 4, 1],
            [192, 168, 1, 1],
            [169, 254, 169, 254],
            [100, 64, 0, 1],
            [0, 0, 0, 0],
            [255, 255, 255, 255],
            [224, 0, 0, 1],
            [192, 0, 2, 7],
            [198, 18, 0, 1],
            [240, 0, 0, 1],
        ]
        .map(Ipv4Addr::from)
        .to_vec()
    }

    #[test]
    fn only_a_public_address_is_one_a_cover_may_come_from() {
        for v4 in inward_v4() {
            assert!(!is_public(IpAddr::V4(v4)), "{v4}");
            assert!(!is_public(IpAddr::V6(v4.to_ipv6_mapped())), "{v4} mapped");
            assert!(
                !is_public(IpAddr::V6(v4.to_ipv6_compatible())),
                "{v4} compatible"
            );
        }
        for refused in [
            "::1",
            "::",
            "64:ff9b::a00:1",
            "2002:7f00:1::",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "fec0::1",
            "ff02::1",
            "2001:db8::1",
            "2001::1",
        ] {
            assert!(!is_public(IpAddr::from_str(refused).unwrap()), "{refused}");
        }
        let public = Ipv4Addr::from([93, 184, 216, 34]);
        assert!(is_public(IpAddr::V4(public)));
        assert!(is_public(IpAddr::V6(public.to_ipv6_mapped())));
        assert!(is_public(IpAddr::from_str("2606:4700::6810:84e5").unwrap()));
    }

    #[test]
    fn an_address_or_scheme_is_refused_before_anything_is_asked() {
        let allowance = Allowance::default();
        let mut refused = vec![
            (
                "http://covers.example.test/1.jpg".to_owned(),
                CoverRefusal::NotHttps,
            ),
            (
                "ftp://covers.example.test/1.jpg".into(),
                CoverRefusal::NotHttps,
            ),
            ("file:///etc/passwd".into(), CoverRefusal::NotHttps),
            ("https://[::1]/1.jpg".into(), CoverRefusal::NotPublic),
            (
                "https://reader:pw@covers.example.test/1.jpg".into(),
                CoverRefusal::NotPublic,
            ),
        ];
        for v4 in inward_v4() {
            refused.push((format!("https://{v4}/1.jpg"), CoverRefusal::NotPublic));
            let mapped = v4.to_ipv6_mapped();
            refused.push((format!("https://[{mapped}]/1.jpg"), CoverRefusal::NotPublic));
        }
        for (url, refusal) in refused {
            let url = Url::parse(&url).unwrap();
            assert_eq!(allowance.check(&url), Err(refusal), "{url}");
        }
        assert!(
            allowance
                .check(&Url::parse("https://covers.example.test/1.jpg").unwrap())
                .is_ok()
        );
    }

    /// A client that may reach the mock server, and resolves `public.test` to it and
    /// `private.test` to a private address.
    fn client(server: &MockServer) -> CoverClient {
        let at = *server.address();
        let lookup = move |host: &str| match host {
            "public.test" => Ok(vec![at.ip()]),
            "private.test" => Ok(vec![v4([10, 0, 0, 7])]),
            "mixed.test" => Ok(vec![at.ip(), v4([192, 168, 0, 7])]),
            _ => Err(std::io::Error::other("no such host")),
        };
        CoverClient::build(
            Arc::new(lookup),
            Allowance {
                http: true,
                addresses: vec![at.ip()],
            },
        )
        .unwrap()
    }

    fn on(server: &MockServer, host: &str, path: &str) -> String {
        format!("http://{host}:{}{path}", server.address().port())
    }

    #[tokio::test]
    async fn a_cover_is_fetched_only_from_a_public_host_and_only_if_it_is_an_image() {
        let server = MockServer::start().await;
        let images = tempfile::tempdir().unwrap();
        Mock::given(path("/cover.png"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(png()))
            .mount(&server)
            .await;
        Mock::given(path("/inward"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", on(&server, "private.test", "/cover.png")),
            )
            .mount(&server)
            .await;
        Mock::given(path("/to-loopback"))
            .respond_with(
                ResponseTemplate::new(302).insert_header("location", "https://[::1]/cover.png"),
            )
            .mount(&server)
            .await;
        Mock::given(path("/round"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", on(&server, "public.test", "/round")),
            )
            .mount(&server)
            .await;
        Mock::given(path("/huge.png"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0x89; COVER_LIMIT + 1]))
            .mount(&server)
            .await;
        Mock::given(path("/page.html"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html>not a cover</html>"))
            .mount(&server)
            .await;

        let client = client(&server);
        let stored = client
            .fetch(&on(&server, "public.test", "/cover.png"), images.path())
            .await
            .unwrap();
        assert!(stored.path.exists());
        assert!(stored.metrics.is_some());

        for (where_, refusal) in [
            ("private.test", "/cover.png"),
            ("mixed.test", "/cover.png"),
            ("public.test", "/inward"),
            ("public.test", "/to-loopback"),
        ]
        .map(|(host, path)| (on(&server, host, path), CoverRefusal::NotPublic))
        .into_iter()
        .chain([
            (
                on(&server, "public.test", "/round"),
                CoverRefusal::TooManyRedirects,
            ),
            (
                on(&server, "public.test", "/huge.png"),
                CoverRefusal::TooLarge,
            ),
            (
                on(&server, "public.test", "/page.html"),
                CoverRefusal::NotAnImage,
            ),
        ]) {
            let error = client.fetch(&where_, images.path()).await.unwrap_err();
            assert_eq!(
                error.to_string(),
                EngineError::from(refusal).to_string(),
                "{where_}"
            );
            assert!(!error.to_string().contains("test"), "{error}");
        }
        let kept: Vec<_> = std::fs::read_dir(images.path()).unwrap().collect();
        assert_eq!(kept.len(), 1, "{kept:?}");
        let fetched = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path() == "/cover.png")
            .count();
        assert_eq!(fetched, 1, "only the public fetch reached the image");
    }

    #[tokio::test]
    async fn a_production_client_refuses_plain_http_and_loopback_outright() {
        let server = MockServer::start().await;
        Mock::given(path("/cover.png"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(png()))
            .mount(&server)
            .await;
        let images = tempfile::tempdir().unwrap();
        let client = CoverClient::new().unwrap();
        for url in [
            format!("{}/cover.png", server.uri()),
            format!("https://127.0.0.1:{}/cover.png", server.address().port()),
            format!("https://[::1]:{}/cover.png", server.address().port()),
        ] {
            assert!(client.fetch(&url, images.path()).await.is_err(), "{url}");
        }
        assert!(server.received_requests().await.unwrap().is_empty());
    }
}
