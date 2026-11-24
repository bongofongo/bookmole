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
    #[error("the cover is larger, in bytes or pixels, than any cover should be")]
    TooLarge,
    #[error("what the cover's address returned is not an image")]
    NotAnImage,
}

impl From<CoverRefusal> for EngineError {
    fn from(refusal: CoverRefusal) -> EngineError {
        EngineError::InvalidInput(refusal.to_string())
    }
}

/// Whether `ip` is an address on the public internet. Deny by default: an IPv4 address outside
/// [`REFUSED_V4`], or an IPv6 address in global unicast (`2000::/3`) outside [`REFUSED_V6`]; an
/// IPv4 address an IPv6 one carries (mapped, NAT64, 6to4) is judged as itself.
pub(crate) fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => public_v4(v4),
        IpAddr::V6(v6) => public_v6(v6),
    }
}

/// Every IPv4 range that is not the public internet, as (first address, prefix length): this
/// network, private, shared (CGNAT), loopback, link-local, private, IETF protocol assignments,
/// TEST-NET-1, the old 6to4 relay anycast, private, benchmarking, TEST-NET-2, TEST-NET-3,
/// multicast, and reserved (which holds the limited broadcast address).
pub(crate) const REFUSED_V4: &[([u8; 4], u8)] = &[
    ([0, 0, 0, 0], 8),
    ([10, 0, 0, 0], 8),
    ([100, 64, 0, 0], 10),
    ([127, 0, 0, 0], 8),
    ([169, 254, 0, 0], 16),
    ([172, 16, 0, 0], 12),
    ([192, 0, 0, 0], 24),
    ([192, 0, 2, 0], 24),
    ([192, 88, 99, 0], 24),
    ([192, 168, 0, 0], 16),
    ([198, 18, 0, 0], 15),
    ([198, 51, 100, 0], 24),
    ([203, 0, 113, 0], 24),
    ([224, 0, 0, 0], 4),
    ([240, 0, 0, 0], 4),
];

/// The special-purpose ranges inside global unicast that are refused: the IETF protocol
/// assignments (holding Teredo, benchmarking and ORCHID), documentation, the newer
/// documentation prefix, and segment-routing identifiers. `2002::/16` (6to4) is judged by the
/// IPv4 address it carries instead.
pub(crate) const REFUSED_V6: &[([u16; 2], u8)] = &[
    ([0x2001, 0x0000], 23),
    ([0x2001, 0x0db8], 32),
    ([0x3fff, 0x0000], 20),
    ([0x5f00, 0x0000], 16),
];

fn in_v4(ip: Ipv4Addr, (base, len): ([u8; 4], u8)) -> bool {
    let mask = u32::MAX.checked_shl(32 - u32::from(len)).unwrap_or(0);
    u32::from(ip) & mask == u32::from(Ipv4Addr::from(base)) & mask
}

fn public_v4(ip: Ipv4Addr) -> bool {
    !REFUSED_V4.iter().any(|&range| in_v4(ip, range))
}

fn public_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    let embedded = |hi: u16, lo: u16| Ipv4Addr::from((u32::from(hi) << 16) | u32::from(lo));
    // IPv4-mapped, `::ffff:0:0/96`.
    if s[..5] == [0; 5] && s[5] == 0xffff {
        return public_v4(embedded(s[6], s[7]));
    }
    // NAT64's well-known prefix, `64:ff9b::/96`; its local-use `64:ff9b:1::/48` is refused below.
    if s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        return public_v4(embedded(s[6], s[7]));
    }
    if s[0] & 0xe000 != 0x2000 {
        return false;
    }
    if s[0] == 0x2002 {
        return public_v4(embedded(s[1], s[2]));
    }
    let top = (u32::from(s[0]) << 16) | u32::from(s[1]);
    !REFUSED_V6.iter().any(|&(base, len)| {
        let base = (u32::from(base[0]) << 16) | u32::from(base[1]);
        let mask = u32::MAX << (32 - u32::from(len));
        top & mask == base & mask
    })
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
        let images_dir = images_dir.to_owned();
        tokio::task::spawn_blocking(move || store_cover(&bytes, &images_dir))
            .await
            .map_err(|e| EngineError::Other(format!("storing a cover: {e}")))?
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
        match super::dimensions(&bytes) {
            Some(size) if super::within_limits(size) => Ok(bytes),
            Some(_) => Err(CoverRefusal::TooLarge.into()),
            None => Err(CoverRefusal::NotAnImage.into()),
        }
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

    /// The first, a middle and the last address of every refused IPv4 range.
    /// Every IPv4 range that is not the public internet, written out here rather than read from
    /// the table under test, so an entry missing there fails.
    const EXPECTED_REFUSED_V4: &[([u8; 4], u8)] = &[
        ([0, 0, 0, 0], 8),
        ([10, 0, 0, 0], 8),
        ([100, 64, 0, 0], 10),
        ([127, 0, 0, 0], 8),
        ([169, 254, 0, 0], 16),
        ([172, 16, 0, 0], 12),
        ([192, 0, 0, 0], 24),
        ([192, 0, 2, 0], 24),
        ([192, 88, 99, 0], 24),
        ([192, 168, 0, 0], 16),
        ([198, 18, 0, 0], 15),
        ([198, 51, 100, 0], 24),
        ([203, 0, 113, 0], 24),
        ([224, 0, 0, 0], 4),
        ([240, 0, 0, 0], 4),
    ];

    /// The same for IPv6: every range refused, as its first address and prefix length.
    const EXPECTED_REFUSED_V6: &[(&str, u8)] = &[
        ("::", 96),
        ("::ffff:0:0:0", 96),
        ("64:ff9b:1::", 48),
        ("100::", 64),
        ("2001::", 23),
        ("2001:db8::", 32),
        ("3fff::", 20),
        ("5f00::", 16),
        ("fc00::", 7),
        ("fe80::", 10),
        ("fec0::", 10),
        ("ff00::", 8),
        ("::", 3),
        ("4000::", 2),
        ("8000::", 1),
    ];

    fn inward_v4() -> Vec<Ipv4Addr> {
        let mut out = Vec::new();
        for &(base, len) in EXPECTED_REFUSED_V4 {
            let first = u32::from(Ipv4Addr::from(base));
            let size = 1u64 << (32 - u32::from(len));
            for offset in [0, size / 2, size - 1] {
                out.push(Ipv4Addr::from(first + offset as u32));
            }
        }
        out
    }

    fn public_v4s() -> Vec<Ipv4Addr> {
        [
            [1, 1, 1, 1],
            [8, 8, 4, 4],
            [93, 184, 216, 34],
            [151, 101, 1, 69],
            [100, 63, 255, 255],
            [100, 128, 0, 0],
            [172, 15, 255, 255],
            [172, 32, 0, 0],
            [192, 0, 1, 0],
            [198, 20, 0, 0],
            [223, 255, 255, 255],
        ]
        .map(Ipv4Addr::from)
        .to_vec()
    }

    fn nat64(v4: Ipv4Addr) -> Ipv6Addr {
        let [a, b, c, d] = v4.octets();
        Ipv6Addr::new(
            0x64,
            0xff9b,
            0,
            0,
            0,
            0,
            u16::from_be_bytes([a, b]),
            u16::from_be_bytes([c, d]),
        )
    }

    fn six_to_four(v4: Ipv4Addr) -> Ipv6Addr {
        let [a, b, c, d] = v4.octets();
        Ipv6Addr::new(
            0x2002,
            u16::from_be_bytes([a, b]),
            u16::from_be_bytes([c, d]),
            0,
            0,
            0,
            0,
            1,
        )
    }

    /// A one-frame GIF whose screen claims `width` by `height`.
    fn gif_claiming(width: u16, height: u16) -> Vec<u8> {
        let mut out = Vec::new();
        image::codecs::gif::GifEncoder::new(&mut out)
            .encode_frame(image::Frame::new(image::RgbaImage::new(2, 2)))
            .unwrap();
        out[6..8].copy_from_slice(&width.to_le_bytes());
        out[8..10].copy_from_slice(&height.to_le_bytes());
        out
    }

    #[test]
    fn only_a_public_address_is_one_a_cover_may_come_from() {
        for v4 in inward_v4() {
            for ip in [
                IpAddr::V4(v4),
                IpAddr::V6(v4.to_ipv6_mapped()),
                IpAddr::V6(v4.to_ipv6_compatible()),
                IpAddr::V6(nat64(v4)),
                IpAddr::V6(six_to_four(v4)),
            ] {
                assert!(!is_public(ip), "{ip} ({v4})");
            }
        }
        for v4 in public_v4s() {
            for ip in [
                IpAddr::V4(v4),
                IpAddr::V6(v4.to_ipv6_mapped()),
                IpAddr::V6(nat64(v4)),
                IpAddr::V6(six_to_four(v4)),
            ] {
                assert!(is_public(ip), "{ip} ({v4})");
            }
            assert!(
                !is_public(IpAddr::V6(v4.to_ipv6_compatible())),
                "{v4} compatible"
            );
        }
        for &(first, len) in EXPECTED_REFUSED_V6 {
            let first = u128::from(Ipv6Addr::from_str(first).unwrap());
            let size = if len == 0 {
                u128::MAX
            } else {
                (1u128 << (128 - u32::from(len))) - 1
            };
            for at in [first, first + size / 2, first + size] {
                let ip = Ipv6Addr::from(at);
                if ip.to_ipv4_mapped().is_some() {
                    continue;
                }
                assert!(!is_public(IpAddr::V6(ip)), "{ip} in {len}");
            }
        }
        for refused in [
            "::",
            "::1",
            "::ffff:0:a00:1",
            "64:ff9b:1::1",
            "100::1",
            "100::ffff:ffff:ffff:ffff",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "febf:ffff::1",
            "fec0::1",
            "ff02::1",
            "ff0e::1",
            "2001::1",
            "2001:0:4136:e378:8000:63bf:3fff:fdd2",
            "2001:2::1",
            "2001:10::1",
            "2001:20::1",
            "2001:1ff::1",
            "2001:db8::1",
            "2001:db8:ffff::1",
            "3fff::1",
            "3fff:fff::1",
            "5f00::1",
            "4000::1",
            "1fff:ffff::1",
            "c000::1",
        ] {
            assert!(!is_public(IpAddr::from_str(refused).unwrap()), "{refused}");
        }
        for public in [
            "2606:4700::6810:84e5",
            "2a00:1450:4001:830::200e",
            "2001:4860:4860::8888",
            "2001:200::1",
            "2620:fe::fe",
            "3fff:1000::1",
        ] {
            assert!(is_public(IpAddr::from_str(public).unwrap()), "{public}");
        }
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
        Mock::given(path("/bomb.gif"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(gif_claiming(60_000, 60_000)))
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
                on(&server, "public.test", "/bomb.gif"),
                CoverRefusal::TooLarge,
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

    /// A server that answers one request with `response` exactly as written.
    fn answering_with(response: Vec<u8>) -> std::net::SocketAddr {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let at = listener.local_addr().unwrap();
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
        at
    }

    #[tokio::test]
    async fn a_cover_with_no_length_is_refused_once_it_passes_the_cap() {
        let mut body = png();
        body.resize(COVER_LIMIT + 1, 0);
        let mut response =
            b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n".to_vec();
        for chunk in body.chunks(64 << 10) {
            response.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
            response.extend_from_slice(chunk);
            response.extend_from_slice(b"\r\n");
        }
        response.extend_from_slice(b"0\r\n\r\n");
        let at = answering_with(response);
        let lookup = move |_: &str| Ok(vec![at.ip()]);
        let client = CoverClient::build(
            Arc::new(lookup),
            Allowance {
                http: true,
                addresses: vec![at.ip()],
            },
        )
        .unwrap();
        let images = tempfile::tempdir().unwrap();
        let url = format!("http://public.test:{}/cover.png", at.port());
        let error = client.fetch(&url, images.path()).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            EngineError::from(CoverRefusal::TooLarge).to_string()
        );
        assert_eq!(std::fs::read_dir(images.path()).unwrap().count(), 0);
    }

    const ORIGIN_PORT: &str = "BOOKMOLE_TEST_COVER_ORIGIN_PORT";

    /// The fetch [`a_proxy_in_the_environment_is_never_used`] runs in a child process with proxy
    /// variables set; run on its own, without the port it is given, it does nothing.
    #[tokio::test]
    #[ignore = "run in a child process by a_proxy_in_the_environment_is_never_used"]
    async fn fetch_with_a_proxy_in_the_environment() {
        let Ok(port) = std::env::var(ORIGIN_PORT) else {
            return;
        };
        let loopback = IpAddr::from([127, 0, 0, 1]);
        let client = CoverClient::build(
            Arc::new(move |_: &str| Ok(vec![loopback])),
            Allowance {
                http: true,
                addresses: vec![loopback],
            },
        )
        .unwrap();
        let images = tempfile::tempdir().unwrap();
        client
            .fetch(
                &format!("http://public.test:{port}/cover.png"),
                images.path(),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_proxy_in_the_environment_is_never_used() {
        let origin = MockServer::start().await;
        Mock::given(path("/cover.png"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(png()))
            .mount(&origin)
            .await;
        let proxy = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(ResponseTemplate::new(200).set_body_bytes(png()))
            .mount(&proxy)
            .await;
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "images::fetch::tests::fetch_with_a_proxy_in_the_environment",
                "--ignored",
            ])
            .env(ORIGIN_PORT, origin.address().port().to_string())
            .env_remove("NO_PROXY")
            .env_remove("no_proxy");
        for name in [
            "HTTP_PROXY",
            "http_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "ALL_PROXY",
            "all_proxy",
        ] {
            child.env(name, proxy.uri());
        }
        let status = tokio::task::spawn_blocking(move || child.status().unwrap())
            .await
            .unwrap();
        assert!(status.success());
        assert!(proxy.received_requests().await.unwrap().is_empty());
        assert_eq!(origin.received_requests().await.unwrap().len(), 1);
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
