#![allow(
    clippy::doc_markdown,
    clippy::missing_errors_doc,
    clippy::cast_precision_loss
)]
//! Deterministic multi-query research, independent of the MCP adapter.
//!
//! `deep_search` bundles the existing search and fetch primitives into one bounded operation:
//! plan a few queries, search them concurrently with retry and backend fallback, deduplicate
//! URLs, rank with explainable signals, then fetch only the highest ranked candidates under a
//! robots gate, a wall-clock budget, and global and per-host concurrency limits. No model is
//! called and no answer is synthesized; the caller owns synthesis. Every backend response is
//! untrusted data and is sanitized before it leaves this module.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use futures_util::{StreamExt, stream};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;
use url::Url;

use crate::{
    fetch::{
        FetchClient, FetchError,
        extract::{ExtractionError, ExtractionOptions, extract},
        search::{SearchClient, SearchError, SearchOptions, SearchResult},
    },
    robots::{RobotsDecision, RobotsGate, origin_key},
};

/// Cached extraction of one page, as stored and returned by a [`PageStore`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedPage {
    pub final_url: String,
    pub title: Option<String>,
    pub markdown: String,
    pub content_hash: String,
    pub truncated: bool,
}

/// Durable page cache seen by this module.
///
/// Research stays free of SQLite and of lock lifetimes: the adapter owns the connection, and
/// every call here happens between stages rather than inside a concurrent fetch, so no storage
/// lock is ever held across an await.
pub trait PageStore: Send + Sync {
    /// Return a fresh entry, or `None` when absent, expired, or unreadable.
    fn get(&self, url: &str, max_chars: usize) -> Option<CachedPage>;
    /// Record one extraction. Failures are ignored by callers; a cache miss is never fatal.
    fn put(&self, url: &str, max_chars: usize, page: &CachedPage);
}

/// Explicit ceilings and filters for one research operation.
#[derive(Debug, Clone)]
pub struct DeepSearchRequest {
    /// Caller question, used verbatim as the first query and as the term-coverage source.
    pub query: String,
    /// Caller-supplied query variants. No variant is invented from the question itself.
    pub variants: Vec<String>,
    /// Maximum number of searches, including the original query.
    pub max_queries: usize,
    /// Maximum number of ranked sources returned.
    pub max_sources: usize,
    /// Maximum number of ranked sources whose page content is fetched.
    pub max_pages: usize,
    /// Extraction output bound per fetched page.
    pub max_chars: usize,
    /// Filters forwarded to the search backend. `domains` doubles as the preferred-domain signal.
    pub search: SearchOptions,
    /// Bound on concurrent searches and concurrent page fetches.
    pub concurrency: usize,
    /// Bound on concurrent fetches sent to any single host.
    pub per_host_concurrency: usize,
    /// Wall-clock ceiling for the whole operation, not for one request.
    pub budget: Duration,
    /// Attempts per transient search or fetch failure, including the first try.
    pub attempts: usize,
}

/// Where a source's content came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ContentSource {
    /// Fetched during this operation.
    Network,
    /// Served from the durable page cache.
    Cache,
    /// Not fetched: outside the page budget, blocked by robots, or the fetch failed.
    None,
}

/// Explainable per-source ranking signals, all normalized to `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, schemars::JsonSchema)]
pub struct RankingSignals {
    /// Reciprocal of the best provider rank the URL reached across all queries.
    pub provider_rank: f64,
    /// Fraction of distinct question terms present in the title and snippet.
    pub term_coverage: f64,
    /// Fraction of executed queries that returned this URL.
    pub query_agreement: f64,
    /// Whether the host matches a caller-supplied domain.
    pub preferred_domain: f64,
    /// Diversity penalty already applied for earlier sources on the same host.
    pub host_repeat_penalty: f64,
}

/// One ranked source with provenance and optional fetched content.
#[derive(Debug, Clone, PartialEq, serde::Serialize, schemars::JsonSchema)]
pub struct DeepSearchSource {
    /// Final position, starting at 1.
    pub rank: usize,
    pub title: String,
    pub url: String,
    pub snippet: String,
    /// Best provider rank across every executed query, starting at 1.
    pub provider_rank: usize,
    /// Weighted score after the diversity penalty.
    pub score: f64,
    pub signals: RankingSignals,
    /// Extracted Markdown when the page was retrieved within budget.
    pub content: Option<String>,
    /// `sha256:` digest of the fetched response body.
    pub content_hash: Option<String>,
    /// Whether extraction hit the output bound.
    pub truncated: bool,
    pub content_source: ContentSource,
    /// URL of the higher ranked source carrying byte-identical content.
    pub duplicate_of: Option<String>,
}

/// Counters describing what the operation actually did.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub struct DeepSearchMeta {
    /// Backend that produced the first successful result set.
    pub provider: String,
    /// Every backend that answered, in the order they were used.
    pub providers_used: Vec<String>,
    /// Queries that returned results.
    pub queries_succeeded: usize,
    /// Distinct URLs seen before the source limit was applied.
    pub candidate_count: usize,
    /// Sources whose content came from the network.
    pub pages_fetched: usize,
    /// Sources whose content came from the durable cache.
    pub pages_from_cache: usize,
    /// Whether the wall-clock budget stopped work before the plan was complete.
    pub budget_exhausted: bool,
}

/// A partial failure that did not end the operation.
#[derive(Debug)]
pub enum ResearchWarning {
    /// One query failed on every backend while others succeeded.
    QueryFailed { query: String, error: SearchError },
    /// One ranked page could not be fetched.
    PageFailed { url: String, error: FetchError },
    /// One fetched page could not be extracted.
    PageNotExtractable { url: String, error: ExtractionError },
    /// The origin's robots rules forbid this client.
    RobotsDisallowed { url: String },
    /// The origin's robots rules could not be read, so the fetch was not attempted.
    RobotsUnavailable { url: String },
    /// The wall-clock budget ended the operation before every planned step ran.
    BudgetExhausted { stage: &'static str },
}

/// The source bundle returned to the caller. It contains sources, never an answer.
#[derive(Debug)]
pub struct DeepSearchBundle {
    pub query: String,
    /// Queries actually executed, in order.
    pub queries: Vec<String>,
    pub sources: Vec<DeepSearchSource>,
    pub warnings: Vec<ResearchWarning>,
    pub meta: DeepSearchMeta,
}

/// Deduplicated candidate assembled from every query's results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub title: String,
    pub url: String,
    pub snippet: String,
    /// Best (lowest) provider rank observed, starting at 1.
    pub best_rank: usize,
    /// Number of distinct queries that returned this URL.
    pub agreement: usize,
}

const WEIGHT_PROVIDER_RANK: f64 = 0.40;
const WEIGHT_TERM_COVERAGE: f64 = 0.35;
const WEIGHT_QUERY_AGREEMENT: f64 = 0.15;
const WEIGHT_PREFERRED_DOMAIN: f64 = 0.10;
/// Score reduction per earlier source already selected from the same host.
const HOST_REPEAT_PENALTY: f64 = 0.15;
/// Repeats beyond this count do not deepen the penalty, so a host is demoted, never banned.
const MAX_PENALIZED_REPEATS: usize = 3;
/// Upper bound on distinct question terms compared against titles and snippets.
const MAX_TERMS: usize = 24;
/// First backoff delay; each further attempt doubles it.
const RETRY_BASE_DELAY: Duration = Duration::from_millis(250);
/// Below this much remaining budget a new network attempt is not worth starting.
const MIN_ATTEMPT_BUDGET: Duration = Duration::from_millis(200);

// Freshness is a documented ranking signal in the specification but neither backend returns a
// publication date, so it is omitted rather than guessed from URL text.

/// Plan the queries to execute: the question first, then caller variants, deduplicated.
///
/// Variants are never invented from the question, because that would require a model.
#[must_use]
pub fn plan_queries(query: &str, variants: &[String], max_queries: usize) -> Vec<String> {
    let mut planned: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for candidate in std::iter::once(query).chain(variants.iter().map(String::as_str)) {
        if planned.len() >= max_queries.max(1) {
            break;
        }
        let trimmed = candidate.trim();
        let key = trimmed.to_lowercase();
        if trimmed.is_empty() || seen.contains(&key) {
            continue;
        }
        seen.push(key);
        planned.push(trimmed.to_owned());
    }
    planned
}

/// Split a question into distinct lowercase terms used for exact coverage scoring.
#[must_use]
pub fn question_terms(query: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    for term in query
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| term.chars().count() >= 2)
    {
        let term = term.to_lowercase();
        if !terms.contains(&term) {
            terms.push(term);
        }
        if terms.len() == MAX_TERMS {
            break;
        }
    }
    terms
}

/// Fraction of `terms` present in `text`. An empty term list scores zero rather than full marks.
fn term_coverage(terms: &[String], text: &str) -> f64 {
    if terms.is_empty() {
        return 0.0;
    }
    let haystack = text.to_lowercase();
    let matched = terms.iter().filter(|term| haystack.contains(*term)).count();
    matched as f64 / terms.len() as f64
}

/// Remove control characters from untrusted backend text and bound its length.
///
/// Terminal escape sequences and stray control bytes in a title or snippet are a display hazard
/// for whatever renders the bundle, so they are dropped at the boundary rather than downstream.
#[must_use]
pub fn sanitize(value: &str, max_chars: usize) -> String {
    let mut output = String::with_capacity(value.len().min(max_chars));
    for character in value.chars() {
        if output.chars().count() >= max_chars {
            break;
        }
        if character == '\n' || character == '\t' || !character.is_control() {
            output.push(character);
        }
    }
    output.trim().to_owned()
}

fn host_of(url: &str) -> Option<String> {
    Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
}

fn matches_preferred_domain(url: &str, domains: &[String]) -> bool {
    let Some(host) = host_of(url) else {
        return false;
    };
    domains.iter().any(|domain| {
        let domain = domain.trim().trim_start_matches('.').to_ascii_lowercase();
        !domain.is_empty() && (host == domain || host.ends_with(&format!(".{domain}")))
    })
}

/// Whether a search failure is worth another attempt or another backend.
#[must_use]
pub fn search_is_retryable(error: &SearchError) -> bool {
    match error {
        SearchError::Timeout(_) | SearchError::Transport(_) | SearchError::InvalidResponse(_) => {
            true
        }
        SearchError::Status(status) => {
            status.is_server_error() || status.as_u16() == 429 || status.as_u16() == 403
        }
        SearchError::NotConfigured
        | SearchError::InvalidUrl(_)
        | SearchError::ResponseTooLarge { .. } => false,
    }
}

/// Whether a fetch failure is worth another attempt.
///
/// Validation, size, and media-type failures are decisions, not accidents, so they are final.
#[must_use]
pub fn fetch_is_retryable(error: &FetchError) -> bool {
    match error {
        FetchError::Timeout(_) | FetchError::Transport(_) | FetchError::ReadBody(_) => true,
        FetchError::Status(status) => status.is_server_error() || status.as_u16() == 429,
        FetchError::InvalidUrl(_)
        | FetchError::BodyTooLarge { .. }
        | FetchError::MissingContentType
        | FetchError::InvalidContentType(_)
        | FetchError::Redirect(_)
        | FetchError::Destination(_) => false,
    }
}

/// One query's outcome: its results plus the backend that produced them.
type QueryOutcome = Result<(Vec<SearchResult>, &'static str), SearchError>;

/// Tracks the wall-clock ceiling shared by every stage of one operation.
#[derive(Debug, Clone, Copy)]
struct Budget {
    started: Instant,
    total: Duration,
}

impl Budget {
    fn new(total: Duration) -> Self {
        Self {
            started: Instant::now(),
            total,
        }
    }
    fn remaining(&self) -> Duration {
        self.total.saturating_sub(self.started.elapsed())
    }
    fn allows_attempt(&self) -> bool {
        self.remaining() > MIN_ATTEMPT_BUDGET
    }
}

/// Retry one bounded async operation while the failure is transient and budget remains.
async fn with_retry<T, E, F, Fut>(
    attempts: usize,
    budget: Budget,
    is_retryable: fn(&E) -> bool,
    mut operation: F,
) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    let mut delay = RETRY_BASE_DELAY;
    let mut last = operation().await;
    for _ in 1..attempts.max(1) {
        match &last {
            Ok(_) => return last,
            Err(error) if !is_retryable(error) => return last,
            Err(_) => {}
        }
        if !budget.allows_attempt() || delay >= budget.remaining() {
            return last;
        }
        tokio::time::sleep(delay).await;
        delay = delay.saturating_mul(2);
        last = operation().await;
    }
    last
}

/// Merge per-query results into deduplicated candidates, keeping the best provider rank.
///
/// URLs are already normalized and public-host validated by the search parsers, so merging keys
/// on the normalized URL string.
#[must_use]
pub fn collect_candidates(per_query: &[Vec<SearchResult>]) -> Vec<Candidate> {
    // Bounded nesting: at most `max_queries` result lists of at most `max_results` entries each,
    // and the inner merge is a hash lookup rather than a scan.
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut candidates: Vec<Candidate> = Vec::new();
    for results in per_query {
        for (position, result) in results.iter().enumerate() {
            let rank = position + 1;
            if let Some(&existing) = index.get(&result.url) {
                let candidate = &mut candidates[existing];
                candidate.agreement += 1;
                candidate.best_rank = candidate.best_rank.min(rank);
                if candidate.title.is_empty() {
                    candidate.title = sanitize(&result.title, 300);
                }
                if candidate.snippet.is_empty() {
                    candidate.snippet = sanitize(&result.content, 1_000);
                }
            } else {
                index.insert(result.url.clone(), candidates.len());
                candidates.push(Candidate {
                    title: sanitize(&result.title, 300),
                    url: result.url.clone(),
                    snippet: sanitize(&result.content, 1_000),
                    best_rank: rank,
                    agreement: 1,
                });
            }
        }
    }
    candidates
}

/// Rank candidates with explainable signals and a same-host diversity penalty.
///
/// Selection is greedy: the highest adjusted score wins each slot, and each already-selected
/// host lowers the remaining candidates from that host.
#[must_use]
pub fn rank_candidates(
    candidates: &[Candidate],
    terms: &[String],
    preferred_domains: &[String],
    executed_queries: usize,
    max_sources: usize,
) -> Vec<DeepSearchSource> {
    let queries = executed_queries.max(1) as f64;
    let mut scored: Vec<(f64, RankingSignals, &Candidate)> = candidates
        .iter()
        .map(|candidate| {
            let signals = RankingSignals {
                provider_rank: 1.0 / candidate.best_rank.max(1) as f64,
                term_coverage: term_coverage(
                    terms,
                    &format!("{} {}", candidate.title, candidate.snippet),
                ),
                query_agreement: (candidate.agreement as f64 / queries).min(1.0),
                preferred_domain: f64::from(u8::from(matches_preferred_domain(
                    &candidate.url,
                    preferred_domains,
                ))),
                host_repeat_penalty: 0.0,
            };
            let base = WEIGHT_PROVIDER_RANK * signals.provider_rank
                + WEIGHT_TERM_COVERAGE * signals.term_coverage
                + WEIGHT_QUERY_AGREEMENT * signals.query_agreement
                + WEIGHT_PREFERRED_DOMAIN * signals.preferred_domain;
            (base, signals, candidate)
        })
        .collect();
    // Ties resolve on provider rank then URL so that identical inputs always produce one order.
    scored.sort_by(|left, right| {
        right
            .0
            .partial_cmp(&left.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(left.2.best_rank.cmp(&right.2.best_rank))
            .then(left.2.url.cmp(&right.2.url))
    });

    // ponytail: O(sources x candidates) greedy selection; both are capped in the low tens.
    let mut host_counts: HashMap<String, usize> = HashMap::new();
    let mut remaining: Vec<usize> = (0..scored.len()).collect();
    let mut sources = Vec::with_capacity(max_sources.min(scored.len()));
    while sources.len() < max_sources && !remaining.is_empty() {
        let mut best_position = 0;
        let mut best_adjusted = f64::NEG_INFINITY;
        let mut best_penalty = 0.0;
        for (position, &index) in remaining.iter().enumerate() {
            let (base, _, candidate) = &scored[index];
            let repeats = host_of(&candidate.url)
                .and_then(|host| host_counts.get(&host).copied())
                .unwrap_or(0)
                .min(MAX_PENALIZED_REPEATS);
            let penalty = HOST_REPEAT_PENALTY * repeats as f64;
            let adjusted = base * (1.0 - penalty);
            if adjusted > best_adjusted {
                best_adjusted = adjusted;
                best_position = position;
                best_penalty = penalty;
            }
        }
        let index = remaining.remove(best_position);
        let (_, signals, candidate) = &scored[index];
        if let Some(host) = host_of(&candidate.url) {
            *host_counts.entry(host).or_insert(0) += 1;
        }
        let mut signals = *signals;
        signals.host_repeat_penalty = best_penalty;
        sources.push(DeepSearchSource {
            rank: sources.len() + 1,
            title: candidate.title.clone(),
            url: candidate.url.clone(),
            snippet: candidate.snippet.clone(),
            provider_rank: candidate.best_rank,
            score: best_adjusted,
            signals,
            content: None,
            content_hash: None,
            truncated: false,
            content_source: ContentSource::None,
            duplicate_of: None,
        });
    }
    sources
}

/// Mark sources whose fetched bytes are identical to a higher ranked source.
///
/// ponytail: exact body-hash equality only. Near-duplicate detection needs shingling or simhash;
/// add it when mirrors with cosmetic differences are shown to matter.
fn mark_duplicates(sources: &mut [DeepSearchSource]) {
    let mut first_by_hash: HashMap<String, String> = HashMap::new();
    for source in sources.iter_mut() {
        let Some(hash) = source.content_hash.clone() else {
            continue;
        };
        if let Some(original) = first_by_hash.get(&hash) {
            source.duplicate_of = Some(original.clone());
            source.content = None;
        } else {
            first_by_hash.insert(hash, source.url.clone());
        }
    }
}

/// Render a bundle as compact numbered text for callers with a tight context budget.
///
/// Signals, hashes, and scores are dropped; every block keeps its citation so the caller can
/// still attribute any statement it derives.
#[must_use]
pub fn render_compact(bundle: &DeepSearchBundle, max_chars: usize) -> String {
    let mut output = String::new();
    for source in &bundle.sources {
        let title = if source.title.is_empty() {
            "(untitled)"
        } else {
            source.title.as_str()
        };
        let block = format!("[{}] {}\n{}\n", source.rank, title, source.url);
        if output.chars().count() + block.chars().count() > max_chars {
            break;
        }
        output.push_str(&block);
        let body = source
            .content
            .as_deref()
            .filter(|content| !content.trim().is_empty())
            .unwrap_or(source.snippet.as_str());
        let remaining = max_chars.saturating_sub(output.chars().count());
        if remaining == 0 {
            break;
        }
        let body: String = body.chars().take(remaining).collect();
        output.push_str(body.trim());
        output.push_str("\n\n");
    }
    output.trim_end().to_owned()
}

/// External services one research operation may use.
pub struct DeepSearchDeps<'a> {
    /// Search backends in preference order; later entries are fallbacks.
    pub search: &'a [SearchClient],
    pub fetch: &'a FetchClient,
    pub robots: &'a RobotsGate,
    /// Durable page cache, or `None` to always fetch.
    pub cache: Option<&'a dyn PageStore>,
}

/// Run one bounded research operation.
///
/// Returns an error only when every planned query failed on every backend; any other failure
/// becomes a warning so that a partial bundle still reaches the caller.
pub async fn deep_search(
    deps: &DeepSearchDeps<'_>,
    request: &DeepSearchRequest,
) -> Result<DeepSearchBundle, SearchError> {
    let budget = Budget::new(request.budget);
    let queries = plan_queries(&request.query, &request.variants, request.max_queries);
    let mut warnings = Vec::new();

    let (per_query, providers_used) =
        run_searches(deps, request, &queries, budget, &mut warnings).await?;
    let candidates = collect_candidates(&per_query);
    let candidate_count = candidates.len();
    let mut sources = rank_candidates(
        &candidates,
        &question_terms(&request.query),
        &request.search.domains,
        queries.len(),
        request.max_sources,
    );

    let counts = retrieve_pages(deps, request, budget, &mut sources, &mut warnings).await;
    if counts.budget_exhausted {
        warnings.push(ResearchWarning::BudgetExhausted { stage: "fetch" });
    }
    mark_duplicates(&mut sources);

    let provider = providers_used
        .first()
        .cloned()
        .unwrap_or_else(|| "unknown".to_owned());
    Ok(DeepSearchBundle {
        query: request.query.clone(),
        meta: DeepSearchMeta {
            provider,
            providers_used,
            queries_succeeded: per_query.len(),
            candidate_count,
            pages_fetched: counts.fetched,
            pages_from_cache: counts.from_cache,
            budget_exhausted: counts.budget_exhausted,
        },
        queries,
        sources,
        warnings,
    })
}

/// Search every planned query concurrently and collect results in planned order.
///
/// Fails only when no query succeeded on any backend, because a bundle without a single result
/// set has nothing to rank or explain.
async fn run_searches(
    deps: &DeepSearchDeps<'_>,
    request: &DeepSearchRequest,
    queries: &[String],
    budget: Budget,
    warnings: &mut Vec<ResearchWarning>,
) -> Result<(Vec<Vec<SearchResult>>, Vec<String>), SearchError> {
    let concurrency = request.concurrency.max(1);
    let planned: Vec<(usize, String)> = queries.iter().cloned().enumerate().collect();
    let mut responses: Vec<(usize, QueryOutcome)> = stream::iter(planned)
        .map(|(index, query)| async move {
            (
                index,
                search_one(
                    deps.search,
                    &query,
                    &request.search,
                    request.attempts,
                    budget,
                )
                .await,
            )
        })
        .buffer_unordered(concurrency)
        .collect()
        .await;
    // Concurrency must not reorder candidates, so results return to planned query order.
    responses.sort_by_key(|(index, _)| *index);

    let mut per_query = Vec::new();
    let mut providers_used: Vec<String> = Vec::new();
    let mut first_error = None;
    for (index, response) in responses {
        match response {
            Ok((results, provider)) => {
                if !providers_used.iter().any(|used| used == provider) {
                    providers_used.push(provider.to_owned());
                }
                per_query.push(results);
            }
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(index);
                }
                warnings.push(ResearchWarning::QueryFailed {
                    query: queries[index].clone(),
                    error,
                });
            }
        }
    }
    if per_query.is_empty() {
        // Hand back the first failure itself rather than an empty success.
        for warning in std::mem::take(warnings) {
            if let ResearchWarning::QueryFailed { error, .. } = warning {
                return Err(error);
            }
        }
        return Err(SearchError::InvalidResponse(
            "no query could be planned from the request".to_owned(),
        ));
    }
    Ok((per_query, providers_used))
}

/// Counters produced by the page-retrieval stage.
#[derive(Debug, Default, Clone, Copy)]
struct RetrievalCounts {
    fetched: usize,
    from_cache: usize,
    budget_exhausted: bool,
}

/// Fill the top ranked sources with content from the cache, then from the network.
///
/// Fetches run under the global concurrency limit and a per-host limit so that one busy host
/// cannot consume the whole budget or hammer a single server.
async fn retrieve_pages(
    deps: &DeepSearchDeps<'_>,
    request: &DeepSearchRequest,
    budget: Budget,
    sources: &mut [DeepSearchSource],
    warnings: &mut Vec<ResearchWarning>,
) -> RetrievalCounts {
    let mut counts = RetrievalCounts::default();
    let mut pending: Vec<(usize, String)> = Vec::new();
    for (index, source) in sources.iter_mut().take(request.max_pages).enumerate() {
        match deps
            .cache
            .and_then(|cache| cache.get(&source.url, request.max_chars))
        {
            Some(page) => {
                counts.from_cache += 1;
                apply_page(source, &page, ContentSource::Cache);
            }
            None => pending.push((index, source.url.clone())),
        }
    }

    let concurrency = request.concurrency.max(1);
    let host_limits = host_semaphores(&pending, request.per_host_concurrency.max(1));
    let global = Arc::new(Semaphore::new(concurrency));
    let fetched: Vec<(usize, String, PageOutcome)> = stream::iter(pending)
        .map(|(index, url)| {
            let global = Arc::clone(&global);
            let host_limit = host_of(&url).and_then(|host| host_limits.get(&host).cloned());
            async move {
                if !budget.allows_attempt() {
                    return (index, url, PageOutcome::OutOfBudget);
                }
                let _global = global.acquire().await;
                let _host = match &host_limit {
                    Some(limit) => Some(limit.acquire().await),
                    None => None,
                };
                let outcome = retrieve_page(deps, &url, request, budget).await;
                (index, url, outcome)
            }
        })
        .buffer_unordered(concurrency)
        .collect()
        .await;

    for (index, url, outcome) in fetched {
        match outcome {
            PageOutcome::Retrieved(page) => {
                counts.fetched += 1;
                if let Some(cache) = deps.cache {
                    cache.put(&url, request.max_chars, &page);
                }
                apply_page(&mut sources[index], &page, ContentSource::Network);
            }
            PageOutcome::Fetch(error) => warnings.push(ResearchWarning::PageFailed { url, error }),
            PageOutcome::Extraction(error) => {
                warnings.push(ResearchWarning::PageNotExtractable { url, error });
            }
            PageOutcome::RobotsDisallowed => {
                warnings.push(ResearchWarning::RobotsDisallowed { url });
            }
            PageOutcome::RobotsUnavailable => {
                warnings.push(ResearchWarning::RobotsUnavailable { url });
            }
            PageOutcome::OutOfBudget => counts.budget_exhausted = true,
        }
    }
    counts
}

fn apply_page(source: &mut DeepSearchSource, page: &CachedPage, origin: ContentSource) {
    source.content = Some(page.markdown.clone());
    source.content_hash = Some(page.content_hash.clone());
    source.truncated = page.truncated;
    source.content_source = origin;
    if source.title.is_empty()
        && let Some(title) = &page.title
    {
        source.title = sanitize(title, 300);
    }
}

fn host_semaphores(
    pending: &[(usize, String)],
    per_host: usize,
) -> HashMap<String, Arc<Semaphore>> {
    let mut limits: HashMap<String, Arc<Semaphore>> = HashMap::new();
    for (_, url) in pending {
        if let Some(host) = host_of(url) {
            limits
                .entry(host)
                .or_insert_with(|| Arc::new(Semaphore::new(per_host)));
        }
    }
    limits
}

/// Search one query with retry and backend fallback under a fresh wall-clock budget.
///
/// Every search-backed tool routes through here, so a blocked or flaky backend degrades result
/// quality in one place instead of failing each tool differently. Returns the results and the
/// backend that actually produced them, which is the provenance callers must report.
pub async fn search_with_fallback(
    clients: &[SearchClient],
    query: &str,
    options: &SearchOptions,
    attempts: usize,
    budget: Duration,
) -> Result<(Vec<SearchResult>, &'static str), SearchError> {
    search_one(clients, query, options, attempts, Budget::new(budget)).await
}

/// Search one query, falling back to the next backend when a failure looks transient or blocked.
async fn search_one(
    clients: &[SearchClient],
    query: &str,
    options: &SearchOptions,
    attempts: usize,
    budget: Budget,
) -> Result<(Vec<SearchResult>, &'static str), SearchError> {
    let mut last = Err(SearchError::NotConfigured);
    for client in clients {
        if !budget.allows_attempt() {
            break;
        }
        let attempt = with_retry(attempts, budget, search_is_retryable, || {
            client.search_with_options(query, options)
        })
        .await;
        match attempt {
            Ok(results) => return Ok((results, client.provider())),
            Err(error) => {
                let retryable = search_is_retryable(&error);
                last = Err(error);
                if !retryable {
                    return last;
                }
            }
        }
    }
    last
}

enum PageOutcome {
    Retrieved(CachedPage),
    Fetch(FetchError),
    Extraction(ExtractionError),
    RobotsDisallowed,
    RobotsUnavailable,
    OutOfBudget,
}

async fn retrieve_page(
    deps: &DeepSearchDeps<'_>,
    url: &str,
    request: &DeepSearchRequest,
    budget: Budget,
) -> PageOutcome {
    let Ok(parsed) = Url::parse(url) else {
        return PageOutcome::RobotsUnavailable;
    };
    let delay = match deps.robots.check(&parsed).await {
        RobotsDecision::Allowed { delay } => delay,
        RobotsDecision::Disallowed => return PageOutcome::RobotsDisallowed,
        RobotsDecision::Unavailable => return PageOutcome::RobotsUnavailable,
    };
    if let Some(origin) = origin_key(&parsed) {
        deps.robots.wait_for_host(&origin, delay).await;
    }
    if !budget.allows_attempt() {
        return PageOutcome::OutOfBudget;
    }
    let fetched = match with_retry(request.attempts, budget, fetch_is_retryable, || {
        deps.fetch.get(url)
    })
    .await
    {
        Ok(fetched) => fetched,
        Err(error) => return PageOutcome::Fetch(error),
    };
    let document = match extract(
        &fetched.body,
        &fetched.content_type,
        Some(&fetched.url),
        ExtractionOptions {
            max_chars: request.max_chars,
        },
    ) {
        Ok(document) => document,
        Err(error) => return PageOutcome::Extraction(error),
    };
    let mut hasher = Sha256::new();
    hasher.update(&fetched.body);
    PageOutcome::Retrieved(CachedPage {
        final_url: fetched.url,
        title: document.title.map(|title| sanitize(&title, 300)),
        markdown: sanitize(&document.markdown, request.max_chars),
        content_hash: format!("sha256:{:x}", hasher.finalize()),
        truncated: document.truncated,
    })
}

#[cfg(test)]
mod e2e_tests;

#[cfg(test)]
mod tests {
    use super::{
        Budget, CachedPage, Candidate, ContentSource, DeepSearchBundle, DeepSearchMeta,
        ResearchWarning, collect_candidates, fetch_is_retryable, mark_duplicates,
        matches_preferred_domain, plan_queries, question_terms, rank_candidates, render_compact,
        sanitize, search_is_retryable, term_coverage, with_retry,
    };
    use crate::fetch::{FetchError, search::SearchError, search::SearchResult};
    use reqwest::StatusCode;
    use std::{
        cell::Cell,
        time::{Duration, Instant},
    };

    fn result(url: &str, title: &str, content: &str) -> SearchResult {
        SearchResult {
            title: title.to_owned(),
            url: url.to_owned(),
            content: content.to_owned(),
            engine: None,
        }
    }

    #[test]
    fn plans_question_first_and_drops_duplicate_variants() {
        let variants = vec![
            "  Rust MCP  ".to_owned(),
            "rust mcp".to_owned(),
            String::new(),
            "rust sdk".to_owned(),
        ];
        assert_eq!(
            plan_queries("rust mcp", &variants, 3),
            vec!["rust mcp".to_owned(), "rust sdk".to_owned()]
        );
        assert_eq!(plan_queries("only", &variants, 1), vec!["only".to_owned()]);
        // A zero ceiling still plans the question rather than searching nothing.
        assert_eq!(plan_queries("only", &[], 0), vec!["only".to_owned()]);
    }

    #[test]
    fn terms_are_distinct_lowercase_and_skip_single_characters() {
        assert_eq!(
            question_terms("Rust MCP, rust a server?"),
            vec!["rust".to_owned(), "mcp".to_owned(), "server".to_owned()]
        );
        assert!(
            (term_coverage(&question_terms("rust mcp"), "Rust guide") - 0.5).abs() < f64::EPSILON
        );
        assert!(term_coverage(&[], "anything").abs() < f64::EPSILON);
    }

    #[test]
    fn sanitize_strips_control_bytes_and_bounds_length() {
        assert_eq!(sanitize("  a\u{1b}[31mred\u{0}  ", 100), "a[31mred");
        assert_eq!(sanitize("line\nnext\tcell", 100), "line\nnext\tcell");
        assert_eq!(sanitize("abcdef", 3), "abc");
    }

    #[test]
    fn candidates_deduplicate_and_keep_the_best_provider_rank() {
        let first = vec![
            result("https://a.example/1", "A", "first"),
            result("https://b.example/2", "B", "second"),
        ];
        let second = vec![
            result("https://c.example/3", "C", "third"),
            result("https://a.example/1", "", ""),
        ];
        let candidates = collect_candidates(&[first, second]);
        assert_eq!(candidates.len(), 3);
        let merged = &candidates[0];
        assert_eq!(merged.url, "https://a.example/1");
        assert_eq!(merged.best_rank, 1);
        assert_eq!(merged.agreement, 2);
        // The empty repeat must not erase the title captured on first sight.
        assert_eq!(merged.title, "A");
    }

    fn candidate(url: &str, title: &str, best_rank: usize, agreement: usize) -> Candidate {
        Candidate {
            title: title.to_owned(),
            url: url.to_owned(),
            snippet: String::new(),
            best_rank,
            agreement,
        }
    }

    #[test]
    fn ranking_prefers_agreement_and_term_coverage_over_raw_provider_rank() {
        let candidates = vec![
            candidate("https://one.example/a", "unrelated page", 1, 1),
            candidate("https://two.example/b", "rust mcp server guide", 2, 2),
        ];
        let ranked = rank_candidates(&candidates, &question_terms("rust mcp server"), &[], 2, 5);
        assert_eq!(ranked[0].url, "https://two.example/b");
        assert!((ranked[0].signals.term_coverage - 1.0).abs() < f64::EPSILON);
        assert!((ranked[0].signals.query_agreement - 1.0).abs() < f64::EPSILON);
        assert_eq!(ranked[1].rank, 2);
    }

    #[test]
    fn repeated_hosts_are_demoted_but_not_removed() {
        let candidates = vec![
            candidate("https://same.example/a", "rust mcp", 1, 1),
            candidate("https://same.example/b", "rust mcp", 2, 1),
            candidate("https://other.example/c", "rust mcp", 3, 1),
        ];
        let ranked = rank_candidates(&candidates, &question_terms("rust mcp"), &[], 1, 3);
        assert_eq!(ranked.len(), 3);
        assert_eq!(ranked[0].url, "https://same.example/a");
        // The second same-host page loses to a lower provider rank on a fresh host.
        assert_eq!(ranked[1].url, "https://other.example/c");
        assert_eq!(ranked[2].url, "https://same.example/b");
        assert!(ranked[2].signals.host_repeat_penalty > 0.0);
    }

    #[test]
    fn preferred_domains_match_the_host_and_its_subdomains_only() {
        let domains = vec!["example.com".to_owned()];
        assert!(matches_preferred_domain("https://example.com/a", &domains));
        assert!(matches_preferred_domain(
            "https://docs.example.com/a",
            &domains
        ));
        assert!(!matches_preferred_domain(
            "https://notexample.com/a",
            &domains
        ));
        assert!(!matches_preferred_domain(
            "https://example.com.evil.net/a",
            &domains
        ));
        assert!(!matches_preferred_domain("https://example.com/a", &[]));
    }

    #[test]
    fn source_limit_bounds_the_bundle() {
        let candidates: Vec<_> = (0..20)
            .map(|index| {
                candidate(
                    &format!("https://host{index}.example/a"),
                    "page",
                    index + 1,
                    1,
                )
            })
            .collect();
        assert_eq!(
            rank_candidates(&candidates, &question_terms("page"), &[], 1, 5).len(),
            5
        );
    }

    #[test]
    fn retry_classification_separates_transient_failures_from_decisions() {
        assert!(search_is_retryable(&SearchError::Timeout("x".to_owned())));
        assert!(search_is_retryable(&SearchError::Status(
            StatusCode::TOO_MANY_REQUESTS
        )));
        // A blocked built-in backend is worth trying on another backend.
        assert!(search_is_retryable(&SearchError::Status(
            StatusCode::FORBIDDEN
        )));
        assert!(!search_is_retryable(&SearchError::NotConfigured));
        assert!(fetch_is_retryable(&FetchError::Status(
            StatusCode::BAD_GATEWAY
        )));
        assert!(!fetch_is_retryable(&FetchError::MissingContentType));
        assert!(!fetch_is_retryable(&FetchError::BodyTooLarge { limit: 1 }));
    }

    #[tokio::test(start_paused = true)]
    async fn retry_stops_on_success_on_final_failures_and_on_an_exhausted_budget() {
        let calls = Cell::new(0);
        let recovered: Result<u8, FetchError> = with_retry(
            3,
            Budget::new(Duration::from_secs(30)),
            fetch_is_retryable,
            || {
                calls.set(calls.get() + 1);
                let attempt = calls.get();
                async move {
                    if attempt < 3 {
                        Err(FetchError::Timeout("slow".to_owned()))
                    } else {
                        Ok(7)
                    }
                }
            },
        )
        .await;
        assert_eq!(recovered.unwrap(), 7);
        assert_eq!(calls.get(), 3);

        let calls = Cell::new(0);
        let final_failure: Result<u8, FetchError> = with_retry(
            5,
            Budget::new(Duration::from_secs(30)),
            fetch_is_retryable,
            || {
                calls.set(calls.get() + 1);
                async { Err(FetchError::MissingContentType) }
            },
        )
        .await;
        assert!(final_failure.is_err());
        // A permanent failure is not retried at all.
        assert_eq!(calls.get(), 1);

        let calls = Cell::new(0);
        let spent = Budget {
            started: Instant::now()
                .checked_sub(Duration::from_secs(60))
                .expect("test clock supports a one minute offset"),
            total: Duration::from_secs(30),
        };
        let out_of_budget: Result<u8, FetchError> =
            with_retry(5, spent, fetch_is_retryable, || {
                calls.set(calls.get() + 1);
                async { Err(FetchError::Timeout("slow".to_owned())) }
            })
            .await;
        assert!(out_of_budget.is_err());
        // The first attempt still runs; the budget only stops further ones.
        assert_eq!(calls.get(), 1);
    }

    fn source_with_content(
        rank: usize,
        url: &str,
        hash: &str,
        body: &str,
    ) -> super::DeepSearchSource {
        let mut ranked = rank_candidates(
            &[candidate(url, "title", rank, 1)],
            &question_terms("title"),
            &[],
            1,
            1,
        );
        let source = &mut ranked[0];
        source.rank = rank;
        source.content = Some(body.to_owned());
        source.content_hash = Some(hash.to_owned());
        source.content_source = ContentSource::Network;
        ranked.remove(0)
    }

    #[test]
    fn identical_bodies_keep_only_the_highest_ranked_copy() {
        let mut sources = vec![
            source_with_content(1, "https://a.example/x", "sha256:same", "body"),
            source_with_content(2, "https://mirror.example/x", "sha256:same", "body"),
            source_with_content(3, "https://c.example/y", "sha256:other", "different"),
        ];
        mark_duplicates(&mut sources);
        assert_eq!(sources[0].duplicate_of, None);
        assert_eq!(
            sources[1].duplicate_of.as_deref(),
            Some("https://a.example/x")
        );
        // The duplicate keeps its citation and drops only the repeated bytes.
        assert!(sources[1].content.is_none());
        assert_eq!(sources[2].duplicate_of, None);
        assert!(sources[2].content.is_some());
    }

    #[test]
    fn compact_rendering_cites_every_block_and_respects_the_char_budget() {
        let mut sources = vec![
            source_with_content(1, "https://a.example/x", "sha256:a", "first body"),
            source_with_content(2, "https://b.example/y", "sha256:b", "second body"),
        ];
        sources[1].content = None;
        sources[1].snippet = "snippet fallback".to_owned();
        let bundle = DeepSearchBundle {
            query: "q".to_owned(),
            queries: vec!["q".to_owned()],
            sources,
            warnings: vec![ResearchWarning::BudgetExhausted { stage: "fetch" }],
            meta: DeepSearchMeta {
                provider: "duckduckgo".to_owned(),
                providers_used: vec!["duckduckgo".to_owned()],
                queries_succeeded: 1,
                candidate_count: 2,
                pages_fetched: 1,
                pages_from_cache: 0,
                budget_exhausted: true,
            },
        };
        let rendered = render_compact(&bundle, 4_000);
        assert!(rendered.contains("[1] title"));
        assert!(rendered.contains("https://a.example/x"));
        assert!(rendered.contains("first body"));
        // A source without fetched content still contributes its snippet.
        assert!(rendered.contains("snippet fallback"));
        assert!(render_compact(&bundle, 40).chars().count() <= 40);
    }

    #[test]
    fn cached_pages_are_applied_without_refetching() {
        let mut source = source_with_content(1, "https://a.example/x", "sha256:a", "body");
        source.content = None;
        source.content_hash = None;
        source.title = String::new();
        super::apply_page(
            &mut source,
            &CachedPage {
                final_url: "https://a.example/x".to_owned(),
                title: Some("Cached title".to_owned()),
                markdown: "cached body".to_owned(),
                content_hash: "sha256:cached".to_owned(),
                truncated: true,
            },
            ContentSource::Cache,
        );
        assert_eq!(source.content.as_deref(), Some("cached body"));
        assert_eq!(source.content_source, ContentSource::Cache);
        assert_eq!(source.title, "Cached title");
        assert!(source.truncated);
    }
}
