//! URL normalization and public-destination policy primitives.

use std::{
    future::Future,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    pin::Pin,
    time::Duration,
};
use tokio::time::timeout;
use url::{Host, Url};

/// Reqwest DNS resolver that validates every resolved address before connect.
#[derive(Clone)]
pub struct ValidatingDnsResolver {
    resolver: std::sync::Arc<dyn DnsResolver>,
    resolve_timeout: Duration,
    max_addresses: usize,
}

impl ValidatingDnsResolver {
    #[must_use]
    pub fn new(
        resolver: std::sync::Arc<dyn DnsResolver>,
        resolve_timeout: Duration,
        max_addresses: usize,
    ) -> Self {
        Self {
            resolver,
            resolve_timeout,
            max_addresses,
        }
    }
}

impl reqwest::dns::Resolve for ValidatingDnsResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let resolver = self.resolver.clone();
        let timeout_duration = self.resolve_timeout;
        let max_addresses = self.max_addresses;
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let addresses = timeout(timeout_duration, resolver.resolve(&host, 80))
                .await
                .map_err(|_| "DNS resolution timed out".to_owned())
                .and_then(|result| result.map_err(|error| error.to_string()))?;
            if timeout_duration.is_zero() || max_addresses == 0 {
                return Err("invalid DNS resolver bounds".into());
            }
            if addresses.is_empty() || addresses.len() > max_addresses {
                return Err("DNS address count exceeded policy".into());
            }
            if addresses
                .iter()
                .any(|address| is_private_or_reserved(*address))
            {
                return Err("DNS resolved to a private or reserved address".into());
            }
            let addrs: reqwest::dns::Addrs = Box::new(
                addresses
                    .into_iter()
                    .map(|address| std::net::SocketAddr::new(address, 0)),
            );
            Ok(addrs)
        })
    }
}

/// A normalized public HTTP(S) URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicUrl(String);

impl PublicUrl {
    /// Validate and normalize a URL without performing DNS resolution.
    ///
    /// Only HTTP port 80 and HTTPS port 443 are accepted, including when the
    /// port is written explicitly.
    ///
    /// # Errors
    ///
    /// Returns [`UrlError`] when the URL is malformed, uses an unsupported
    /// scheme or authority, targets an unsafe host, or uses an invalid port.
    pub fn parse(input: &str) -> Result<Self, UrlError> {
        let value = input.trim();
        let mut url = Url::parse(value).map_err(|error| match error {
            url::ParseError::RelativeUrlWithoutBase | url::ParseError::EmptyHost => {
                UrlError::InvalidHost
            }
            url::ParseError::InvalidPort | url::ParseError::InvalidIpv4Address => {
                UrlError::InvalidPort
            }
            _ => UrlError::InvalidScheme,
        })?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(UrlError::InvalidScheme);
        }
        if url.username() != "" || url.password().is_some() {
            return Err(UrlError::UnsafeAuthority);
        }
        let host = url.host().ok_or(UrlError::InvalidHost)?;
        match host {
            Host::Domain(value) if is_unsafe_host(value) => return Err(UrlError::UnsafeHost),
            Host::Ipv4(address) if is_private_or_reserved(IpAddr::V4(address)) => {
                return Err(UrlError::PrivateAddress);
            }
            Host::Ipv6(address) if is_private_or_reserved(IpAddr::V6(address)) => {
                return Err(UrlError::PrivateAddress);
            }
            _ => {}
        }
        if let Some(port) = url.port() {
            let expected = if url.scheme() == "http" { 80 } else { 443 };
            if port == 0 || port != expected {
                return Err(UrlError::InvalidPort);
            }
            if url.set_port(None).is_err() {
                return Err(UrlError::InvalidPort);
            }
        }
        url.set_fragment(None);
        Ok(Self(url.to_string()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Resolve and validate every address, with a caller-supplied resolver seam.
    ///
    /// # Errors
    ///
    /// Returns [`DestinationError`] when bounds, ports, DNS resolution, address
    /// count, or resolved-address safety checks fail.
    pub async fn resolve_and_validate<R: DnsResolver + ?Sized>(
        &self,
        resolver: &R,
        resolve_timeout: Duration,
        max_addresses: usize,
    ) -> Result<Vec<IpAddr>, DestinationError> {
        if resolve_timeout.is_zero() || max_addresses == 0 {
            return Err(DestinationError::InvalidBounds);
        }
        let url = Url::parse(&self.0).map_err(|_| DestinationError::InvalidUrl)?;
        let port = effective_port(&url)?;
        let addresses = match url.host().ok_or(DestinationError::InvalidUrl)? {
            Host::Ipv4(ip) => vec![IpAddr::V4(ip)],
            Host::Ipv6(ip) => vec![IpAddr::V6(ip)],
            Host::Domain(host) => timeout(resolve_timeout, resolver.resolve(host, port))
                .await
                .map_err(|_| DestinationError::ResolutionTimeout)?
                .map_err(|_| DestinationError::ResolutionFailed)?,
        };
        if addresses.is_empty() || addresses.len() > max_addresses {
            return Err(DestinationError::TooManyAddresses);
        }
        if addresses.iter().any(|ip| is_private_or_reserved(*ip)) {
            return Err(DestinationError::PrivateAddress);
        }
        Ok(addresses)
    }
}

fn effective_port(url: &Url) -> Result<u16, DestinationError> {
    let port = url
        .port_or_known_default()
        .ok_or(DestinationError::InvalidPort)?;
    let expected = if url.scheme() == "http" { 80 } else { 443 };
    if port != expected {
        return Err(DestinationError::InvalidPort);
    }
    Ok(port)
}

/// Async DNS resolver seam used by [`PublicUrl::resolve_and_validate`].
pub trait DnsResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        host: &'a str,
        port: u16,
    ) -> Pin<Box<dyn Future<Output = std::io::Result<Vec<IpAddr>>> + Send + 'a>>;
}

/// Production resolver using the operating system resolver.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemDnsResolver;
impl DnsResolver for SystemDnsResolver {
    fn resolve<'a>(
        &'a self,
        host: &'a str,
        port: u16,
    ) -> Pin<Box<dyn Future<Output = std::io::Result<Vec<IpAddr>>> + Send + 'a>> {
        Box::pin(async move {
            Ok(tokio::net::lookup_host((host, port))
                .await?
                .map(|socket| socket.ip())
                .collect())
        })
    }
}

/// Redirect policy that bounds hops, ports, and HTTPS downgrade attempts.
#[derive(Debug, Clone, Copy)]
pub struct RedirectGuard {
    max_redirects: usize,
    redirects: usize,
    https_origin: bool,
}
impl RedirectGuard {
    /// Create a redirect guard for an HTTP(S) origin.
    ///
    /// # Errors
    ///
    /// Returns [`DestinationError`] if the origin cannot be parsed or does not
    /// use its explicitly allowed port.
    pub fn new(origin: &PublicUrl, max_redirects: usize) -> Result<Self, DestinationError> {
        let url = Url::parse(origin.as_str()).map_err(|_| DestinationError::InvalidUrl)?;
        effective_port(&url)?;
        Ok(Self {
            max_redirects,
            redirects: 0,
            https_origin: url.scheme() == "https",
        })
    }
    /// Validate and consume one redirect hop.
    ///
    /// # Errors
    ///
    /// Returns [`RedirectError`] when the hop limit is reached, the destination
    /// is malformed or uses an invalid port, or HTTPS would be downgraded.
    pub fn check(&mut self, next: &PublicUrl) -> Result<(), RedirectError> {
        if self.redirects >= self.max_redirects {
            return Err(RedirectError::LimitExceeded);
        }
        let url = Url::parse(next.as_str()).map_err(|_| RedirectError::InvalidDestination)?;
        effective_port(&url).map_err(|_| RedirectError::InvalidDestination)?;
        if self.https_origin && url.scheme() != "https" {
            return Err(RedirectError::Downgrade);
        }
        self.redirects += 1;
        Ok(())
    }

    /// Resolve a redirect target before consuming its hop budget.
    ///
    /// # Errors
    ///
    /// Returns [`DestinationError`] if DNS or address policy validation fails,
    /// otherwise [`RedirectError`] for an invalid or unsafe redirect hop.
    pub async fn check_and_resolve<R: DnsResolver + ?Sized>(
        &mut self,
        next: &PublicUrl,
        resolver: &R,
        resolve_timeout: Duration,
        max_addresses: usize,
    ) -> Result<Vec<IpAddr>, RedirectDestinationError> {
        let addresses = next
            .resolve_and_validate(resolver, resolve_timeout, max_addresses)
            .await
            .map_err(RedirectDestinationError::Destination)?;
        self.check(next)
            .map_err(RedirectDestinationError::Redirect)?;
        Ok(addresses)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectDestinationError {
    Destination(DestinationError),
    Redirect(RedirectError),
}

fn is_unsafe_host(host: &str) -> bool {
    let lower = host.to_ascii_lowercase();
    let lower = lower.strip_suffix('.').unwrap_or(&lower);
    lower == "localhost"
        || lower.ends_with(".localhost")
        || lower
            .rsplit_once('.')
            .is_some_and(|(_, suffix)| suffix == "local")
        || host.contains('%')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlError {
    InvalidScheme,
    InvalidHost,
    UnsafeHost,
    UnsafeAuthority,
    InvalidPort,
    PrivateAddress,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DestinationError {
    InvalidUrl,
    InvalidPort,
    InvalidBounds,
    ResolutionTimeout,
    ResolutionFailed,
    TooManyAddresses,
    PrivateAddress,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectError {
    LimitExceeded,
    Downgrade,
    InvalidDestination,
}

fn is_private_or_reserved(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(value) => {
            value.is_private()
                || value.is_loopback()
                || value.is_link_local()
                || value.is_unspecified()
                || value.is_broadcast()
                || value.is_multicast()
                || value.octets()[0] == 100 && (64..=127).contains(&value.octets()[1])
                || value.octets()[0] == 169 && value.octets()[1] == 254
                || value.octets()[0] == 0
                || value.octets()[0] >= 240
                || value.octets()[0] == 192 && value.octets()[1] == 0
                || value.octets()[0] == 198 && (value.octets()[1] == 18 || value.octets()[1] == 19)
                || value.octets()[0] == 203 && value.octets()[1] == 0 && value.octets()[2] == 113
                || value == Ipv4Addr::new(169, 254, 169, 254)
                || value == Ipv4Addr::new(100, 100, 100, 200)
        }
        IpAddr::V6(value) => {
            value
                .to_ipv4()
                .is_some_and(|address| is_private_or_reserved(IpAddr::V4(address)))
                || value.is_loopback()
                || value.is_unspecified()
                || value.is_multicast()
                || is_unique_local(value)
                || (value.segments()[0] & 0xffc0) == 0xfe80
                || value.segments()[0] == 0x2001 && value.segments()[1] == 0x0db8
        }
    }
}
fn is_unique_local(value: Ipv6Addr) -> bool {
    (value.segments()[0] & 0xfe00) == 0xfc00
}

/// Parsed robots.txt directives for one origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RobotsRules {
    groups: Vec<RobotsGroup>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RobotsGroup {
    agents: Vec<String>,
    rules: Vec<RobotsRule>,
    crawl_delay: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RobotsRule {
    allow: bool,
    pattern: String,
}

impl RobotsRules {
    /// Parse a bounded robots document. Malformed directives are ignored.
    #[must_use]
    pub fn parse(document: &str, max_bytes: usize) -> Self {
        let mut groups = Vec::new();
        let mut agents = Vec::new();
        let mut rules = Vec::new();
        let mut delay = None;
        for line in document
            .bytes()
            .take(max_bytes)
            .collect::<Vec<_>>()
            .split(|b| *b == b'\n')
        {
            let Ok(line) = std::str::from_utf8(line) else {
                continue;
            };
            let line = line.split('#').next().unwrap_or("").trim();
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();
            if key == "user-agent" {
                if !agents.is_empty() && (!rules.is_empty() || delay.is_some()) {
                    groups.push(RobotsGroup {
                        agents,
                        rules,
                        crawl_delay: delay,
                    });
                    agents = Vec::new();
                    rules = Vec::new();
                    delay = None;
                }
                if !value.is_empty() {
                    agents.push(value.to_ascii_lowercase());
                }
            } else if key == "allow" || key == "disallow" {
                if !agents.is_empty() && !value.is_empty() {
                    rules.push(RobotsRule {
                        allow: key == "allow",
                        pattern: value.to_owned(),
                    });
                }
            } else if key == "crawl-delay"
                && !agents.is_empty()
                && let Ok(seconds) = value.parse::<f64>()
                && seconds.is_finite()
                && (0.0..=86_400.0).contains(&seconds)
            {
                delay = Some(Duration::from_secs_f64(seconds));
            }
        }
        if !agents.is_empty() {
            groups.push(RobotsGroup {
                agents,
                rules,
                crawl_delay: delay,
            });
        }
        Self { groups }
    }

    /// Check a path using the most specific matching user-agent group.
    #[must_use]
    pub fn allowed(&self, path_and_query: &str, user_agent: &str) -> bool {
        let groups = self.groups.iter().filter(|group| {
            group
                .agents
                .iter()
                .any(|agent| agent == "*" || user_agent.to_ascii_lowercase().contains(agent))
        });
        let group =
            groups.max_by_key(|group| i32::from(group.agents.iter().any(|agent| agent != "*")));
        let Some(group) = group else { return true };
        group
            .rules
            .iter()
            .filter(|rule| robots_match(path_and_query, &rule.pattern))
            .max_by_key(|rule| rule.pattern.len())
            .is_none_or(|rule| rule.allow)
    }

    /// Return the crawl delay from the selected user-agent group.
    #[must_use]
    pub fn crawl_delay(&self, user_agent: &str) -> Option<Duration> {
        self.groups
            .iter()
            .filter(|group| {
                group
                    .agents
                    .iter()
                    .any(|agent| agent == "*" || user_agent.to_ascii_lowercase().contains(agent))
            })
            .max_by_key(|group| i32::from(group.agents.iter().any(|agent| agent != "*")))
            .and_then(|group| group.crawl_delay)
    }
}

fn robots_match(path: &str, pattern: &str) -> bool {
    let end = pattern.ends_with('$');
    let pattern = pattern.strip_suffix('$').unwrap_or(pattern).as_bytes();
    let path = path.as_bytes();
    let mut pattern_index = 0;
    let mut path_index = 0;
    let mut star_index = None;
    let mut star_path_index = 0;

    while path_index < path.len() {
        if pattern_index == pattern.len() {
            return !end;
        }
        if pattern[pattern_index] != b'*' {
            if pattern[pattern_index] == path[path_index] {
                pattern_index += 1;
                path_index += 1;
                continue;
            }
        } else if pattern_index < pattern.len() {
            star_index = Some(pattern_index);
            pattern_index += 1;
            star_path_index = path_index;
            continue;
        }

        if let Some(star) = star_index {
            pattern_index = star + 1;
            star_path_index += 1;
            path_index = star_path_index;
        } else {
            return false;
        }
    }

    while pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
        pattern_index += 1;
    }
    pattern_index == pattern.len()
}

/// Bounded in-memory robots cache. Callers own fetching and can apply TTL semantics.
#[derive(Debug)]
pub struct RobotsCache {
    entries: std::collections::HashMap<String, (std::time::Instant, RobotsRules)>,
    ttl: Duration,
    max_entries: usize,
}

impl RobotsCache {
    #[must_use]
    pub fn new(ttl: Duration, max_entries: usize) -> Self {
        Self {
            entries: std::collections::HashMap::new(),
            ttl,
            max_entries,
        }
    }
    pub fn get(&mut self, origin: &str) -> Option<&RobotsRules> {
        let fresh = self
            .entries
            .get(origin)
            .is_some_and(|(at, _)| at.elapsed() <= self.ttl);
        if !fresh {
            self.entries.remove(origin);
        }
        self.entries.get(origin).map(|(_, rules)| rules)
    }
    pub fn insert(&mut self, origin: impl Into<String>, rules: RobotsRules) {
        if self.max_entries == 0 {
            return;
        }
        if self.entries.len() >= self.max_entries
            && let Some(key) = self.entries.keys().next().cloned()
        {
            self.entries.remove(&key);
        }
        self.entries
            .insert(origin.into(), (std::time::Instant::now(), rules));
    }
}

/// Reject URLs that create unbounded query fan-out.
#[must_use]
pub fn query_is_bounded(url: &Url) -> bool {
    let pairs: Vec<_> = url.query_pairs().collect();
    pairs.len() <= 8 && pairs.iter().all(|(key, _)| !key.is_empty()) && {
        let mut keys = std::collections::HashSet::new();
        pairs
            .iter()
            .all(|(key, _)| keys.insert(key.as_ref().to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Stub(Vec<IpAddr>);
    impl DnsResolver for Stub {
        fn resolve<'a>(
            &'a self,
            _: &'a str,
            _: u16,
        ) -> Pin<Box<dyn Future<Output = std::io::Result<Vec<IpAddr>>> + Send + 'a>> {
            Box::pin(async { Ok(self.0.clone()) })
        }
    }

    #[test]
    fn robots_rules_and_cache_are_bounded() {
        let rules = RobotsRules::parse(
            "User-agent: *\nDisallow: /private\nAllow: /private/public$\nCrawl-delay: 2\n",
            1024,
        );
        assert!(!rules.allowed("/private/x", "websift"));
        assert!(rules.allowed("/private/public", "websift"));
        assert_eq!(rules.crawl_delay("websift"), Some(Duration::from_secs(2)));
        let mut cache = RobotsCache::new(Duration::from_secs(60), 1);
        cache.insert("https://example.com", rules);
        assert!(cache.get("https://example.com").is_some());
        cache.insert("https://other.example", RobotsRules::parse("", 10));
        assert!(cache.get("https://example.com").is_none());
    }

    #[test]
    fn accepts_and_normalizes_urls() {
        assert_eq!(
            PublicUrl::parse("HTTPS://Example.com:443/path#secret")
                .unwrap()
                .as_str(),
            "https://example.com/path"
        );
        assert_eq!(
            PublicUrl::parse("http://example.com:80").unwrap().as_str(),
            "http://example.com/"
        );
        assert!(PublicUrl::parse("file:///tmp/a").is_err());
    }
    #[test]
    fn rejects_literal_unsafe_hosts() {
        for value in [
            "127.0.0.1",
            "::1",
            "10.0.0.1",
            "224.0.0.1",
            "169.254.169.254",
            "fc00::1",
        ] {
            let input = if value.contains(':') {
                format!("http://[{value}]")
            } else {
                format!("http://{value}")
            };
            assert!(PublicUrl::parse(&input).is_err());
        }
        assert_eq!(
            PublicUrl::parse("http://localhost").unwrap_err(),
            UrlError::UnsafeHost
        );
    }
    #[tokio::test]
    async fn resolver_rejects_private_and_bounds_addresses() {
        let url = PublicUrl::parse("https://example.com").unwrap();
        assert_eq!(
            url.resolve_and_validate(
                &Stub(vec!["192.168.1.1".parse().unwrap()]),
                Duration::from_secs(1),
                4
            )
            .await
            .unwrap_err(),
            DestinationError::PrivateAddress
        );
        assert_eq!(
            url.resolve_and_validate(&Stub(vec![]), Duration::from_secs(1), 4)
                .await
                .unwrap_err(),
            DestinationError::TooManyAddresses
        );
    }
    #[test]
    fn redirects_bound_hops_and_downgrades() {
        let origin = PublicUrl::parse("https://example.com").unwrap();
        let mut guard = RedirectGuard::new(&origin, 1).unwrap();
        assert_eq!(
            guard.check(&PublicUrl::parse("http://example.com").unwrap()),
            Err(RedirectError::Downgrade)
        );
        assert!(
            guard
                .check(&PublicUrl::parse("https://example.org").unwrap())
                .is_ok()
        );
        assert_eq!(
            guard.check(&PublicUrl::parse("https://example.net").unwrap()),
            Err(RedirectError::LimitExceeded)
        );
    }
}
