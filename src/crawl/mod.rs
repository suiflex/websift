//! Bounded URL discovery primitives.

use std::{
    collections::{BTreeSet, HashSet},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use url::Url;

use crate::{
    fetch::{
        FetchClient,
        extract::{self, ExtractionOptions},
    },
    robots::{RobotsDecision, RobotsGate, origin_key},
    storage::{StorageError, Store},
};

const MAX_URL_CHARS: usize = 2_048;
static NEXT_JOB_ID: AtomicU64 = AtomicU64::new(1);

/// Limits applied to one crawl job.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct CrawlBudgets {
    pub max_pages: usize,
    pub max_depth: usize,
    pub max_duration: Duration,
    pub concurrency: usize,
}

impl Default for CrawlBudgets {
    fn default() -> Self {
        Self {
            max_pages: 100,
            max_depth: 3,
            max_duration: Duration::from_secs(60),
            concurrency: 4,
        }
    }
}

/// Input for a crawl job.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CrawlRequest {
    pub seed_url: String,
    pub map: MapOptions,
    pub budgets: CrawlBudgets,
}

/// Stable lifecycle states exposed by the service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum CrawlState {
    Queued,
    Running,
    Completed,
    Cancelled,
    Failed,
}

/// Transport-independent job status.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub struct CrawlStatus {
    pub id: String,
    pub state: CrawlState,
    pub pages: usize,
    pub results: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum CrawlError {
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),
    #[error("invalid crawl request: {0}")]
    InvalidRequest(String),
    #[error("job not found: {0}")]
    NotFound(String),
}

/// Synchronous bounded crawl service; callers may run `run` on a worker task.
pub struct CrawlService<'a> {
    store: &'a Store,
    fetch: FetchClient,
    profile: String,
    robots: RobotsGate,
}

#[allow(clippy::missing_errors_doc)]
impl<'a> CrawlService<'a> {
    pub fn new(store: &'a Store, fetch: FetchClient, profile: impl Into<String>) -> Self {
        let robots = RobotsGate::new(fetch.clone());
        Self {
            store,
            fetch,
            profile: profile.into(),
            robots,
        }
    }

    #[cfg(test)]
    fn with_robots_fetcher(
        store: &'a Store,
        fetch: FetchClient,
        profile: impl Into<String>,
        robots_fetcher: crate::robots::RobotsFetcher,
    ) -> Self {
        Self {
            store,
            fetch,
            profile: profile.into(),
            robots: RobotsGate::with_fetcher(robots_fetcher),
        }
    }

    pub fn start(&self, request: &CrawlRequest) -> Result<String, CrawlError> {
        if request.seed_url.len() > MAX_URL_CHARS
            || Url::parse(&request.seed_url)
                .map_or(true, |u| !matches!(u.scheme(), "http" | "https"))
        {
            return Err(CrawlError::InvalidRequest(
                "seed_url must be an HTTP(S) URL".into(),
            ));
        }
        if request.budgets.max_pages == 0
            || request.budgets.max_duration.is_zero()
            || request.budgets.concurrency == 0
        {
            return Err(CrawlError::InvalidRequest(
                "budgets must be non-zero".into(),
            ));
        }
        let id = format!("crawl-{}", NEXT_JOB_ID.fetch_add(1, Ordering::Relaxed));
        let request_json = serde_json::to_string(request)
            .map_err(|e| CrawlError::InvalidRequest(e.to_string()))?;
        self.store.crawl_jobs(&self.profile).create(
            &id,
            &request_json,
            None,
            &chrono::Utc::now().to_rfc3339(),
        )?;
        self.store.crawl_urls(&self.profile).add(
            &format!("{id}-seed"),
            &id,
            &request.seed_url,
            0,
        )?;
        Ok(id)
    }

    pub fn status(&self, id: &str) -> Result<CrawlStatus, CrawlError> {
        let state = self
            .store
            .crawl_jobs(&self.profile)
            .get_state(id)?
            .ok_or_else(|| CrawlError::NotFound(id.into()))?;
        let state = parse_state(&state);
        let pages = usize::try_from(self.store.documents(&self.profile).count_for_job(id)?)
            .unwrap_or(usize::MAX);
        let results = self.store.documents(&self.profile).list_for_job(id)?;
        Ok(CrawlStatus {
            id: id.into(),
            state,
            pages,
            results,
        })
    }

    pub fn cancel(&self, id: &str) -> Result<bool, CrawlError> {
        let changed = self.store.crawl_jobs(&self.profile).set_state(
            id,
            "cancelled",
            Some("cancelled by caller"),
            &chrono::Utc::now().to_rfc3339(),
        )?;
        if changed {
            return Ok(true);
        }
        match self.store.crawl_jobs(&self.profile).get_state(id)? {
            Some(state) if state == "cancelled" => Ok(true),
            Some(_) => Ok(false),
            None => Err(CrawlError::NotFound(id.into())),
        }
    }

    pub fn list(&self) -> Result<Vec<CrawlStatus>, CrawlError> {
        self.store
            .crawl_jobs(&self.profile)
            .list()?
            .into_iter()
            .map(|(id, _)| self.status(&id))
            .collect()
    }

    #[allow(clippy::too_many_lines)]
    pub async fn run(&self, id: &str, request: &CrawlRequest) -> Result<CrawlStatus, CrawlError> {
        self.store.crawl_jobs(&self.profile).set_state(
            id,
            "running",
            None,
            &chrono::Utc::now().to_rfc3339(),
        )?;
        let started = Instant::now();
        let mut queue = vec![(format!("{id}-seed"), request.seed_url.clone(), 0usize)];
        let mut scheduled = HashSet::from([request.seed_url.clone()]);
        let mut seen = HashSet::new();
        let mut pages = 0usize;
        while let Some((url_id, url, depth)) = queue.pop() {
            if pages >= request.budgets.max_pages
                || started.elapsed() >= request.budgets.max_duration
            {
                break;
            }
            if self
                .store
                .crawl_jobs(&self.profile)
                .get_state(id)?
                .as_deref()
                == Some("cancelled")
            {
                return self.status(id);
            }
            if !seen.insert(url.clone()) || depth > request.budgets.max_depth {
                continue;
            }
            let Ok(url_parsed) = Url::parse(&url) else {
                continue;
            };
            let Some(origin) = origin_key(&url_parsed) else {
                continue;
            };
            let delay = match self.robots.check(&url_parsed).await {
                RobotsDecision::Allowed { delay } => delay,
                RobotsDecision::Disallowed => {
                    let _ = self.store.crawl_urls(&self.profile).set_state(
                        &url_id,
                        "failed",
                        Some("robots_disallowed"),
                    );
                    continue;
                }
                RobotsDecision::Unavailable => {
                    let _ = self.store.crawl_urls(&self.profile).set_state(
                        &url_id,
                        "failed",
                        Some("robots_unavailable"),
                    );
                    continue;
                }
            };
            self.robots.wait_for_host(&origin, delay).await;
            let Ok(fetched) = self.fetch.get(&url).await else {
                let _ = self.store.crawl_urls(&self.profile).set_state(
                    &url_id,
                    "failed",
                    Some("fetch_failed"),
                );
                continue;
            };
            // The gate cleared the requested URL, but the origin chose where the redirect landed.
            // Ask again about the final URL so a redirect cannot carry us into a disallowed path.
            if fetched.url != url && !self.final_url_is_allowed(&fetched.url).await {
                let _ = self.store.crawl_urls(&self.profile).set_state(
                    &url_id,
                    "failed",
                    Some("robots_disallowed"),
                );
                continue;
            }
            let extracted = match extract::extract(
                &fetched.body,
                &fetched.content_type,
                Some(&fetched.url),
                ExtractionOptions::default(),
            ) {
                Ok(extracted) => extracted,
                Err(error) => {
                    let reason = match error {
                        extract::ExtractionError::UnsupportedContentType(_) => {
                            "unsupported_content_type"
                        }
                        extract::ExtractionError::OutputBoundZero => "extraction_failed",
                    };
                    let _ = self.store.crawl_urls(&self.profile).set_state(
                        &url_id,
                        "failed",
                        Some(reason),
                    );
                    continue;
                }
            };
            let doc_id = format!("{id}-{pages}");
            self.store
                .documents(&self.profile)
                .add(&doc_id, id, &fetched.url, None)?;
            let _ = self
                .store
                .crawl_urls(&self.profile)
                .set_state(&url_id, "completed", None);
            pages += 1;
            if depth < request.budgets.max_depth {
                self.enqueue_links(
                    id,
                    depth,
                    request.budgets.max_pages,
                    extracted.links,
                    &mut scheduled,
                    &mut queue,
                );
            }
        }
        if self
            .store
            .crawl_jobs(&self.profile)
            .get_state(id)?
            .as_deref()
            != Some("cancelled")
        {
            self.store.crawl_jobs(&self.profile).set_state(
                id,
                "completed",
                None,
                &chrono::Utc::now().to_rfc3339(),
            )?;
        }
        self.status(id)
    }

    /// Move a job to `failed`, recording why.
    ///
    /// Used when `run` itself could not continue, so that a caller polling `status` sees a
    /// terminal state instead of waiting on a job that has already stopped.
    pub fn fail(&self, id: &str, reason: &str) -> Result<bool, CrawlError> {
        Ok(self.store.crawl_jobs(&self.profile).set_state(
            id,
            "failed",
            Some(reason),
            &chrono::Utc::now().to_rfc3339(),
        )?)
    }

    /// Whether the URL a redirect actually landed on is still permitted.
    ///
    /// An unparseable or unreadable destination is a denial, matching the gate's own posture.
    async fn final_url_is_allowed(&self, final_url: &str) -> bool {
        let Ok(parsed) = Url::parse(final_url) else {
            return false;
        };
        matches!(
            self.robots.check(&parsed).await,
            RobotsDecision::Allowed { .. }
        )
    }

    fn enqueue_links(
        &self,
        id: &str,
        depth: usize,
        max_pages: usize,
        links: Vec<String>,
        scheduled: &mut HashSet<String>,
        queue: &mut Vec<(String, String, usize)>,
    ) {
        for next in links {
            if scheduled.len() >= max_pages || !scheduled.insert(next.clone()) {
                continue;
            }
            let next_id = format!("{id}-url-{}", scheduled.len());
            let _ = self.store.crawl_urls(&self.profile).add(
                &next_id,
                id,
                &next,
                i64::try_from(depth + 1).unwrap_or(i64::MAX),
            );
            queue.push((next_id, next, depth + 1));
        }
    }
}

fn parse_state(value: &str) -> CrawlState {
    match value {
        "queued" => CrawlState::Queued,
        "running" => CrawlState::Running,
        "completed" => CrawlState::Completed,
        "cancelled" => CrawlState::Cancelled,
        _ => CrawlState::Failed,
    }
}

/// Bounded map request independent of transport or MCP.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MapOptions {
    pub limit: usize,
    pub include_paths: Vec<String>,
    pub exclude_paths: Vec<String>,
    pub include_subdomains: bool,
}

impl Default for MapOptions {
    fn default() -> Self {
        Self {
            limit: 5_000,
            include_paths: vec!["/**".to_owned()],
            exclude_paths: Vec::new(),
            include_subdomains: false,
        }
    }
}

/// Transport-independent result of bounded URL discovery.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub struct MapResult {
    pub seed_url: String,
    pub urls: Vec<String>,
    pub discovered_count: usize,
    pub truncated: bool,
}

/// Collect and normalize URLs from sitemap XML and extracted HTML links.
#[must_use]
pub fn map_documents(
    seed_url: &str,
    sitemap_documents: &[&str],
    html_links: &[&str],
    options: &MapOptions,
) -> MapResult {
    let Ok(seed) = Url::parse(seed_url) else {
        return MapResult {
            seed_url: seed_url.to_owned(),
            urls: Vec::new(),
            discovered_count: 0,
            truncated: false,
        };
    };
    if !matches!(seed.scheme(), "http" | "https") {
        return MapResult {
            seed_url: seed_url.to_owned(),
            urls: Vec::new(),
            discovered_count: 0,
            truncated: false,
        };
    }
    let mut candidates = BTreeSet::new();
    candidates.insert(normalize_url(seed.clone()));
    sitemap_documents
        .iter()
        .flat_map(|document| xml_locations(document))
        .filter_map(|value| resolve_url(&seed, value))
        .map(normalize_url)
        .for_each(|url| {
            candidates.insert(url);
        });
    for value in html_links {
        if let Some(url) = resolve_url(&seed, value) {
            candidates.insert(normalize_url(url));
        }
    }
    let discovered_count = candidates.len();
    let scoped_urls = candidates
        .into_iter()
        .filter(|value| in_scope(&seed, value, options))
        .collect::<Vec<_>>();
    let truncated = scoped_urls.len() > options.limit;
    let urls = scoped_urls
        .into_iter()
        .take(options.limit)
        .collect::<Vec<_>>();
    MapResult {
        seed_url: normalize_url(seed),
        urls,
        discovered_count,
        truncated,
    }
}

fn xml_locations(document: &str) -> impl Iterator<Item = &str> {
    document.split('<').filter_map(|part| {
        let (tag, rest) = part.split_once('>')?;
        if !tag.trim_start().starts_with("loc") {
            return None;
        }
        let end = rest.find("</loc>")?;
        Some(rest[..end].trim())
    })
}

fn resolve_url(seed: &Url, value: &str) -> Option<Url> {
    let value = value.trim();
    if value.is_empty()
        || value.starts_with('#')
        || value.starts_with("javascript:")
        || value.len() > MAX_URL_CHARS
    {
        return None;
    }
    let url = seed.join(value).ok()?;
    matches!(url.scheme(), "http" | "https").then_some(url)
}

fn normalize_url(mut url: Url) -> String {
    url.set_fragment(None);
    if (url.scheme() == "http" && url.port() == Some(80))
        || (url.scheme() == "https" && url.port() == Some(443))
    {
        let _ = url.set_port(None);
    }
    url.to_string()
}

fn in_scope(seed: &Url, candidate: &str, options: &MapOptions) -> bool {
    let Ok(url) = Url::parse(candidate) else {
        return false;
    };
    if url.host_str() != seed.host_str()
        && (!options.include_subdomains
            || !same_registrable_suffix(seed.host_str(), url.host_str()))
    {
        return false;
    }
    if !crate::policy::query_is_bounded(&url) {
        return false;
    }
    let path = url.path();
    let included = options.include_paths.is_empty()
        || options
            .include_paths
            .iter()
            .any(|glob| path_matches(path, glob));
    let excluded = options
        .exclude_paths
        .iter()
        .any(|glob| path_matches(path, glob));
    included && !excluded
}

fn same_registrable_suffix(left: Option<&str>, right: Option<&str>) -> bool {
    let (Some(left), Some(right)) = (left, right) else {
        return false;
    };
    if left.parse::<std::net::IpAddr>().is_ok() || right.parse::<std::net::IpAddr>().is_ok() {
        return false;
    }
    let left = left.rsplitn(3, '.').collect::<Vec<_>>();
    let right = right.rsplitn(3, '.').collect::<Vec<_>>();
    left.len() >= 2 && right.len() >= 2 && left[..2] == right[..2]
}

fn path_matches(path: &str, glob: &str) -> bool {
    let glob = glob.trim();
    if glob == "/**" || glob == "**" {
        return true;
    }
    if let Some(prefix) = glob.strip_suffix("/**") {
        return path == prefix || path.starts_with(&format!("{prefix}/"));
    }
    path == glob
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{CrawlBudgets, CrawlRequest, CrawlService, CrawlState, MapOptions, map_documents};
    use crate::robots::ROBOTS_USER_AGENT;
    use crate::robots::{RobotsFetchError, RobotsFetcher};
    use crate::{fetch::FetchClient, storage::Store};

    #[test]
    fn starts_jobs_with_seed_and_isolates_profiles() {
        let store = Store::open_in_memory().unwrap();
        let fetch = FetchClient::new(Duration::from_secs(1), 1024).unwrap();
        let request = CrawlRequest {
            seed_url: "https://example.com/".into(),
            map: MapOptions::default(),
            budgets: CrawlBudgets::default(),
        };
        let alpha = CrawlService::new(&store, fetch.clone(), "alpha");
        let beta = CrawlService::new(&store, fetch, "beta");
        let id = alpha.start(&request).unwrap();
        assert_eq!(alpha.status(&id).unwrap().state, CrawlState::Queued);
        assert_eq!(alpha.status(&id).unwrap().pages, 0);
        assert!(beta.status(&id).is_err());
        assert_eq!(store.crawl_urls("alpha").count_for_job(&id).unwrap(), 1);
    }

    #[tokio::test]
    async fn unavailable_robots_default_deny_without_fetching_page() {
        let store = Store::open_in_memory().unwrap();
        let fetch = FetchClient::new(Duration::from_secs(1), 1024).unwrap();
        let robots_fetcher: RobotsFetcher =
            std::sync::Arc::new(|_| Box::pin(async { Err(RobotsFetchError::Unreadable) }));
        let service = CrawlService::with_robots_fetcher(&store, fetch, "test", robots_fetcher);
        let request = CrawlRequest {
            seed_url: "https://example.com/".into(),
            map: MapOptions::default(),
            budgets: CrawlBudgets::default(),
        };
        let id = service.start(&request).unwrap();
        let status = service.run(&id, &request).await.unwrap();
        assert_eq!(status.pages, 0);
        assert_eq!(store.crawl_urls("test").pending(&id, 1).unwrap().len(), 0);
    }

    // Redirects are followed, so the URL that was cleared is not always the URL that was fetched.
    #[tokio::test]
    async fn a_redirect_destination_is_checked_against_robots_before_it_is_kept() {
        let store = Store::open_in_memory().unwrap();
        let fetch = FetchClient::new(Duration::from_secs(1), 1024).unwrap();
        let robots_fetcher: RobotsFetcher = std::sync::Arc::new(|_| {
            Box::pin(async { Ok("User-agent: websift\nDisallow: /private\nAllow: /".to_owned()) })
        });
        let service = CrawlService::with_robots_fetcher(&store, fetch, "test", robots_fetcher);
        assert!(
            service
                .final_url_is_allowed("https://example.com/public")
                .await
        );
        assert!(
            !service
                .final_url_is_allowed("https://example.com/private/page")
                .await
        );
        // A destination that is not a URL at all is a denial, not a pass.
        assert!(!service.final_url_is_allowed("not a url").await);
    }

    #[tokio::test]
    async fn robots_rules_disallow_page_and_allow_other_page() {
        let store = Store::open_in_memory().unwrap();
        let fetch = FetchClient::new(Duration::from_secs(1), 1024).unwrap();
        let robots_fetcher: RobotsFetcher = std::sync::Arc::new(|_| {
            Box::pin(async { Ok("User-agent: websift\nDisallow: /private\nAllow: /".to_owned()) })
        });
        let service = CrawlService::with_robots_fetcher(&store, fetch, "test", robots_fetcher);
        let rules = service
            .robots
            .rules_for("https://example.com")
            .await
            .unwrap();
        assert!(!rules.allowed("/private", ROBOTS_USER_AGENT));
        assert!(rules.allowed("/public", ROBOTS_USER_AGENT));
    }

    #[tokio::test]
    async fn status_and_cancel_can_run_while_crawl_is_waiting() {
        let store = Store::open_in_memory().unwrap();
        let fetch = FetchClient::new(Duration::from_secs(1), 1024).unwrap();
        let robots_fetcher: RobotsFetcher = std::sync::Arc::new(|_| {
            Box::pin(async {
                tokio::time::sleep(Duration::from_millis(20)).await;
                Ok("User-agent: *\\nAllow: /".to_owned())
            })
        });
        let service = CrawlService::with_robots_fetcher(&store, fetch, "test", robots_fetcher);
        let request = CrawlRequest {
            seed_url: "https://example.com/".into(),
            map: MapOptions::default(),
            budgets: CrawlBudgets::default(),
        };
        let id = service.start(&request).unwrap();
        let (running, ()) = tokio::join!(service.run(&id, &request), async {
            tokio::task::yield_now().await;
            let status = service.status(&id).unwrap();
            assert!(matches!(status.state, CrawlState::Running));
            assert!(service.cancel(&id).unwrap());
        });
        assert_eq!(running.unwrap().state, CrawlState::Cancelled);
        assert_eq!(service.status(&id).unwrap().state, CrawlState::Cancelled);
    }

    #[test]
    fn cancellation_is_idempotent_and_preserves_terminal_states() {
        let store = Store::open_in_memory().unwrap();
        let fetch = FetchClient::new(Duration::from_secs(1), 1024).unwrap();
        let service = CrawlService::new(&store, fetch, "test");
        let request = CrawlRequest {
            seed_url: "https://example.com/".into(),
            map: MapOptions::default(),
            budgets: CrawlBudgets::default(),
        };
        let completed = service.start(&request).unwrap();
        store
            .crawl_jobs("test")
            .set_state(&completed, "completed", None, "now")
            .unwrap();
        assert!(!service.cancel(&completed).unwrap());
        assert_eq!(
            service.status(&completed).unwrap().state,
            CrawlState::Completed
        );

        let failed = service.start(&request).unwrap();
        store
            .crawl_jobs("test")
            .set_state(&failed, "failed", Some("test"), "now")
            .unwrap();
        assert!(!service.cancel(&failed).unwrap());
        assert_eq!(service.status(&failed).unwrap().state, CrawlState::Failed);

        let cancelled = service.start(&request).unwrap();
        assert!(service.cancel(&cancelled).unwrap());
        assert!(service.cancel(&cancelled).unwrap());
        assert_eq!(
            service.status(&cancelled).unwrap().state,
            CrawlState::Cancelled
        );
    }

    #[test]
    fn rejects_zero_page_budget() {
        let store = Store::open_in_memory().unwrap();
        let fetch = FetchClient::new(Duration::from_secs(1), 1024).unwrap();
        let service = CrawlService::new(&store, fetch, "test");
        let request = CrawlRequest {
            seed_url: "https://example.com/".into(),
            map: MapOptions::default(),
            budgets: CrawlBudgets {
                max_pages: 0,
                ..CrawlBudgets::default()
            },
        };
        assert!(service.start(&request).is_err());
    }

    #[test]
    fn maps_sitemap_and_html_links_with_deduplication_and_filters() {
        let sitemap = r"<urlset><url><loc>https://example.com/guides/one#top</loc></url><url><loc>https://example.com/changelog/x</loc></url></urlset>";
        let result = map_documents(
            "https://example.com/",
            &[sitemap],
            &[
                "/guides/one",
                "https://example.com/guides/two",
                "https://other.example.com/no",
            ],
            &MapOptions {
                limit: 10,
                include_paths: vec!["/guides/**".into()],
                exclude_paths: vec!["/guides/two".into()],
                include_subdomains: false,
            },
        );
        assert_eq!(result.urls, ["https://example.com/guides/one"]);
        assert_eq!(result.discovered_count, 4);
    }

    #[test]
    fn rejects_non_web_seed_urls() {
        let result = map_documents("file:///tmp/index.html", &[], &[], &MapOptions::default());
        assert!(result.urls.is_empty());
        assert_eq!(result.discovered_count, 0);
    }

    #[test]
    fn does_not_match_unrelated_ip_addresses_as_subdomains() {
        let result = map_documents(
            "http://127.0.0.1/",
            &[],
            &["http://10.0.0.1/private"],
            &MapOptions {
                include_subdomains: true,
                ..MapOptions::default()
            },
        );
        assert_eq!(result.urls, ["http://127.0.0.1/"]);
    }

    #[test]
    fn truncation_reflects_filtered_results() {
        let result = map_documents(
            "https://example.com/",
            &[],
            &["/blocked/one", "/blocked/two"],
            &MapOptions {
                limit: 1,
                exclude_paths: vec!["/blocked/**".into()],
                ..MapOptions::default()
            },
        );
        assert_eq!(result.urls, ["https://example.com/"]);
        assert_eq!(result.discovered_count, 3);
        assert!(!result.truncated);
    }

    #[test]
    fn applies_limit_and_subdomain_scope() {
        let result = map_documents(
            "https://docs.example.com/",
            &[],
            &[
                "https://api.example.com/a",
                "https://docs.example.com/b",
                "https://evil.com/c",
            ],
            &MapOptions {
                limit: 2,
                include_subdomains: true,
                ..MapOptions::default()
            },
        );
        assert_eq!(result.urls.len(), 2);
        assert!(result.truncated);
        assert!(!result.urls.iter().any(|url| url.contains("evil")));
    }
}
