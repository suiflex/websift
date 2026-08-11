# Changelog

Notable changes per release. Versions follow semantic versioning; the release tag is the version
prefixed with `v`, which is also the tag `websift update` compares against.

## 0.2.0 — 2026-08-11

### Added

- `web_deep_search` MCP tool. It plans queries from the question plus caller-supplied variants,
  searches them concurrently, deduplicates URLs, ranks with explainable signals, and fetches the
  top pages. It returns ranked sources and never synthesizes an answer, so no model is involved.
  Each source carries its ranking signals, so any ordering can be explained without rerunning the
  operation.
- `format: "compact"` on `web_deep_search`, which returns numbered cited text blocks instead of
  full source records for callers with a tight context window.
- Durable page cache for research fetches, in profile-scoped SQLite, keyed by URL and extraction
  bound (migration `0002_page_cache.sql`).
- Structured operational events as one JSON object per line on stderr, carrying operation,
  duration, status, and counters. Queries, URLs, and page content are never logged.
- New optional configuration: `WEBSIFT_CACHE_TTL_MS` (default `900000`, `0` disables the cache),
  `WEBSIFT_DEEP_SEARCH_BUDGET_MS` (default `60000`), and `WEBSIFT_LOG` (`off` silences events).
- `docs/BACKLOG.md`, recording the gaps left after this work.

### Changed

- `web_search` now uses the same resilient search path as `web_deep_search`: transient failures
  retry with exponential backoff, and a configured SearXNG instance falls back to the built-in
  backend when it is blocked or unreachable. It previously failed outright in that case.
- Page fetches performed by research pass the same robots gate as crawling, including crawl
  delay, and respect global and per-host concurrency limits.
- The robots cache, denial memory, and per-host schedule moved out of the crawl service into a
  shared gate, so crawling and research answer the same question the same way.

### Fixed

- `web_search` reported the configured backend rather than the backend that actually answered,
  which was wrong whenever a fallback occurred. `source` and `meta.provider` now name the backend
  that served the results.

### Known gaps

- Live retrieval against the public web is still unverified; all verification so far is unit
  tests plus end-to-end tests against loopback HTTP servers. See `docs/BACKLOG.md`.

## 0.1.1

- `websift update`: checksum-verified self-update, with staging cleaned up on every failed path.

## 0.1.0

- First tagged release: MCP stdio server, `web_search`, `web_scrape`, `web_map`, crawl lifecycle
  tools, URL policy, durable SQLite state, and platform installers.
