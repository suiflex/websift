//! End-to-end tests for `deep_search` over real HTTP against loopback servers.
//!
//! The unit tests above cover ranking, planning, and classification in isolation. These cover the
//! assembled pipeline — search, fallback, retry, robots, cache, concurrency, deduplication —
//! because those only exist once the stages run together.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use super::{
    CachedPage, ContentSource, DeepSearchDeps, DeepSearchRequest, PageStore, ResearchWarning,
    deep_search,
};
use crate::{
    fetch::{FetchClient, search::SearchClient, search::SearchOptions},
    robots::{RobotsFetcher, RobotsGate},
    testing::{LoopbackResolver, Reply, TestServer},
};

const TIMEOUT: Duration = Duration::from_secs(5);

/// Search backend payload naming pages served by this suite.
fn search_json(urls: &[(&str, &str)]) -> String {
    let results: Vec<String> = urls
        .iter()
        .map(|(url, title)| {
            format!(
                r#"{{"title":"{title}","url":"{url}","content":"rust mcp snippet","engine":"test"}}"#
            )
        })
        .collect();
    format!(r#"{{"results":[{}]}}"#, results.join(","))
}

fn robots_allowing_all() -> RobotsGate {
    let fetcher: RobotsFetcher =
        Arc::new(|_| Box::pin(async { Ok("User-agent: *\nAllow: /".to_owned()) }));
    RobotsGate::with_fetcher(fetcher)
}

fn request(query: &str) -> DeepSearchRequest {
    DeepSearchRequest {
        query: query.to_owned(),
        variants: Vec::new(),
        max_queries: 1,
        max_sources: 5,
        max_pages: 5,
        max_chars: 5_000,
        search: SearchOptions::default(),
        concurrency: 4,
        per_host_concurrency: 2,
        budget: Duration::from_secs(20),
        attempts: 3,
    }
}

/// In-memory [`PageStore`] that records every read and write.
#[derive(Default)]
struct MemoryCache {
    entries: Mutex<HashMap<String, CachedPage>>,
    writes: Mutex<Vec<String>>,
}

impl MemoryCache {
    fn seeded(url: &str, page: CachedPage) -> Self {
        let cache = Self::default();
        cache
            .entries
            .lock()
            .unwrap()
            .insert(format!("{url}|5000"), page);
        cache
    }
}

impl PageStore for MemoryCache {
    fn get(&self, url: &str, max_chars: usize) -> Option<CachedPage> {
        self.entries
            .lock()
            .unwrap()
            .get(&format!("{url}|{max_chars}"))
            .cloned()
    }
    fn put(&self, url: &str, max_chars: usize, page: &CachedPage) {
        self.entries
            .lock()
            .unwrap()
            .insert(format!("{url}|{max_chars}"), page.clone());
        self.writes.lock().unwrap().push(url.to_owned());
    }
}

#[tokio::test]
async fn searches_ranks_fetches_and_caches_one_question_end_to_end() {
    let server = TestServer::start(Duration::ZERO).await;
    server.route(
        "/search",
        vec![Reply::json(search_json(&[
            ("http://docs.example/guide", "rust mcp guide"),
            ("http://blog.example/post", "unrelated"),
        ]))],
    );
    server.route(
        "/guide",
        vec![Reply::html(
            "<html><body><main><p>Guide body</p></main></body></html>",
        )],
    );
    server.route(
        "/post",
        vec![Reply::html("<html><body><p>Post body</p></body></html>")],
    );

    let resolver = LoopbackResolver::new()
        .map("searx.example", server.address())
        .map("docs.example", server.address())
        .map("blog.example", server.address())
        .shared();
    let search = SearchClient::with_dns_override(
        "http://searx.example/",
        TIMEOUT,
        1_000_000,
        10,
        Arc::clone(&resolver),
    )
    .unwrap();
    let fetch = FetchClient::with_dns_override(TIMEOUT, 1_000_000, Arc::clone(&resolver)).unwrap();
    let robots = robots_allowing_all();
    let cache = MemoryCache::default();
    let deps = DeepSearchDeps {
        search: std::slice::from_ref(&search),
        fetch: &fetch,
        robots: &robots,
        cache: Some(&cache),
    };

    let bundle = deep_search(&deps, &request("rust mcp guide"))
        .await
        .unwrap();

    assert_eq!(bundle.meta.queries_succeeded, 1);
    assert_eq!(bundle.meta.candidate_count, 2);
    assert_eq!(bundle.meta.pages_fetched, 2);
    assert_eq!(bundle.meta.pages_from_cache, 0);
    assert_eq!(bundle.meta.provider, "searxng");
    assert!(!bundle.meta.budget_exhausted);
    // Term coverage puts the matching title first even though both share a provider rank order.
    assert_eq!(bundle.sources[0].url, "http://docs.example/guide");
    assert_eq!(bundle.sources[0].content_source, ContentSource::Network);
    assert!(
        bundle.sources[0]
            .content
            .as_deref()
            .unwrap()
            .contains("Guide body")
    );
    assert!(bundle.sources[0].content_hash.is_some());
    // Every fetched page is written back for the next call.
    assert_eq!(cache.writes.lock().unwrap().len(), 2);
    assert!(bundle.warnings.is_empty());
}

#[tokio::test]
async fn a_blocked_primary_backend_falls_back_to_the_next_one() {
    let blocked = TestServer::start(Duration::ZERO).await;
    blocked.route("/search", vec![Reply::status(403)]);
    let healthy = TestServer::start(Duration::ZERO).await;
    healthy.route(
        "/search",
        vec![Reply::json(search_json(&[(
            "http://docs.example/guide",
            "rust mcp guide",
        )]))],
    );
    healthy.route(
        "/guide",
        vec![Reply::html("<html><body><p>Body</p></body></html>")],
    );

    let resolver = LoopbackResolver::new()
        .map("primary.example", blocked.address())
        .map("secondary.example", healthy.address())
        .map("docs.example", healthy.address())
        .shared();
    let clients = vec![
        SearchClient::with_dns_override(
            "http://primary.example/",
            TIMEOUT,
            1_000_000,
            10,
            Arc::clone(&resolver),
        )
        .unwrap(),
        SearchClient::with_dns_override(
            "http://secondary.example/",
            TIMEOUT,
            1_000_000,
            10,
            Arc::clone(&resolver),
        )
        .unwrap(),
    ];
    let fetch = FetchClient::with_dns_override(TIMEOUT, 1_000_000, Arc::clone(&resolver)).unwrap();
    let robots = robots_allowing_all();
    let deps = DeepSearchDeps {
        search: &clients,
        fetch: &fetch,
        robots: &robots,
        cache: None,
    };

    let bundle = deep_search(&deps, &request("rust mcp guide"))
        .await
        .unwrap();

    assert_eq!(bundle.sources.len(), 1);
    assert_eq!(bundle.meta.pages_fetched, 1);
    // The blocked instance was retried before the fallback took over.
    assert_eq!(blocked.hits("/search"), 3);
    assert_eq!(healthy.hits("/search"), 1);
    assert!(bundle.warnings.is_empty());
}

#[tokio::test]
async fn a_transient_backend_failure_is_retried_rather_than_reported() {
    let server = TestServer::start(Duration::ZERO).await;
    server.route(
        "/search",
        vec![
            Reply::status(503),
            Reply::json(search_json(&[(
                "http://docs.example/guide",
                "rust mcp guide",
            )])),
        ],
    );
    server.route(
        "/guide",
        vec![Reply::html("<html><body><p>Body</p></body></html>")],
    );

    let resolver = LoopbackResolver::new()
        .map("searx.example", server.address())
        .map("docs.example", server.address())
        .shared();
    let search = SearchClient::with_dns_override(
        "http://searx.example/",
        TIMEOUT,
        1_000_000,
        10,
        Arc::clone(&resolver),
    )
    .unwrap();
    let fetch = FetchClient::with_dns_override(TIMEOUT, 1_000_000, Arc::clone(&resolver)).unwrap();
    let robots = robots_allowing_all();
    let deps = DeepSearchDeps {
        search: std::slice::from_ref(&search),
        fetch: &fetch,
        robots: &robots,
        cache: None,
    };

    let bundle = deep_search(&deps, &request("rust mcp guide"))
        .await
        .unwrap();

    assert_eq!(server.hits("/search"), 2);
    assert_eq!(bundle.sources.len(), 1);
    assert!(bundle.warnings.is_empty());
}

#[tokio::test]
async fn a_cached_page_is_reused_and_the_server_is_never_asked() {
    let server = TestServer::start(Duration::ZERO).await;
    server.route(
        "/search",
        vec![Reply::json(search_json(&[(
            "http://docs.example/guide",
            "rust mcp guide",
        )]))],
    );
    server.route(
        "/guide",
        vec![Reply::html("<html><body><p>Fresh</p></body></html>")],
    );

    let resolver = LoopbackResolver::new()
        .map("searx.example", server.address())
        .map("docs.example", server.address())
        .shared();
    let search = SearchClient::with_dns_override(
        "http://searx.example/",
        TIMEOUT,
        1_000_000,
        10,
        Arc::clone(&resolver),
    )
    .unwrap();
    let fetch = FetchClient::with_dns_override(TIMEOUT, 1_000_000, Arc::clone(&resolver)).unwrap();
    let robots = robots_allowing_all();
    let cache = MemoryCache::seeded(
        "http://docs.example/guide",
        CachedPage {
            final_url: "http://docs.example/guide".to_owned(),
            title: Some("Cached".to_owned()),
            markdown: "cached body".to_owned(),
            content_hash: "sha256:cached".to_owned(),
            truncated: false,
        },
    );
    let deps = DeepSearchDeps {
        search: std::slice::from_ref(&search),
        fetch: &fetch,
        robots: &robots,
        cache: Some(&cache),
    };

    let bundle = deep_search(&deps, &request("rust mcp guide"))
        .await
        .unwrap();

    assert_eq!(bundle.meta.pages_from_cache, 1);
    assert_eq!(bundle.meta.pages_fetched, 0);
    assert_eq!(bundle.sources[0].content.as_deref(), Some("cached body"));
    assert_eq!(bundle.sources[0].content_source, ContentSource::Cache);
    assert_eq!(server.hits("/guide"), 0);
    // A cache hit must not rewrite the entry it just read.
    assert!(cache.writes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_robots_disallowed_page_is_warned_about_and_left_unfetched() {
    let server = TestServer::start(Duration::ZERO).await;
    server.route(
        "/search",
        vec![Reply::json(search_json(&[(
            "http://docs.example/private",
            "rust mcp guide",
        )]))],
    );
    server.route(
        "/private",
        vec![Reply::html("<html><body><p>Secret</p></body></html>")],
    );

    let resolver = LoopbackResolver::new()
        .map("searx.example", server.address())
        .map("docs.example", server.address())
        .shared();
    let search = SearchClient::with_dns_override(
        "http://searx.example/",
        TIMEOUT,
        1_000_000,
        10,
        Arc::clone(&resolver),
    )
    .unwrap();
    let fetch = FetchClient::with_dns_override(TIMEOUT, 1_000_000, Arc::clone(&resolver)).unwrap();
    let robots_fetcher: RobotsFetcher =
        Arc::new(|_| Box::pin(async { Ok("User-agent: *\nDisallow: /private".to_owned()) }));
    let robots = RobotsGate::with_fetcher(robots_fetcher);
    let deps = DeepSearchDeps {
        search: std::slice::from_ref(&search),
        fetch: &fetch,
        robots: &robots,
        cache: None,
    };

    let bundle = deep_search(&deps, &request("rust mcp guide"))
        .await
        .unwrap();

    assert_eq!(bundle.meta.pages_fetched, 0);
    assert_eq!(server.hits("/private"), 0);
    // The source survives with its snippet so the caller still sees the citation.
    assert_eq!(bundle.sources.len(), 1);
    assert!(bundle.sources[0].content.is_none());
    assert!(matches!(
        bundle.warnings.as_slice(),
        [ResearchWarning::RobotsDisallowed { url }] if url == "http://docs.example/private"
    ));
}

#[tokio::test]
async fn mirrors_serving_identical_bytes_keep_one_copy_and_both_citations() {
    let server = TestServer::start(Duration::ZERO).await;
    let page = "<html><body><main><p>Identical body</p></main></body></html>";
    server.route(
        "/search",
        vec![Reply::json(search_json(&[
            ("http://docs.example/guide", "rust mcp guide"),
            ("http://mirror.example/guide", "rust mcp guide"),
        ]))],
    );
    server.route("/guide", vec![Reply::html(page)]);

    let resolver = LoopbackResolver::new()
        .map("searx.example", server.address())
        .map("docs.example", server.address())
        .map("mirror.example", server.address())
        .shared();
    let search = SearchClient::with_dns_override(
        "http://searx.example/",
        TIMEOUT,
        1_000_000,
        10,
        Arc::clone(&resolver),
    )
    .unwrap();
    let fetch = FetchClient::with_dns_override(TIMEOUT, 1_000_000, Arc::clone(&resolver)).unwrap();
    let robots = robots_allowing_all();
    let deps = DeepSearchDeps {
        search: std::slice::from_ref(&search),
        fetch: &fetch,
        robots: &robots,
        cache: None,
    };

    let bundle = deep_search(&deps, &request("rust mcp guide"))
        .await
        .unwrap();

    assert_eq!(bundle.sources.len(), 2);
    assert!(bundle.sources[0].content.is_some());
    assert_eq!(
        bundle.sources[1].duplicate_of.as_deref(),
        Some(bundle.sources[0].url.as_str())
    );
    assert!(bundle.sources[1].content.is_none());
}

#[tokio::test]
async fn one_host_never_receives_more_concurrent_fetches_than_its_limit() {
    let server = TestServer::start(Duration::from_millis(80)).await;
    server.route(
        "/search",
        vec![Reply::json(search_json(&[
            ("http://docs.example/a", "rust mcp guide"),
            ("http://docs.example/b", "rust mcp guide"),
            ("http://docs.example/c", "rust mcp guide"),
        ]))],
    );
    for path in ["/a", "/b", "/c"] {
        server.route(path, vec![Reply::text("body")]);
    }

    let resolver = LoopbackResolver::new()
        .map("searx.example", server.address())
        .map("docs.example", server.address())
        .shared();
    let search = SearchClient::with_dns_override(
        "http://searx.example/",
        TIMEOUT,
        1_000_000,
        10,
        Arc::clone(&resolver),
    )
    .unwrap();
    let fetch = FetchClient::with_dns_override(TIMEOUT, 1_000_000, Arc::clone(&resolver)).unwrap();
    let robots = robots_allowing_all();
    let deps = DeepSearchDeps {
        search: std::slice::from_ref(&search),
        fetch: &fetch,
        robots: &robots,
        cache: None,
    };
    let mut request = request("rust mcp guide");
    request.per_host_concurrency = 1;
    request.concurrency = 4;

    let bundle = deep_search(&deps, &request).await.unwrap();

    assert_eq!(bundle.meta.pages_fetched, 3);
    // The search request and the page requests are all served by one process, so the peak also
    // covers robots reads; a per-host limit of one still forbids two pages at once.
    assert!(
        server.peak_concurrency() <= 2,
        "peak concurrency was {}",
        server.peak_concurrency()
    );
}

#[tokio::test]
async fn every_backend_failing_returns_the_error_instead_of_an_empty_bundle() {
    let server = TestServer::start(Duration::ZERO).await;
    server.route("/search", vec![Reply::status(500)]);
    let resolver = LoopbackResolver::new()
        .map("searx.example", server.address())
        .shared();
    let search = SearchClient::with_dns_override(
        "http://searx.example/",
        TIMEOUT,
        1_000_000,
        10,
        Arc::clone(&resolver),
    )
    .unwrap();
    let fetch = FetchClient::with_dns_override(TIMEOUT, 1_000_000, Arc::clone(&resolver)).unwrap();
    let robots = robots_allowing_all();
    let deps = DeepSearchDeps {
        search: std::slice::from_ref(&search),
        fetch: &fetch,
        robots: &robots,
        cache: None,
    };

    let failure = deep_search(&deps, &request("rust mcp guide")).await;

    assert!(failure.is_err());
    assert_eq!(server.hits("/search"), 3);
}
