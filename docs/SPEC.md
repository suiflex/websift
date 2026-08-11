# Websift — Product and Technical Specification

Status: **product contract and target roadmap; partial implementation**  
Last updated: 2026-08-11

> **Current implementation snapshot.** Configuration/profile handling, public URL policy primitives, bounded native HTTP retrieval, HTML/plain-text extraction, zero-configuration search through the built-in backend with optional SearXNG, static mapping, crawl lifecycle/background execution with robots checks, worker extraction mode, durable SQLite state, released binaries with platform installers, and checksum-verified `websift update` are implemented. Playwright/Chromium rendering, full scheduler leases/concurrency/retries/resume, the remaining management CLI, and complete worker artifact integration remain gaps. The current worker advertises extraction only; rendering and screenshot formats are not worker capabilities. This specification intentionally retains target behavior for those gaps.

## 1. Product definition

Websift is a self-hostable, open-source web retrieval and crawling layer for AI agents. It gives agent harnesses a consistent way to search, scrape, map, and crawl the public web without requiring a paid search provider.

The product is not a new internet-scale search index. It sits between an agent and existing retrieval sources, beginning with a user-supplied SearXNG instance, and returns bounded, attributable data that the calling agent can reason over.

Initial positioning:

> Give any AI agent safe, source-attributed web search and production-grade crawling.

## 2. Goals and non-goals

### Goals

- Provide a simple local mode while allowing browser workers and durable crawl jobs when advanced crawling is enabled.
- Install without a Rust toolchain, Node.js setup, database server, or manual agent configuration.
- Expose the same core behavior through an MCP stdio server and CLI.
- Work with zero configuration: a built-in keyless backend answers search out of the box, and a self-hosted SearXNG instance is an optional privacy upgrade rather than a prerequisite.
- Scrape static and JavaScript-rendered pages and extract high-quality, LLM-ready content.
- Discover, schedule, resume, inspect, and cancel bounded crawl jobs.
- Return stable, machine-readable results with source provenance.
- Bound requests, response sizes, redirects, concurrency, per-host pressure, and crawl scope.
- Keep the core independent from MCP so another transport can be added without rewriting retrieval.

### Non-goals for v1

- Building or operating a web-scale index.
- Generating an answer or claiming factual confidence; the caller owns synthesis.
- CAPTCHA bypass, paywall bypass, exploit-oriented stealth, or ignoring site access controls.
- Paid-provider routing, semantic/vector reranking, SDKs, REST, or remote MCP transport.
- Unbounded crawling or mirroring websites.

## 3. Delivery scope

### Phase 1: useful vertical slice

Target scope (partially implemented; see the implementation snapshot above):

1. `search` — query the built-in backend, or a configured SearXNG endpoint when one is set.
2. `scrape` — safely retrieve one page, extract Markdown and links, and fall back to browser rendering when requested or required.
3. `map` — discover normalized URLs from a site without scraping every page.
4. CLI commands and MCP stdio tools backed by the same core.

Phase 1 proves provider integration, static/dynamic extraction quality, URL discovery, agent usability, and the security boundary before multi-page orchestration is added.

### Phase 2: production crawler

Target scope; crawl lifecycle/background execution and durable SQLite storage exist, but the full scheduler contract below is not yet complete.

- `find` — locate literal text within an already fetched document.
- Asynchronous crawl jobs with status, pagination, cancellation, resumability, and partial results.
- Sitemap and HTML-link discovery with precise include/exclude/scope controls.
- Per-host scheduling, retries, rate limits, content deduplication, and incremental recrawl.
- Multiple scrape formats: Markdown, cleaned HTML, raw HTML, links, images, metadata, and screenshots.
- `deep_search` — deterministic search/fetch/dedup/rank bundle; it does not call an LLM.
- Durable job state and content cache with explicit TTL and cache metadata.

### Later, only when measured demand exists

- Optional Brave, Tavily, or Exa fallback.
- Specialized adapters for documentation, GitHub, papers, packages, news, or forums.
- Streamable HTTP MCP, REST, and language SDKs.
- PDF parsing, schema-guided JSON extraction, browser action sequences, and signed webhooks.
- Learned or embedding-based reranking.

## 4. Architecture

```text
Agent / shell
    │
    ├── MCP stdio adapter ─┐
    └── CLI adapter ───────┤
                           ▼
                    retrieval core
       ┌────────────┬──────┴──────┬─────────────┐
       ▼            ▼             ▼             ▼
 SearXNG search  URL policy  crawl scheduler  job/cache store
                                  │
                           ┌──────┴──────┐
                           ▼             ▼
                     HTTP worker   browser worker
                           └──────┬──────┘
                                  ▼
                     extraction + provenance
```

Adapters only translate inputs and outputs. URL policy, scheduling, timeouts, deduplication, extraction, and error mapping live in the core and therefore apply equally to CLI and MCP callers.

Implementation direction: Rust owns MCP, CLI, policy, scheduling, native HTTP, and embedded state. A narrow TypeScript worker owns Playwright, DOM extraction, and browser artifacts. They communicate through a versioned local protocol; the worker is not a second public service. Prebuilt native packages are the primary distribution, with a pinned container image for self-hosted deployments. See [ARCHITECTURE.md](ARCHITECTURE.md) and [INSTALLATION.md](INSTALLATION.md).

## 5. Configuration

Configuration is environment-first so the same binary works under MCP launchers and shells. **No variable is required.** Every value below has a working default, and installers write the variables they set into the client's MCP configuration so a user never edits an environment by hand.

### Optional overrides

| Variable | Default | Meaning |
| --- | --- | --- |
| `WEBSIFT_SEARXNG_URL` | none | Base URL of a trusted SearXNG instance. When set it replaces the built-in backend; when unset search still works |
| `WEBSIFT_TIMEOUT` | `10s` | Total timeout per outbound request |
| `WEBSIFT_MAX_RESULTS` | `10` | Maximum search results; hard ceiling `50` |
| `WEBSIFT_MAX_BYTES` | `2000000` | Maximum compressed or decoded response bytes, whichever limit is reached first |
| `WEBSIFT_CRAWL_CONCURRENCY` | `4` | Global page-worker limit; hard ceiling `32` |
| `WEBSIFT_PER_HOST_CONCURRENCY` | `2` | Per-host request limit |
| `WEBSIFT_BROWSER` | `auto` | `auto`, `enabled`, or `disabled` browser-worker policy |
| `WEBSIFT_DATA_DIR` | platform data directory | Advanced override for automatically managed state and artifacts |
| `WEBSIFT_PROFILE` | `default` | Local visibility namespace; client installers set `codex`, `claude-code`, `hermes`, or `openclaw` |

### Internal variables

These exist for development, testing, and packaging. They are not part of the user-facing contract and may change without notice: `WEBSIFT_TIMEOUT_MS` (numeric alias for `WEBSIFT_TIMEOUT`), `WEBSIFT_SPOOL_ROOT`, `WEBSIFT_WORKER_PROGRAM`, and `WEBSIFT_WORKER_ARGS`.

The outbound user agent is a fixed `websift/<version>` identifier rather than a configurable value, so the client stays identifiable in robots rules and server logs and cannot be pointed at browser impersonation.

Secrets must not appear in tool results or logs. Paid-provider keys are intentionally absent from v1.

## 6. Tool contracts

All tool output uses JSON-compatible objects. Unknown input fields are rejected. Strings are trimmed; empty queries and URLs are rejected.

### `websift_status`

Takes no meaningful input and performs no network request. It returns the running `version` and the active `profile` so a caller can confirm which installation and namespace it is talking to.

### `web_search`

Input:

```json
{
  "query": "PostgreSQL 18 async I/O benchmark",
  "limit": 10,
  "language": "en",
  "time_range": "month",
  "domains": ["postgresql.org"]
}
```

- `query` is required, 1–500 Unicode characters.
- `limit` is optional, 1–50.
- `language` and `time_range` are translated per backend and passed only when the backend supports them.
- `domains` is optional, maximum 20 normalized hostnames. v1 translates it into query constraints and still validates returned hosts; it is not a security boundary.

Output:

```json
{
  "query": "PostgreSQL 18 async I/O benchmark",
  "results": [
    {
      "title": "Example title",
      "url": "https://example.com/article",
      "snippet": "Result snippet",
      "published_at": null,
      "source": "duckduckgo",
      "rank": 1
    }
  ],
  "meta": {
    "provider": "duckduckgo",
    "result_count": 1,
    "truncated": false,
    "duration_ms": 120
  }
}
```

Results are deduplicated by normalized final URL while preserving provider order. `rank` is positional, not a universal relevance probability. `source` and `provider` name the backend that actually served the request: `duckduckgo` for the built-in backend, `searxng` when an instance is configured. A backend response that cannot be parsed is reported as `provider_unavailable` instead of an empty result list.

### `web_scrape`

Input:

```json
{
  "url": "https://example.com/article",
  "formats": ["markdown", "links"],
  "render": "auto",
  "only_main_content": true,
  "wait_for_ms": 0,
  "max_chars": 30000
}
```

- `url` is required and must be public HTTP or HTTPS.
- `formats` accepts `markdown`, `clean_html`, `raw_html`, `links`, `images`, `metadata`, and `screenshot`; Phase 1 requires `markdown`, `links`, and `metadata`.
- `render` accepts `auto`, `never`, or `always`. `auto` starts with HTTP and escalates once when the response is a JavaScript shell, has too little meaningful content, or explicitly signals client rendering.
- `only_main_content` removes navigation, repeated chrome, cookie banners, ads, scripts, and styles when possible.
- `wait_for_ms` is bounded and only applies to browser rendering.
- `max_chars` is optional, 1–100000, and truncates extracted output without bypassing byte limits.

Output:

```json
{
  "url": "https://example.com/article",
  "final_url": "https://www.example.com/article",
  "title": "Example title",
  "content_type": "text/html",
  "markdown": "# Example title\n\nReadable page text...",
  "links": ["https://www.example.com/next"],
  "rendered_with": "http",
  "fetched_at": "2026-08-10T00:00:00Z",
  "truncated": false,
  "content_hash": "sha256:...",
  "attribution": {
    "source_url": "https://www.example.com/article"
  }
}
```

Supported Phase 1 media types are `text/html`, `text/plain`, and `application/xhtml+xml`. Other types return `unsupported_content_type`; PDFs and binary documents are deferred.

### `web_map`

Discovers URLs from the start page, sitemap indexes, sitemaps, and optionally bounded HTML traversal. It returns normalized URLs and discovery metadata without extracting every page.

```json
{
  "url": "https://docs.example.com",
  "limit": 5000,
  "include_paths": ["/guides/**"],
  "exclude_paths": ["/changelog/**"],
  "include_subdomains": false,
  "use_sitemap": true
}
```

### Crawl job tools

- `web_crawl_start` validates the complete job, persists it, and returns `job_id`, `status`, and resolved budgets.
- `web_crawl_status` returns counters, warnings, terminal reason, and a pagination cursor for completed page results.
- `web_crawl_cancel` is idempotent and stops scheduling new pages; in-flight pages may finish and remain available.
- `web_crawl_results` returns a stable page of documents and per-URL failures without requiring the entire crawl in one MCP response.

```json
{
  "url": "https://docs.example.com",
  "limit": 1000,
  "max_depth": 5,
  "max_duration_seconds": 1800,
  "include_paths": ["/**"],
  "exclude_paths": ["/account/**"],
  "allow_subdomains": false,
  "allow_external_links": false,
  "ignore_query_parameters": true,
  "sitemap": "include",
  "render": "auto",
  "formats": ["markdown", "links", "metadata"]
}
```

Job states are `queued`, `running`, `completed`, `failed`, and `cancelled`. A terminal job may contain partial successes and per-page failures. Restarting the service resumes non-terminal jobs without refetching pages whose content was already committed.

## 7. Error contract

Failures are data, not partial success disguised as an empty result.

```json
{
  "error": {
    "code": "timeout",
    "message": "Upstream request exceeded 10s",
    "retryable": true
  }
}
```

Stable codes:

- `invalid_input`
- `invalid_url`
- `blocked_url`
- `robots_disallowed`
- `timeout`
- `response_too_large`
- `unsupported_content_type`
- `provider_unavailable`
- `provider_rejected_format`
- `search_not_configured`
- `rate_limited`
- `extraction_failed`
- `browser_unavailable`
- `render_failed`
- `crawl_budget_exhausted`
- `job_not_found`
- `internal_error`

Upstream bodies, credentials, stack traces, and local network details must not be exposed to tool callers.

## 8. Security and web policy

Every fetched URL, including every redirect target, must pass the same check:

- Allow only `http` and `https`; reject embedded credentials and malformed hosts.
- Resolve DNS and reject loopback, private, link-local, multicast, unspecified, reserved, and cloud metadata destinations for IPv4 and IPv6.
- Pin or revalidate the connected address to prevent DNS rebinding.
- Limit redirects to 5 and reject HTTPS-to-HTTP downgrade by default.
- Apply total timeout, connection timeout, response-byte limit, and bounded concurrency.
- Never forward caller credentials, cookies, proxy credentials, or arbitrary headers. A later authenticated-scrape feature must use named, host-scoped secrets configured outside tool arguments.
- Treat downloaded content as untrusted data. Never execute scripts, follow instructions found in page text, or render raw HTML in a trusted UI.
- Honor `robots.txt` for automated crawl operations and cache it within RFC bounds. Single user-directed scrapes still obey network safety and site terms; crawl defaults to deny when robots policy cannot be established after transient server errors.
- Identify the client with a configurable user agent and avoid retry storms.
- Run browser workers with process, filesystem, network, memory, and execution-time isolation. Browser actions never execute caller-supplied JavaScript.

MCP v1 uses stdio only. stdout contains protocol messages only; diagnostics go to stderr with URL query strings and secrets redacted.

## 9. Crawler quality bar

“As capable as Firecrawl” means comparable crawling primitives and output quality for public websites, not copying its API or promising universal anti-bot bypass. The target is measured by the following behaviors.

### Discovery and scope

- Seed the frontier from the requested URL, sitemap and sitemap indexes, and links found in fetched pages.
- Normalize scheme/host casing, default ports, fragments, dot segments, and safe percent encoding before deduplication.
- Respect canonical URLs while retaining the requested and final URL for provenance.
- Support include/exclude path globs, query-parameter policy, subdomain policy, external-link policy, maximum discovery depth, page limit, and total duration.
- Avoid crawler traps: repeated path segments, calendars, faceted-query explosions, session identifiers, infinite pagination, and identical-content URL variants.

### Fetch and rendering pipeline

1. Use conditional HTTP requests and the cache when freshness policy permits.
2. Try the low-cost HTTP path first unless browser rendering is explicitly required.
3. Detect JavaScript shells using documented signals, not domain-specific hacks.
4. Escalate once to an isolated browser worker and wait for bounded readiness conditions.
5. Block ads, trackers, popups, media, and unnecessary resources when they do not affect requested output.
6. Capture final DOM and requested artifacts, then terminate the page context.
7. Return a transparent warning when content may be incomplete instead of pretending success.

The crawler supports outbound proxy configuration for legitimate deployment/network needs. It does not ship CAPTCHA solving, fingerprint impersonation, residential-proxy resale, or automatic access-control evasion.

### Extraction quality

- Preserve document title, description, language, canonical URL, publication/modified time when declared, headings, paragraphs, lists, tables, links, images, blockquotes, and fenced code.
- Remove scripts, styles, repeated navigation, cookie overlays, advertisements, and unrelated boilerplate.
- Convert relative URLs to absolute URLs and keep link text.
- Keep deterministic document order and avoid duplicate repeated sections.
- Emit both requested output and metadata describing renderer, status code, timing, content hash, truncation, and extraction warnings.
- Treat every format as an independently bounded artifact; screenshots and raw HTML cannot consume the text-output budget.

### Scheduling and reliability

- Use a durable frontier with atomic URL state transitions: `discovered`, `queued`, `running`, `succeeded`, `failed`, or `skipped`.
- Enforce global and per-host concurrency, crawl delay, `Retry-After`, exponential backoff with jitter, bounded retries, and a host circuit breaker.
- Make job creation and cancellation idempotent. A process crash must not lose committed results or duplicate completed work on resume.
- Stream or paginate results so memory and MCP message size do not grow with crawl size.
- Store a content hash and fetch metadata to support cache revalidation, duplicate-content collapse, and incremental recrawl.
- Preserve per-page errors and a terminal reason; one failed page does not fail an otherwise useful job.

### Functional parity matrix

| Capability | Target | Current status |
| --- | --- | --- |
| Configuration and profile-scoped runtime | required | implemented |
| Public URL policy and redirect checks | required | implemented primitives |
| Static HTML/plain text to bounded Markdown | required | implemented |
| Link and XML sitemap mapping | required | implemented; broader discovery remains |
| `web_search` with zero configuration through the built-in backend | required | implemented; live backend behavior not yet measured |
| `web_search` through configured SearXNG | required | implemented |
| `web_scrape` HTTP and worker extraction modes | required | implemented; render modes unavailable |
| Async crawl jobs, status, pagination, cancel | required | implemented |
| Robots checks and background crawl execution | required | implemented |
| Durable SQLite job state | required | implemented; full resume semantics remain |
| Automatic JavaScript-render fallback | required | gap: Playwright not integrated |
| Full leases, global/per-host concurrency, retries, resume | required | gap |
| Clean/raw HTML, links, images, metadata, screenshot artifacts | required | gap: worker integration is Markdown-limited |
| Incremental recrawl and content change detection | required | gap |
| Release binaries, platform installers, and checksum-verified self-update | required | implemented |
| Remaining management CLI (`install`, `purge`, cache) | required | gap |
| PDF parsing | planned | later |
| Declarative click/type/wait/scroll actions | planned with strict limits | later |
| Schema-guided JSON extraction | planned via optional caller-supplied model | later |
| Signed webhooks and remote API | planned with remote service mode | later |
| CAPTCHA/paywall bypass | excluded | never |

### Evaluation gate

A release cannot claim crawler parity from feature count alone. The checked-in evaluation corpus must include static articles, documentation sites, SPAs, lazy-loaded pages, tables, code blocks, redirects, sitemaps, duplicate URLs, crawl traps, robots rules, rate limits, malformed HTML, and adversarial internal URLs.

Baseline: the repository contains working unit/integration coverage for several implemented primitives, but no checked-in evaluation corpus and benchmark harness establishes the quality gates below. Extraction-quality, browser-render, and full scheduler metrics therefore remain unmeasured.

Minimum release gates:

- At least 95% successful extraction on the maintained public-page corpus.
- At least 90% main-content recall and at most 10% boilerplate ratio on hand-labeled pages.
- 100% block rate for the SSRF and redirect-rebinding corpus.
- Zero duplicate committed canonical URLs per job.
- Successful resume after forced termination without losing committed pages.
- Every crawl terminates at its declared page, depth, byte, or time budget.

## 10. Phase 2 research behavior

`deep_search` remains deterministic and bounded:

1. Accept one question plus explicit `max_queries`, `max_sources`, and `max_pages` ceilings.
2. Produce simple query variants from caller-supplied variants or documented syntax rules; no hidden LLM call.
3. Search with bounded concurrency.
4. Normalize and deduplicate URLs.
5. Fetch only the highest provider-ranked candidates within budget.
6. Rank with explainable signals: provider rank, exact term coverage, freshness when known, preferred-domain match, and duplicate penalty.
7. Return a source bundle with per-item provenance and warnings; do not synthesize an answer.

`confidence` is deliberately omitted until an evaluation dataset can show that it is calibrated. Source-type weights are configuration ideas, not facts, and are also deferred until measured against retrieval quality.

## 11. Observability and privacy

Default logs contain operation, duration, status, byte count, and result count. Full queries, page content, and URL query strings are not logged by default because they may contain sensitive data.

No telemetry is sent externally in v1. A request ID may be generated locally and returned in errors for correlation.

## 12. Acceptance criteria

Phase 1 is complete only when automated checks demonstrate:

- One core call produces equivalent CLI and MCP output.
- Parallel calls within one MCP connection and concurrent MCP processes complete without duplicate committed work or cross-profile result visibility.
- SearXNG JSON success, disabled-JSON `403`, malformed response, timeout, and rate-limit paths map to stable errors.
- Duplicate and fragment-only URL variants collapse to one result.
- Public HTML and plain text are extracted into stable Markdown and bounded.
- Sitemap/HTML discovery, URL normalization, scope filters, and trap guards work on a local test site.
- A static page uses the HTTP path, while a JavaScript shell escalates to the browser and produces equivalent main content.
- Localhost, private IPs, IPv6 local addresses, metadata endpoints, unsafe schemes, redirect-to-private, and DNS-rebinding cases are blocked.
- Oversized and unsupported responses fail without unbounded buffering.
- Browser crashes and timeouts release resources and return stable errors.
- MCP stdout remains valid protocol output when diagnostics are emitted.
- No network request or secret appears in tests unless explicitly provided by a local test server.

## 13. Resolved gaps from the original concept

| Gap | Decision |
| --- | --- |
| Scope mixed search, crawling, research, providers, APIs, and SDKs | Make scraping/crawling core; defer provider breadth, remote APIs, and SDKs |
| SearXNG public instances may disable JSON | Confirmed by measurement: none of ten sampled public instances answered `format=json` on 2026-08-11, so SearXNG cannot be the default. Ship a keyless built-in backend as the default and treat a self-hosted instance as an optional privacy upgrade that returns `provider_rejected_format` on `403` |
| Requiring a search instance blocked non-technical users | No environment variable is required; the built-in backend answers search immediately after install |
| Arbitrary URL fetching enables SSRF | Validate scheme, DNS result, connected IP, and every redirect |
| “Deep search” implied hidden reasoning | Core stays model-free; deterministic orchestration returns sources, not an answer |
| Relevance and confidence numbers lacked calibration | Preserve provider rank; defer probabilistic scores until evaluated |
| Crawl quality was previously a minimal Phase 2 feature | Define Firecrawl-class discovery, dynamic rendering, extraction, durable jobs, and measurable quality gates |
| Single binary/no database conflicted with advanced crawling | Keep simple local mode, allow an isolated browser worker, and use one embedded durable store |
| Embedded state sounded like a database users must operate | SQLite remains an invisible local file: no server, port, credentials, or manual migration |
| Crawl could grow without bound | Require domain, page, depth, byte, time, concurrency, trap, and per-host budgets |
| Citations were underspecified | Preserve canonical/final URL and provenance on every result and fetched document |
| Remote MCP adds authentication and DNS-rebinding concerns | stdio only in v1; Streamable HTTP is a later separately secured scope |
| Cache could leak sensitive queries/content | No persistent cache in phase 1; phase 2 requires TTL, size bound, and privacy rules |
| Web content can contain prompt injection | Label it untrusted and never interpret or execute page instructions inside the tool |

## 14. Open decisions after Phase 1

- Cache eviction policy, selected from observed workload.
- Query-variant syntax and ranking coefficients, based on a small checked-in retrieval evaluation set.
- PDF parser and optional model boundary for structured JSON extraction.

These do not block Phase 1. Rust, TypeScript, extraction, MCP, Playwright, and SQLite dependencies must pass a short maintenance, license, security, and corpus-quality check before being added.

## 15. References

- [Original product discussion](https://chatgpt.com/share/6a78e780-01e0-83ec-9874-aa5365c38eb6)
- [SearXNG Search API](https://docs.searxng.org/dev/search_api.html)
- [Model Context Protocol transports](https://modelcontextprotocol.io/specification/draft/basic/transports)
- [RFC 9309: Robots Exclusion Protocol](https://www.rfc-editor.org/rfc/rfc9309.html)
- [OWASP SSRF Prevention Cheat Sheet](https://cheatsheetseries.owasp.org/cheatsheets/Server_Side_Request_Forgery_Prevention_Cheat_Sheet.html)
- [Firecrawl crawl API](https://docs.firecrawl.dev/api-reference/endpoint/crawl-post)
- [Firecrawl scrape formats](https://docs.firecrawl.dev/migrating-from-v0)
- [Firecrawl enhanced proxy modes](https://docs.firecrawl.dev/features/stealth-mode)
