//! Wake signals to push URLs, and the checks that keep clients from using the
//! relay to reach internal addresses.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use reqwest::Url;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::redirect::Policy;
use url::Host;

use crate::limits::{MAX_PUSH_URL_LEN, WAKE_TIMEOUT_SECS};

/// The fixed body of a wake signal.
pub const WAKE_BODY: &str = "wake";

/// Which push URLs the operator allows.
#[derive(Debug, Clone, Default)]
pub struct PushPolicy {
    /// Allowed host names in lower case. Empty means every public host.
    allowed_hosts: Vec<String>,
}

impl PushPolicy {
    /// A policy limited to the given host names. An empty list allows every
    /// public host.
    pub fn new(allowed_hosts: impl IntoIterator<Item = String>) -> Self {
        Self {
            allowed_hosts: allowed_hosts
                .into_iter()
                .map(|h| normalize_host(&h))
                .filter(|h| !h.is_empty())
                .collect(),
        }
    }

    /// Parses the comma separated list from `LZ_PUSH_ALLOWED_HOSTS`.
    pub fn from_list(list: &str) -> Self {
        Self::new(list.split(',').map(str::to_owned))
    }

    /// Checks a push URL without any network access: `https` only, port 443
    /// only, no user info, a length limit, no non-public IP literals, no
    /// `localhost`, and the host allowlist if one is set. Returns the parsed
    /// URL.
    pub fn validate(&self, url: &str) -> Result<Url, InvalidPushUrl> {
        if url.len() > MAX_PUSH_URL_LEN {
            return Err(InvalidPushUrl);
        }
        let parsed = Url::parse(url).map_err(|_| InvalidPushUrl)?;
        if parsed.scheme() != "https"
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            // The parser drops an explicit default port, so any port left is
            // not 443; other ports would let clients probe public services.
            || parsed.port().is_some()
        {
            return Err(InvalidPushUrl);
        }
        match parsed.host() {
            Some(Host::Domain(domain)) => {
                let domain = normalize_host(domain);
                if domain == "localhost" || domain.ends_with(".localhost") {
                    return Err(InvalidPushUrl);
                }
            }
            Some(Host::Ipv4(ip)) if is_public_ip(IpAddr::V4(ip)) => {}
            Some(Host::Ipv6(ip)) if is_public_ip(IpAddr::V6(ip)) => {}
            _ => return Err(InvalidPushUrl),
        }
        if !self.allowed_hosts.is_empty() {
            let host = normalize_host(parsed.host_str().unwrap_or_default());
            if !self.allowed_hosts.contains(&host) {
                return Err(InvalidPushUrl);
            }
        }
        Ok(parsed)
    }
}

fn normalize_host(host: &str) -> String {
    host.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// A push URL the relay refuses to call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidPushUrl;

/// Whether an address is publicly routable, so that a wake signal may go
/// there. Rejects loopback, private, shared (CGNAT), link-local, unique-local,
/// multicast, documentation, benchmarking and reserved ranges. For IPv6 only
/// global unicast (`2000::/3`) is allowed; IPv4-mapped, NAT64 and 6to4
/// addresses are judged by the embedded IPv4 address.
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_ipv4(v4),
        IpAddr::V6(v6) => is_public_ipv6(v6),
    }
}

fn is_public_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    let blocked = a == 0 // "this network"
        || a == 10 // private
        || (a == 100 && (64..128).contains(&b)) // shared address space (CGNAT)
        || a == 127 // loopback
        || (a == 169 && b == 254) // link-local
        || (a == 172 && (16..32).contains(&b)) // private
        || (a == 192 && b == 0 && c == 0) // IETF protocol assignments
        || (a == 192 && b == 0 && c == 2) // documentation
        || (a == 192 && b == 88 && c == 99) // 6to4 relay anycast
        || (a == 192 && b == 168) // private
        || (a == 198 && (b == 18 || b == 19)) // benchmarking
        || (a == 198 && b == 51 && c == 100) // documentation
        || (a == 203 && b == 0 && c == 113) // documentation
        || a >= 224; // multicast, reserved, broadcast
    !blocked
}

fn is_public_ipv6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_ipv4(v4);
    }
    let s = ip.segments();
    // NAT64 well-known prefix 64:ff9b::/96.
    if s[0] == 0x64 && s[1] == 0xff9b && s[2..6] == [0, 0, 0, 0] {
        return is_public_ipv4(embedded_v4(s[6], s[7]));
    }
    // Everything outside global unicast: unspecified, loopback, IPv4-compatible,
    // discard, local-use NAT64, unique-local, link-local, site-local, multicast.
    if s[0] & 0xe000 != 0x2000 {
        return false;
    }
    // 6to4 carries an IPv4 address in bits 16..48.
    if s[0] == 0x2002 {
        return is_public_ipv4(embedded_v4(s[1], s[2]));
    }
    // 2001::/23 holds IETF protocol assignments including Teredo,
    // 2001:db8::/32 and 3fff::/20 are for documentation.
    let blocked =
        (s[0] == 0x2001 && (s[1] < 0x0200 || s[1] == 0x0db8)) || (s[0] == 0x3fff && s[1] < 0x1000);
    !blocked
}

fn embedded_v4(high: u16, low: u16) -> Ipv4Addr {
    let [a, b] = high.to_be_bytes();
    let [c, d] = low.to_be_bytes();
    Ipv4Addr::new(a, b, c, d)
}

/// Sends wake signals. Implementations must not block and must not report
/// failures; tests replace the HTTP implementation with a recorder.
pub trait Waker: Send + Sync + 'static {
    /// Sends one wake signal to `url` in the background.
    fn wake(&self, url: String);
}

/// Sends wake signals over HTTPS.
pub struct HttpWaker {
    client: reqwest::Client,
    policy: PushPolicy,
}

impl HttpWaker {
    /// Builds the HTTP client: `https` only, no proxy, no redirects, a short
    /// time limit, and a resolver that only returns public addresses.
    pub fn new(policy: PushPolicy) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .https_only(true)
            .redirect(Policy::none())
            .no_proxy()
            .timeout(Duration::from_secs(WAKE_TIMEOUT_SECS))
            .connect_timeout(Duration::from_secs(WAKE_TIMEOUT_SECS))
            .pool_max_idle_per_host(0)
            .user_agent("lz-relay")
            .dns_resolver(Arc::new(PublicResolver))
            .build()?;
        Ok(Self { client, policy })
    }
}

impl Waker for HttpWaker {
    fn wake(&self, url: String) {
        // Checked again because the allowlist may have changed since the
        // reader was registered. IP literals bypass the resolver, so
        // `validate` covers them.
        let Ok(url) = self.policy.validate(&url) else {
            return;
        };
        let request = self.client.post(url).body(WAKE_BODY);
        tokio::spawn(async move {
            // Failures are neither retried nor logged.
            let _ = request.send().await;
        });
    }
}

/// Resolves host names and keeps only public addresses. Connecting to the
/// checked addresses directly avoids a second lookup that could answer
/// differently (DNS rebinding).
struct PublicResolver;

impl Resolve for PublicResolver {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((name.as_str(), 0))
                .await?
                .filter(|addr| is_public_ip(addr.ip()))
                .collect();
            if addrs.is_empty() {
                return Err("no public address".into());
            }
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

/// Discards wake signals. For tests and for operation without push.
pub struct NoWaker;

impl Waker for NoWaker {
    fn wake(&self, _url: String) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn public(ip: &str) -> bool {
        is_public_ip(ip.parse().unwrap())
    }

    #[test]
    fn ipv4_classification() {
        for ip in [
            "0.0.0.0",
            "10.1.2.3",
            "100.64.0.1",
            "100.127.255.255",
            "127.0.0.1",
            "169.254.169.254",
            "172.16.0.1",
            "172.31.255.255",
            "192.0.0.8",
            "192.0.2.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.51.100.7",
            "203.0.113.9",
            "224.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
        ] {
            assert!(!public(ip), "{ip} must not count as public");
        }
        for ip in [
            "1.1.1.1",
            "8.8.8.8",
            "100.63.255.255",
            "100.128.0.1",
            "172.32.0.1",
            "93.184.216.34",
        ] {
            assert!(public(ip), "{ip} must count as public");
        }
    }

    #[test]
    fn ipv6_classification() {
        for ip in [
            "::",
            "::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::127.0.0.1",
            "64:ff9b::a00:1",
            "64:ff9b:1::1",
            "100::1",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "fec0::1",
            "ff02::1",
            "2001::1",
            "2001:db8::1",
            "2002:7f00:1::1",
            "2002:c0a8:101::1",
            "3fff::1",
        ] {
            assert!(!public(ip), "{ip} must not count as public");
        }
        for ip in [
            "2606:4700:4700::1111",
            "2a01:4f8::1",
            "::ffff:1.1.1.1",
            "64:ff9b::808:808",
            "2002:808:808::1",
            "2001:200::1",
            "3fff:1000::1",
        ] {
            assert!(public(ip), "{ip} must count as public");
        }
    }

    #[test]
    fn url_validation() {
        let any = PushPolicy::default();
        assert!(any.validate("https://ntfy.example.org/upAbC?up=1").is_ok());
        assert!(any.validate("https://[2606:4700:4700::1111]/x").is_ok());
        assert!(any.validate("https://ntfy.example.org:443/up").is_ok());
        assert!(any.validate("https://[2606:4700:4700::1111]:443/x").is_ok());
        assert!(any.validate("https://ntfy.example.org:8443/up").is_err());
        assert!(any.validate("https://ntfy.example.org:80/up").is_err());
        assert!(any.validate("https://ntfy.example.org:22/up").is_err());
        assert!(any.validate("https://1.1.1.1:6379/up").is_err());
        assert!(
            any.validate("https://[2606:4700:4700::1111]:8080/x")
                .is_err()
        );
        assert!(any.validate("http://ntfy.example.org/up").is_err());
        assert!(any.validate("ftp://ntfy.example.org/up").is_err());
        assert!(any.validate("not a url").is_err());
        assert!(any.validate("https://user:pw@ntfy.example.org/up").is_err());
        assert!(any.validate("https://127.0.0.1/up").is_err());
        assert!(any.validate("https://10.0.0.5:8443/up").is_err());
        assert!(any.validate("https://[::1]/up").is_err());
        assert!(any.validate("https://[fd00::1]/up").is_err());
        assert!(any.validate("https://[::ffff:192.168.0.1]/up").is_err());
        assert!(any.validate("https://localhost/up").is_err());
        assert!(any.validate("https://foo.localhost./up").is_err());
        // Decimal and hex forms of an IPv4 address are normalized by the parser.
        assert!(any.validate("https://2130706433/up").is_err());
        assert!(any.validate("https://0x7f.1/up").is_err());
        let long = format!("https://ntfy.example.org/{}", "a".repeat(MAX_PUSH_URL_LEN));
        assert!(any.validate(&long).is_err());

        let listed = PushPolicy::from_list(" ntfy.example.org , Push.Example.NET ");
        assert!(listed.validate("https://ntfy.example.org/up").is_ok());
        assert!(listed.validate("https://NTFY.example.org./up").is_ok());
        assert!(listed.validate("https://push.example.net/up").is_ok());
        assert!(listed.validate("https://evil.example.com/up").is_err());
        assert!(
            PushPolicy::from_list("")
                .validate("https://evil.example.com/up")
                .is_ok()
        );
    }
}
