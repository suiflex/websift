//! Shared robots gate used by every operation that fetches pages it did not author.
//!
//! Crawling and research must answer the same question — may this client fetch this URL, and how
//! long must it wait first — so the cache, the unavailable-origin memory, and the per-host
//! schedule live here instead of being restated per caller. An origin whose `robots.txt` cannot
//! be read is denied rather than assumed permissive. The one exception is an origin that answers
//! that no rules exist: 404 and 410 mean the site published nothing to obey, which RFC 9309
//! treats as full permission.

use std::{
    collections::{HashMap, HashSet},
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use url::Url;

use reqwest::StatusCode;

use crate::{
    fetch::{FetchClient, FetchError},
    policy::{RobotsCache, RobotsRules},
};

/// Product token this client matches in robots rules.
pub const ROBOTS_USER_AGENT: &str = "websift";
const ROBOTS_MAX_BYTES: usize = 512 * 1024;
const ROBOTS_CACHE_TTL: Duration = Duration::from_secs(300);
const ROBOTS_CACHE_ENTRIES: usize = 256;

/// Why a `robots.txt` document could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RobotsFetchError {
    /// The origin answered that no rules exist, so there is nothing to obey.
    Absent,
    /// Unreachable, refused, or unreadable. The origin stays denied.
    Unreadable,
}

/// Why `rules_for` could not return rules. `Unreadable` covers the fetch
/// failures; `Poisoned` means a mutex was left held by a panicked thread,
/// which denies the origin the same way an unreadable document does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RobotsError {
    Unreadable,
    Poisoned,
}

type RobotsFetchFuture = Pin<Box<dyn Future<Output = Result<String, RobotsFetchError>> + Send>>;
/// Fetches one `robots.txt` document. Injectable so tests never touch the network.
pub type RobotsFetcher = Arc<dyn Fn(String) -> RobotsFetchFuture + Send + Sync>;

/// Outcome of one robots check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RobotsDecision {
    /// Fetching is allowed after honoring `delay`.
    Allowed { delay: Option<Duration> },
    /// The origin's rules forbid this path for this user agent.
    Disallowed,
    /// The rules could not be read, so the fetch is denied.
    Unavailable,
}

/// Bounded robots cache, denial memory, and per-host request schedule.
#[derive(Clone)]
pub struct RobotsGate {
    cache: Arc<Mutex<RobotsCache>>,
    unavailable: Arc<Mutex<HashSet<String>>>,
    next_host_request: Arc<Mutex<HashMap<String, Instant>>>,
    fetcher: RobotsFetcher,
}

impl std::fmt::Debug for RobotsGate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("RobotsGate").finish_non_exhaustive()
    }
}

impl RobotsGate {
    /// Build a gate that reads `robots.txt` with the same bounded HTTP client as page fetches.
    #[must_use]
    pub fn new(fetch: FetchClient) -> Self {
        Self::with_fetcher(Arc::new(move |url| {
            let fetch = fetch.clone();
            Box::pin(async move {
                let result = fetch.get(&url).await.map_err(|error| match error {
                    // Only "there is nothing here" counts as absent. This is stricter than RFC
                    // 9309, which lets the whole 4xx range mean full permission: 401 and 403 say
                    // access is restricted and 429 says we are being rate limited, so reading
                    // either as permission inverts what the origin asked for.
                    FetchError::Status(status)
                        if status == StatusCode::NOT_FOUND || status == StatusCode::GONE =>
                    {
                        RobotsFetchError::Absent
                    }
                    _ => RobotsFetchError::Unreadable,
                })?;
                String::from_utf8(result.body).map_err(|_| RobotsFetchError::Unreadable)
            })
        }))
    }

    /// Build a gate over an injected document fetcher.
    #[must_use]
    pub fn with_fetcher(fetcher: RobotsFetcher) -> Self {
        Self {
            cache: Arc::new(Mutex::new(RobotsCache::new(
                ROBOTS_CACHE_TTL,
                ROBOTS_CACHE_ENTRIES,
            ))),
            unavailable: Arc::new(Mutex::new(HashSet::new())),
            next_host_request: Arc::new(Mutex::new(HashMap::new())),
            fetcher,
        }
    }

    /// Read the rules for one origin, using the cache and the denial memory first.
    ///
    /// # Errors
    ///
    /// Returns `Err(RobotsError::Unreadable)` when the document cannot be read; callers must
    /// treat that as a denial.
    pub async fn rules_for(&self, origin: &str) -> Result<RobotsRules, RobotsError> {
        if let Some(rules) = self
            .cache
            .lock()
            .map_err(|_| RobotsError::Poisoned)?
            .get(origin)
            .cloned()
        {
            return Ok(rules);
        }
        if self
            .unavailable
            .lock()
            .map_err(|_| RobotsError::Poisoned)?
            .contains(origin)
        {
            return Err(RobotsError::Unreadable);
        }
        let robots_url = format!("{origin}/robots.txt");
        let document = match (self.fetcher)(robots_url).await {
            Ok(document) => document,
            // No rules published means nothing to obey. The permissive result is cached like any
            // other document so one 404 does not become a refetch per candidate URL.
            Err(RobotsFetchError::Absent) => String::new(),
            Err(RobotsFetchError::Unreadable) => {
                self.unavailable
                    .lock()
                    .map_err(|_| RobotsError::Poisoned)?
                    .insert(origin.to_owned());
                return Err(RobotsError::Unreadable);
            }
        };
        let rules = RobotsRules::parse(&document, ROBOTS_MAX_BYTES);
        self.cache
            .lock()
            .map_err(|_| RobotsError::Poisoned)?
            .insert(origin.to_owned(), rules.clone());
        Ok(rules)
    }

    /// Decide whether one absolute URL may be fetched.
    pub async fn check(&self, url: &Url) -> RobotsDecision {
        let Some(origin) = origin_key(url) else {
            return RobotsDecision::Unavailable;
        };
        let Ok(rules) = self.rules_for(&origin).await else {
            return RobotsDecision::Unavailable;
        };
        if rules.allowed(&path_and_query(url), ROBOTS_USER_AGENT) {
            RobotsDecision::Allowed {
                delay: rules.crawl_delay(ROBOTS_USER_AGENT),
            }
        } else {
            RobotsDecision::Disallowed
        }
    }

    /// Sleep until this client may issue the next request to `origin`.
    pub async fn wait_for_host(&self, origin: &str, delay: Option<Duration>) {
        let Some(delay) = delay.filter(|value| !value.is_zero()) else {
            return;
        };
        let wait = {
            let Ok(mut schedule) = self.next_host_request.lock() else {
                return;
            };
            let now = Instant::now();
            let at = schedule.get(origin).copied().unwrap_or(now);
            let wait = at.saturating_duration_since(now);
            schedule.insert(origin.to_owned(), now + wait + delay);
            wait
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }
}

/// Scheme, host, and explicit port, which is the key robots rules apply to.
#[must_use]
pub fn origin_key(url: &Url) -> Option<String> {
    Some(format!(
        "{}://{}{}",
        url.scheme(),
        url.host_str()?,
        url.port().map_or(String::new(), |port| format!(":{port}"))
    ))
}

/// Path plus query, which is what robots patterns match against.
#[must_use]
pub fn path_and_query(url: &Url) -> String {
    let mut value = url.path().to_owned();
    if let Some(query) = url.query() {
        value.push('?');
        value.push_str(query);
    }
    value
}

#[cfg(test)]
mod tests {
    use super::{RobotsDecision, RobotsFetchError, RobotsGate, origin_key, path_and_query};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use url::Url;

    fn gate_failing(error: RobotsFetchError, calls: Arc<AtomicUsize>) -> RobotsGate {
        RobotsGate::with_fetcher(Arc::new(move |_| {
            let calls = Arc::clone(&calls);
            Box::pin(async move {
                calls.fetch_add(1, Ordering::Relaxed);
                Err(error)
            })
        }))
    }

    fn gate_returning(document: &'static str, calls: Arc<AtomicUsize>) -> RobotsGate {
        RobotsGate::with_fetcher(Arc::new(move |_| {
            let calls = Arc::clone(&calls);
            Box::pin(async move {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok(document.to_owned())
            })
        }))
    }

    #[tokio::test]
    async fn disallowed_paths_are_denied_and_rules_are_cached_per_origin() {
        let calls = Arc::new(AtomicUsize::new(0));
        let gate = gate_returning(
            "User-agent: *\nDisallow: /private\nCrawl-delay: 0\n",
            Arc::clone(&calls),
        );
        let denied = Url::parse("https://example.test/private/page").unwrap();
        let allowed = Url::parse("https://example.test/public/page").unwrap();
        assert_eq!(gate.check(&denied).await, RobotsDecision::Disallowed);
        assert!(matches!(
            gate.check(&allowed).await,
            RobotsDecision::Allowed { .. }
        ));
        // The second check reuses the cached document instead of refetching it.
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn absent_rules_allow_instead_of_denying_the_whole_origin() {
        let calls = Arc::new(AtomicUsize::new(0));
        let gate = gate_failing(RobotsFetchError::Absent, Arc::clone(&calls));
        let url = Url::parse("https://example.test/anything").unwrap();
        assert!(matches!(
            gate.check(&url).await,
            RobotsDecision::Allowed { .. }
        ));
        // The permissive answer is cached, so a missing document is fetched once per origin.
        assert!(matches!(
            gate.check(&url).await,
            RobotsDecision::Allowed { .. }
        ));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn unreadable_rules_deny_instead_of_assuming_permission() {
        let calls = Arc::new(AtomicUsize::new(0));
        let gate = gate_failing(RobotsFetchError::Unreadable, Arc::clone(&calls));
        let url = Url::parse("https://example.test/page").unwrap();
        assert_eq!(gate.check(&url).await, RobotsDecision::Unavailable);
        // A failed origin is remembered, so it is not refetched for every candidate URL.
        assert_eq!(gate.check(&url).await, RobotsDecision::Unavailable);
    }

    #[test]
    fn origin_and_path_keys_match_what_robots_rules_address() {
        let url = Url::parse("https://example.test:8443/a/b?c=d").unwrap();
        assert_eq!(
            origin_key(&url).unwrap(),
            "https://example.test:8443".to_owned()
        );
        assert_eq!(path_and_query(&url), "/a/b?c=d");
    }
}
