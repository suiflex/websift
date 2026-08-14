# Websift

MCP server for bounded web retrieval. Rust, edition 2024, MSRV 1.88, stdio transport via
`rmcp` 3.1. `publish = false` — distribution is through GitHub releases and `install.sh` /
`install.ps1`, not crates.io.

For code changes, use `forgeguard-engineering`.

`docs/` is deliberately gitignored, so this file is the only architectural context in the tree.
Keep it accurate when behavior changes.

## Modules

| Path | Responsibility |
| --- | --- |
| `src/main.rs` | CLI: `mcp`, `status`, `setup --lite`, `doctor`, `update` |
| `src/adapters/` | MCP boundary. Parameter validation, stable error codes, tool registration |
| `src/application/` | Transport-independent operations. `RuntimeStatus` validates the profile |
| `src/research/` | `deep_search`: plan queries, search concurrently, dedupe, rank, fetch top candidates |
| `src/crawl/` | Bounded BFS crawl and sitemap/link discovery (`map_documents`) |
| `src/fetch/` | Bounded HTTP client; `search.rs` (SearXNG + built-in backend), `extract.rs` (static HTML) |
| `src/policy/` | Public-destination policy, redirect guard, `robots.txt` parser, URL normalization |
| `src/robots.rs` | The shared robots gate: cache, unavailable-origin memory, per-host delay schedule |
| `src/storage/` | Embedded SQLite, ordered migrations, profile-scoped repositories |
| `src/worker/` | JSONL supervisor for the browser worker |
| `src/update.rs` | Self-update against published releases, checksum-verified |
| `browser-worker/` | Node worker implementing protocol v1 (see status below) |

Nine MCP tools: `websift_status`, `web_search`, `web_deep_search`, `web_map`, `web_scrape`,
`web_crawl_start`, `web_crawl_status`, `web_crawl_cancel`, `web_crawl_results`.

## Profiles

A profile is an isolation namespace. It is selected at startup — by `--profile` or
`WEBSIFT_PROFILE` — and belongs to the process, **never to a tool call**. No tool accepts a
profile argument, and none should be added.

Each profile gets its own database at `<data_dir>/<profile>.sqlite3`, and every repository in
`src/storage/` is keyed by profile. Two agents sharing a machine cannot see each other's crawl
jobs, documents, or page cache.

`RuntimeStatus::new` bounds a profile to 1–64 characters of ASCII alphanumerics, `-`, and `_`,
because the value becomes a filename and a storage key.

## Invariants

These are load-bearing. Do not relax one without saying so explicitly in the change.

- **Robots is deny-by-default.** An origin whose `robots.txt` cannot be read is denied, not
  assumed permissive (`RobotsDecision::Unavailable`). Every operation that fetches a page it did
  not author goes through `RobotsGate`. The single exception is `RobotsFetchError::Absent` — a
  404 or 410 means the site published nothing to obey, so the origin is allowed. Deliberately
  narrower than RFC 9309 §2.3.1.3, which permits the whole 4xx range: 401, 403, and 429 stay
  denied because they say access is restricted, not that it is free.
- **Private-address rejection happens after DNS resolution**, not by inspecting the hostname.
  `ValidatingDnsResolver` in `src/policy/` is installed on every `reqwest` client, which is what
  closes DNS rebinding. A hostname-only check would not.
- **Every redirect hop is revalidated**, hop count is bounded, and HTTPS→HTTP downgrade is
  refused (`RedirectGuard`, driven by the loop in `FetchClient::get`). Because the origin picks
  where a redirect lands, crawl and research recheck the robots gate against the **final** URL
  whenever it differs from the one that was cleared.
- **Everything is bounded**: response bytes, extracted characters, crawl depth and pages, redirect
  hops, wall-clock budgets, global and per-host concurrency, `robots.txt` document size, and
  robots cache entries. A new operation without a bound is incomplete.
- **All remote content is untrusted data.** Search responses, HTML, sitemaps, release metadata,
  and worker frames are parsed defensively and sanitized before leaving their module. No markup
  is rendered and no script is executed.
- **`unsafe_code = "forbid"`**, and clippy runs with `all` + `pedantic`.
- **The MCP process owns stdout.** Worker output must never reach it; worker stderr is discarded.

## Browser worker status

`browser-worker/` is a stub, and code should not assume otherwise:

- No Playwright dependency. `render` always fails with `render_unavailable`, and `web_scrape`
  rejects `render: "always"` outright.
- Its markdown extraction is a regex tag-strip, well below the `scraper`-based extractor in
  `src/fetch/extract.rs`.
- `BrowserMode::Auto` and `BrowserMode::Enabled` are therefore not meaningfully different from
  `BrowserMode::Disabled`. The mode is plumbed through and reported by `websift status`, but
  nothing downstream renders.

The Rust worker protocol (`src/worker/`) and the JSON schema (`schemas/worker-v1.schema.json`)
are real and validated; only the rendering implementation is missing.

## Configuration

All configuration is environment variables, validated once at startup in `src/config.rs`.
Invalid values fail the process rather than falling back silently.

| Variable | Default | Notes |
| --- | --- | --- |
| `WEBSIFT_PROFILE` | `default` | Overridden by `--profile` |
| `WEBSIFT_DATA_DIR` | platform data dir | Holds `<profile>.sqlite3` |
| `WEBSIFT_SEARXNG_URL` | unset | When set, replaces the built-in search backend. Must be a public URL |
| `WEBSIFT_MAX_RESULTS` | `10` | Upper bound 50 |
| `WEBSIFT_MAX_BYTES` | `2000000` | Response size bound |
| `WEBSIFT_TIMEOUT` / `WEBSIFT_TIMEOUT_MS` | `10000` ms | `WEBSIFT_TIMEOUT` accepts a duration (`30s`) and wins when both are set |
| `WEBSIFT_CRAWL_CONCURRENCY` | `4` | |
| `WEBSIFT_PER_HOST_CONCURRENCY` | `2` | |
| `WEBSIFT_CACHE_TTL_MS` | `900000` | Page cache TTL |
| `WEBSIFT_DEEP_SEARCH_BUDGET_MS` | `60000` | Wall-clock budget for one `web_deep_search` |
| `WEBSIFT_SEARCH_FALLBACK` | `false` | A configured SearXNG instance never falls back to a public backend unless this is set |
| `WEBSIFT_BROWSER` | `auto` | `auto` / `enabled` / `disabled` |
| `WEBSIFT_SPOOL_ROOT` | temp dir | Worker spool directory |
| `WEBSIFT_WORKER_PROGRAM` | `node` | |
| `WEBSIFT_WORKER_ARGS` | unset | JSON array |

## Tests

Tests are inline `#[cfg(test)] mod tests` per module, named as sentences describing the guarantee
(`unreadable_rules_deny_instead_of_assuming_permission`). Fixtures live in `src/fetch/fixtures/`.
`src/testing.rs` is test-only support.

**Anything reachable through an MCP tool needs a test that enters through that tool.** Every crawl
test drove `CrawlService` directly, so the adapter path was never executed and a self-deadlock in
`web_crawl_start` shipped and passed CI. Testing the service is not testing the tool.

Note that `src/policy/` rejects private addresses, so a localhost test server is not reachable
from any client built by `FetchClient::from_config`. Tests either inject a resolver through
`FetchClient::with_resolver` or use a host that never resolves.

## Releasing

Versions are owned by release-please. Never bump `Cargo.toml`, `npm/package.json`, or
`CHANGELOG.md` by hand, and never push a `v*` tag by hand — the release PR does both together, and
a manual edit only creates a conflict for the next run.

1. A maintainer dispatches `.github/workflows/release-please.yml` when a batch is ready.
2. release-please opens a release PR bumping `Cargo.toml`, `Cargo.lock`, `npm/package.json`, and
   `CHANGELOG.md` from the conventional-commit history. Merging it tags `vX.Y.Z`.
3. The tag runs `.github/workflows/release.yml`: six targets built, assets and checksums attached
   to the GitHub Release, then the formula and manifest rendered from `packaging/` into
   `suiflex/homebrew-tap` and `suiflex/scoop-bucket`, and the crate and npm package published.

Because the changelog is generated, the commit subject *is* the release note. Conventional-commit
types are enforced by that, not just by convention.

Release assets are named `websift-<tag>-<triple>[.tar.gz|.zip]`, with the bare binary alongside
them. `update::asset_name` (`src/update.rs:109`), `npm/install.js`, and both `packaging/`
templates all resolve that exact string, so renaming an asset breaks self-update and every package
manager at once. `npm/install.js --selftest` asserts the mapping and runs in CI.

crates.io and npm publish through GitHub OIDC Trusted Publishing from the `Release` environment,
so no registry token is stored anywhere. Two repository secrets are still required:
`RELEASE_PLEASE_TOKEN` (release-please needs a PAT — a PR opened with `GITHUB_TOKEN` triggers no
CI) and `TAP_PUBLISH_TOKEN` (push access to the two tap repositories).

## Verification

`make check` runs the whole chain below; `.github/workflows/ci.yml` remains the authority.

```sh
cargo fmt --check
cargo check --all-targets
cargo test
cargo clippy --all-targets -- -D warnings
npm test --prefix browser-worker
jq empty schemas/worker-v1.schema.json
```

CI runs the Rust chain on Linux, macOS, and Windows — all three are shipped targets, so
platform-specific code must compile and pass there on every change, not first on a release tag.
`install.sh` is checked with `shellcheck`; `install.ps1` with `PSScriptAnalyzer`.

`.mcp.json` points at `./target/debug/websift`, so `cargo build` is enough to exercise the server
from a local MCP client.
