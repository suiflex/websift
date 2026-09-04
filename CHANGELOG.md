# Changelog

Notable changes per release. Versions follow semantic versioning; the release tag is the version
prefixed with `v`, which is also the tag `websift update` compares against.

## [0.4.1](https://github.com/suiflex/websift/compare/v0.4.0...v0.4.1) (2026-09-04)


### Bug Fixes

* satisfy clippy 1.98 after the stable toolchain moved ([639c997](https://github.com/suiflex/websift/commit/639c997d5cd38e73b148b6bf19d1c9697e274980))
* satisfy clippy 1.98 after the stable toolchain moved ([e9dcf2a](https://github.com/suiflex/websift/commit/e9dcf2adb57ab86754f0eb0a0e3e11b670953e11))

## [0.4.0](https://github.com/suiflex/websift/compare/v0.3.0...v0.4.0) (2026-08-16)


### Features

* brand the setup wizard and let it install several clients at once ([f91f0cf](https://github.com/suiflex/websift/commit/f91f0cf84de880c6e2f5abcd3c3cde1629b7937e))
* register websift with MCP clients via `websift setup` ([3af2237](https://github.com/suiflex/websift/commit/3af2237df7f04b71fd795bf8d2dbe8a4f9333fc7))
* register websift with MCP clients via websift setup ([40b9b01](https://github.com/suiflex/websift/commit/40b9b012b421d0e05eed1ae8b0c08ff1d880d4c0))

## [0.3.0](https://github.com/suiflex/websift/compare/v0.2.2...v0.3.0) (2026-08-14)


### Features

* automate releases and publish to every distribution channel ([8bc0519](https://github.com/suiflex/websift/commit/8bc05199be3ff8f2de2414815d47a3bffabab78e))


### Bug Fixes

* **adapters:** record a crawl that dies instead of leaving it queued ([73fb7e0](https://github.com/suiflex/websift/commit/73fb7e0e1928d6d957dc05e2f49c7c31b476ba50))
* **adapters:** unblock web_crawl_start ([4c53ec9](https://github.com/suiflex/websift/commit/4c53ec9bd75e5e7e19227956f9fd2c0a2db0d984))
* **crawl:** recheck robots after a redirect ([f7d0f1a](https://github.com/suiflex/websift/commit/f7d0f1a5acab829c45efb7a4c58159ad4f624c91))
* **fetch:** follow redirects under the existing redirect guard ([2710c9d](https://github.com/suiflex/websift/commit/2710c9df3b0e1f460a2006ac488a4c00167ed802))
* point the install commands at a branch that exists ([1be2ba8](https://github.com/suiflex/websift/commit/1be2ba899951b6081b97347f234894a1b5e63f8b))
* **robots:** treat a missing robots.txt as permission ([00abeca](https://github.com/suiflex/websift/commit/00abeca5302b011142a87a15b141f636ee94d3db))
* **storage:** wait out shared-cache table locks ([975ead2](https://github.com/suiflex/websift/commit/975ead2c20b7ec61aeb7d4ac45deb28f2136f800))
* unblock crawling and give the repository its missing governance files ([3e3f1bf](https://github.com/suiflex/websift/commit/3e3f1bf613ea2bd946a227abd597f4fbf46a50b1))

## 0.2.2 — 2026-08-12

### Fixed

- A configured SearXNG instance no longer falls back to the built-in public backend by default.
  Configuring a private instance is a decision not to send queries to a public engine, and 0.2.0
  and 0.2.1 quietly reversed that on any transient failure. Set `WEBSIFT_SEARCH_FALLBACK=1` to opt
  back in.
- A `WEBSIFT_CACHE_TTL_MS` below one second no longer writes cache rows that can never be read;
  the cache is disabled instead.
- Two processes opening the same database at the same time no longer race during migration. Both
  read the applied version before taking a write lock, so both ran the same `CREATE TABLE` and the
  loser failed to start with `storage initialization failed`. The SQLite busy timeout is also set
  before the journal is switched to WAL, which needs a brief exclusive lock; setting it afterwards
  meant a simultaneous start failed with `database is locked` rather than waiting. SQLite refuses
  some lock upgrades outright instead of routing them to the busy handler, so switching the
  journal and applying migrations both retry while the database reports busy.

### Changed

- The README follows the layout used across the organization: status, install, tools, and
  configuration, each answerable at a glance. It no longer links into `docs/`, which is kept out
  of the published repository.

## 0.2.1 — 2026-08-12

### Fixed

- The browser worker no longer opens a console window on Windows. It is a console program, and an
  MCP host is usually a GUI process with no console to inherit, so Windows opened one for it and
  took focus. Every worker stream is piped, so that window never carried output.
- `websift update` refuses a destination that is not a regular file. Windows renames the running
  image aside before installing, which previously moved a directory standing at the destination
  out of the way and installed the binary in its place.

### Changed

- CI compiles, tests, and lints on Windows and macOS as well as Linux. Platform-specific code was
  previously first exercised on a release tag, which is how both fixes above reached `0.2.0`.
- Tests no longer derive temporary directory names from the process id alone; two tests shared one
  path and deleted each other's files under load.

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
