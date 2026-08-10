#![allow(clippy::doc_markdown, clippy::missing_errors_doc)]
//! Search retrieval, independent of the MCP adapter.
//!
//! Two backends share one bounded transport. A configured SearXNG instance returns JSON and is
//! preferred when present. Otherwise the built-in keyless backend is used so that search works
//! without any configuration. Backend responses are parsed as untrusted data.

use std::{collections::HashSet, time::Duration};

use futures_util::StreamExt;
use reqwest::{Client, StatusCode, header::USER_AGENT};
use scraper::{Html, Selector};
use serde::Deserialize;
use url::Url;

use crate::{
    config::Config,
    policy::{DnsResolver, PublicUrl, SystemDnsResolver, UrlError, ValidatingDnsResolver},
};
use std::sync::Arc;

/// A normalized SearXNG result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub content: String,
    pub engine: Option<String>,
}

/// Optional SearXNG filters supported by the MCP search contract.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchOptions {
    pub language: Option<String>,
    pub time_range: Option<String>,
    pub domains: Vec<String>,
}

/// Stable failures returned by the search client.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SearchError {
    #[error("no search backend is configured")]
    NotConfigured,
    #[error("search endpoint validation failed: {0:?}")]
    InvalidUrl(UrlError),
    #[error("search request timed out: {0}")]
    Timeout(String),
    #[error("search request failed: {0}")]
    Transport(String),
    #[error("search response status was {0}")]
    Status(StatusCode),
    #[error("search response exceeded maximum size of {limit} bytes")]
    ResponseTooLarge { limit: u64 },
    #[error("search response was invalid: {0}")]
    InvalidResponse(String),
}

/// Backend selected from configuration, never from tool arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Backend {
    /// A user-configured SearXNG instance returning JSON.
    Searxng(PublicUrl),
    /// The built-in keyless backend used when no instance is configured.
    Duckduckgo,
}

/// Bounded search client covering every supported backend.
#[derive(Debug, Clone)]
pub struct SearchClient {
    client: Client,
    backend: Backend,
    timeout: Duration,
    max_bytes: u64,
    max_results: usize,
}

const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_ADDRESSES: usize = 16;
const MAX_REDIRECTS: usize = 5;
const DUCKDUCKGO_ENDPOINT: &str = "https://lite.duckduckgo.com/lite/";
/// Stable outbound identity. The crawler identifies itself instead of impersonating a browser.
const CLIENT_USER_AGENT: &str = concat!("websift/", env!("CARGO_PKG_VERSION"));

#[allow(clippy::needless_pass_by_value)]
fn search_transport_error(error: reqwest::Error) -> SearchError {
    let message = error.to_string();
    if error.is_timeout() {
        SearchError::Timeout(message)
    } else {
        SearchError::Transport(message)
    }
}

impl SearchClient {
    /// Construct a client from process configuration.
    ///
    /// A configured SearXNG instance wins. Without one the built-in keyless backend is used, so
    /// search never requires configuration.
    pub fn from_config(config: &Config) -> Result<Self, SearchError> {
        let backend = match config.searxng_url.as_deref() {
            Some(endpoint) => {
                Backend::Searxng(PublicUrl::parse(endpoint).map_err(SearchError::InvalidUrl)?)
            }
            None => Backend::Duckduckgo,
        };
        Self::with_backend(
            backend,
            Duration::from_millis(config.timeout_ms),
            config.max_bytes,
            config.max_results,
            Arc::new(SystemDnsResolver),
        )
    }

    /// Construct a SearXNG client with explicit bounds.
    pub fn new(
        endpoint: &str,
        timeout: Duration,
        max_bytes: u64,
        max_results: u32,
    ) -> Result<Self, SearchError> {
        Self::with_resolver(
            endpoint,
            timeout,
            max_bytes,
            max_results,
            Arc::new(SystemDnsResolver),
        )
    }

    /// Construct a SearXNG client with an injectable DNS resolver for tests and controlled runtimes.
    pub fn with_resolver(
        endpoint: &str,
        timeout: Duration,
        max_bytes: u64,
        max_results: u32,
        resolver: Arc<dyn DnsResolver>,
    ) -> Result<Self, SearchError> {
        let endpoint = PublicUrl::parse(endpoint).map_err(SearchError::InvalidUrl)?;
        Self::with_backend(
            Backend::Searxng(endpoint),
            timeout,
            max_bytes,
            max_results,
            resolver,
        )
    }

    /// Report the backend name used for result provenance.
    #[must_use]
    pub fn provider(&self) -> &'static str {
        match self.backend {
            Backend::Searxng(_) => "searxng",
            Backend::Duckduckgo => "duckduckgo",
        }
    }

    fn with_backend(
        backend: Backend,
        timeout: Duration,
        max_bytes: u64,
        max_results: u32,
        resolver: Arc<dyn DnsResolver>,
    ) -> Result<Self, SearchError> {
        if max_bytes == 0 || max_results == 0 {
            return Err(SearchError::InvalidResponse(
                "search bounds must be non-zero".to_owned(),
            ));
        }
        let validating_resolver =
            ValidatingDnsResolver::new(resolver, RESOLVE_TIMEOUT.min(timeout), MAX_ADDRESSES);
        let client = Client::builder()
            .dns_resolver(Arc::new(validating_resolver))
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                let Some(previous) = attempt.previous().last() else {
                    return attempt.stop();
                };
                let is_https_downgrade =
                    previous.scheme() == "https" && attempt.url().scheme() == "http";
                if attempt.previous().len() > MAX_REDIRECTS
                    || is_https_downgrade
                    || PublicUrl::parse(attempt.url().as_str()).is_err()
                {
                    attempt.stop()
                } else {
                    attempt.follow()
                }
            }))
            .connect_timeout(timeout)
            .timeout(timeout)
            .build()
            .map_err(search_transport_error)?;
        Ok(Self {
            client,
            backend,
            timeout,
            max_bytes,
            max_results: max_results as usize,
        })
    }

    /// Search the selected backend and return at most the configured number of results.
    pub async fn search(&self, query: &str) -> Result<Vec<SearchResult>, SearchError> {
        self.search_with_options(query, &SearchOptions::default())
            .await
    }

    /// Search the selected backend with optional filters.
    pub async fn search_with_options(
        &self,
        query: &str,
        options: &SearchOptions,
    ) -> Result<Vec<SearchResult>, SearchError> {
        let url = self.request_url(query, options)?;
        let response = self
            .client
            .get(url)
            .header(USER_AGENT, CLIENT_USER_AGENT)
            .timeout(self.timeout)
            .send()
            .await
            .map_err(search_transport_error)?;
        let status = response.status();
        if !status.is_success() {
            return Err(SearchError::Status(status));
        }
        if response
            .content_length()
            .is_some_and(|size| size > self.max_bytes)
        {
            return Err(SearchError::ResponseTooLarge {
                limit: self.max_bytes,
            });
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(search_transport_error)?;
            let new_len =
                body.len()
                    .checked_add(chunk.len())
                    .ok_or(SearchError::ResponseTooLarge {
                        limit: self.max_bytes,
                    })?;
            if u64::try_from(new_len).map_or(true, |size| size > self.max_bytes) {
                return Err(SearchError::ResponseTooLarge {
                    limit: self.max_bytes,
                });
            }
            body.extend_from_slice(&chunk);
        }
        match self.backend {
            Backend::Searxng(_) => parse_response(&body, self.max_results),
            Backend::Duckduckgo => parse_duckduckgo(&body, self.max_results),
        }
    }

    fn request_url(&self, query: &str, options: &SearchOptions) -> Result<Url, SearchError> {
        match &self.backend {
            Backend::Searxng(endpoint) => {
                let mut url = Url::parse(endpoint.as_str()).map_err(|_| {
                    SearchError::InvalidResponse("configured endpoint was not a URL".to_owned())
                })?;
                {
                    let path = format!("{}/search", url.path().trim_end_matches('/'));
                    url.set_path(if path == "/search" { "/search" } else { &path });
                }
                url.set_query(None);
                url.query_pairs_mut()
                    .append_pair("q", query)
                    .append_pair("format", "json");
                if let Some(language) = &options.language {
                    url.query_pairs_mut().append_pair("language", language);
                }
                if let Some(time_range) = &options.time_range {
                    url.query_pairs_mut().append_pair("time_range", time_range);
                }
                for domain in &options.domains {
                    url.query_pairs_mut().append_pair("site", domain);
                }
                Ok(url)
            }
            Backend::Duckduckgo => {
                let mut url = Url::parse(DUCKDUCKGO_ENDPOINT).map_err(|_| {
                    SearchError::InvalidResponse("built-in endpoint was not a URL".to_owned())
                })?;
                // The built-in backend has no structured domain filter, so scope becomes query syntax.
                let mut terms = query.to_owned();
                for domain in &options.domains {
                    terms.push_str(" site:");
                    terms.push_str(domain);
                }
                url.query_pairs_mut().append_pair("q", &terms);
                // The built-in backend keys on a region code such as `us-en`, not a bare language
                // tag. Sending a bare tag would filter on a region that was never requested, so it
                // is dropped rather than guessed.
                if let Some(region) = options
                    .language
                    .as_deref()
                    .filter(|value| value.contains('-'))
                {
                    url.query_pairs_mut().append_pair("kl", region);
                }
                if let Some(time_range) = &options.time_range
                    && let Some(code) = duckduckgo_time_range(time_range)
                {
                    url.query_pairs_mut().append_pair("df", code);
                }
                Ok(url)
            }
        }
    }
}

const fn duckduckgo_time_range(time_range: &str) -> Option<&'static str> {
    match time_range.as_bytes() {
        b"day" => Some("d"),
        b"week" => Some("w"),
        b"month" => Some("m"),
        b"year" => Some("y"),
        _ => None,
    }
}

#[derive(Debug, Deserialize)]
struct SearxResponse {
    results: Vec<RawResult>,
}

#[derive(Debug, Deserialize)]
struct RawResult {
    title: String,
    url: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    engine: Option<String>,
}

/// Parse and normalize a SearXNG response without performing I/O.
pub fn parse_response(body: &[u8], max_results: usize) -> Result<Vec<SearchResult>, SearchError> {
    if max_results == 0 {
        return Ok(Vec::new());
    }
    let response: SearxResponse = serde_json::from_slice(body)
        .map_err(|error| SearchError::InvalidResponse(error.to_string()))?;
    let mut seen = HashSet::new();
    let mut results = Vec::with_capacity(max_results.min(response.results.len()));
    for raw in response.results {
        let Ok(url) = PublicUrl::parse(&raw.url) else {
            continue;
        };
        let normalized = url.as_str().to_owned();
        if !seen.insert(normalized.clone()) {
            continue;
        }
        results.push(SearchResult {
            title: raw.title,
            url: normalized,
            content: raw.content,
            engine: raw.engine,
        });
        if results.len() == max_results {
            break;
        }
    }
    Ok(results)
}

/// Parse and normalize a built-in backend HTML response without performing I/O.
///
/// The markup is treated as untrusted data: nothing is executed or rendered, only anchors and
/// snippet text are read. An unparsable page is a failure rather than an empty result list.
pub fn parse_duckduckgo(body: &[u8], max_results: usize) -> Result<Vec<SearchResult>, SearchError> {
    if max_results == 0 {
        return Ok(Vec::new());
    }
    let document = Html::parse_document(&String::from_utf8_lossy(body));
    let links = Selector::parse("a.result-link, a.result__a")
        .map_err(|_| SearchError::InvalidResponse("invalid result selector".to_owned()))?;
    let snippets = Selector::parse("td.result-snippet, a.result__snippet")
        .map_err(|_| SearchError::InvalidResponse("invalid snippet selector".to_owned()))?;
    let snippets: Vec<String> = document
        .select(&snippets)
        .map(|element| collapse_whitespace(&element.text().collect::<String>()))
        .collect();
    let anchors: Vec<_> = document.select(&links).collect();
    // Snippets are matched to results by position, which only holds while the page emits exactly
    // one snippet per result. On any other shape the snippets are dropped: a missing snippet is
    // recoverable for the caller, a snippet attached to the wrong result is not.
    let aligned = snippets.len() == anchors.len();
    let mut seen = HashSet::new();
    let mut results = Vec::with_capacity(max_results);
    for (index, anchor) in anchors.into_iter().enumerate() {
        let Some(target) = anchor.value().attr("href").and_then(result_target) else {
            continue;
        };
        let Ok(url) = PublicUrl::parse(&target) else {
            continue;
        };
        let normalized = url.as_str().to_owned();
        if !seen.insert(normalized.clone()) {
            continue;
        }
        results.push(SearchResult {
            title: collapse_whitespace(&anchor.text().collect::<String>()),
            url: normalized,
            content: if aligned {
                snippets.get(index).cloned().unwrap_or_default()
            } else {
                String::new()
            },
            engine: Some("duckduckgo".to_owned()),
        });
        if results.len() == max_results {
            break;
        }
    }
    if results.is_empty() {
        return Err(SearchError::InvalidResponse(
            "no results could be parsed from the built-in backend response".to_owned(),
        ));
    }
    Ok(results)
}

/// Resolve a result anchor to its destination, unwrapping the backend redirect wrapper.
fn result_target(href: &str) -> Option<String> {
    let absolute = if let Some(rest) = href.strip_prefix("//") {
        format!("https://{rest}")
    } else {
        href.to_owned()
    };
    let parsed = Url::parse(&absolute).ok()?;
    let is_redirect = parsed
        .host_str()
        .is_some_and(|host| host == "duckduckgo.com" || host.ends_with(".duckduckgo.com"))
        && parsed.path().starts_with("/l/");
    if is_redirect {
        return parsed
            .query_pairs()
            .find(|(key, _)| key == "uddg")
            .map(|(_, value)| value.into_owned());
    }
    Some(absolute)
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::{SearchClient, SearchError, parse_duckduckgo, parse_response, result_target};
    use crate::config::Config;
    use reqwest::StatusCode;

    #[test]
    fn selects_builtin_backend_without_any_environment_variable() {
        let config = Config::from_lookup(|_| None).unwrap();
        assert!(config.searxng_url.is_none());
        let client = SearchClient::from_config(&config).unwrap();
        assert_eq!(client.provider(), "duckduckgo");
    }

    #[test]
    fn configured_instance_replaces_the_builtin_backend() {
        let config = Config::from_lookup(|key| {
            (key == "WEBSIFT_SEARXNG_URL").then(|| "https://search.example.com".to_owned())
        })
        .unwrap();
        let client = SearchClient::from_config(&config).unwrap();
        assert_eq!(client.provider(), "searxng");
    }

    #[test]
    fn parses_fixture_deduplicates_normalizes_and_limits() {
        let results = parse_response(include_bytes!("fixtures/search.json"), 2).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].url, "https://example.com/article");
        assert_eq!(results[1].url, "https://example.org/second");
    }

    #[test]
    fn rejects_malformed_fixture() {
        let error = parse_response(include_bytes!("fixtures/malformed.json"), 10).unwrap_err();
        assert!(matches!(error, SearchError::InvalidResponse(_)));
    }

    #[test]
    fn parses_builtin_fixture_unwrapping_redirects_and_rejecting_private_hosts() {
        let results = parse_duckduckgo(include_bytes!("fixtures/duckduckgo.html"), 10).unwrap();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].url, "https://example.com/article");
        assert_eq!(results[0].title, "Example title");
        assert_eq!(
            results[0].content,
            "Result snippet with collapsed whitespace."
        );
        assert_eq!(results[1].url, "https://example.org/second");
        // The duplicate and the loopback address are dropped, keeping snippet alignment intact.
        assert_eq!(results[2].url, "https://example.net/third");
        assert_eq!(results[2].content, "Third snippet.");
    }

    #[test]
    fn limits_builtin_results() {
        let results = parse_duckduckgo(include_bytes!("fixtures/duckduckgo.html"), 1).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn drops_snippets_rather_than_attaching_them_to_the_wrong_result() {
        // One result carries no snippet row, so positional pairing would shift every later
        // snippet onto the preceding result.
        let body = br#"<html><body><table>
            <tr><td><a class="result-link" href="https://example.com/a">A</a></td></tr>
            <tr><td><a class="result-link" href="https://example.org/b">B</a></td></tr>
            <tr><td class="result-snippet">snippet for B</td></tr>
        </table></body></html>"#;
        let results = parse_duckduckgo(body, 10).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].url, "https://example.com/a");
        assert!(results.iter().all(|result| result.content.is_empty()));
    }

    #[test]
    fn reports_unparsable_builtin_response_instead_of_empty_success() {
        let error = parse_duckduckgo(b"<html><body>blocked</body></html>", 10).unwrap_err();
        assert!(matches!(error, SearchError::InvalidResponse(_)));
    }

    #[test]
    fn resolves_redirect_and_direct_targets() {
        assert_eq!(
            result_target("//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fa&rut=x").unwrap(),
            "https://example.com/a"
        );
        assert_eq!(
            result_target("https://example.com/b").unwrap(),
            "https://example.com/b"
        );
        assert_eq!(result_target("/settings"), None);
    }

    #[test]
    fn maps_forbidden_status_stably() {
        let error = SearchError::Status(StatusCode::FORBIDDEN);
        assert_eq!(
            error.to_string(),
            "search response status was 403 Forbidden"
        );
    }
}
