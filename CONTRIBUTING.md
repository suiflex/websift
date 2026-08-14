# Contributing to Websift

Thanks for your interest in Websift — an MCP server for bounded web retrieval: search, research,
mapping, scraping, and crawling, with no API key and no cloud service.

This guide covers how to build, test, and submit changes. By participating you agree to abide by
our [Code of Conduct](CODE_OF_CONDUCT.md).

## Getting started

A stable Rust toolchain with `rustfmt` and `clippy`. The crate is edition 2024 with an MSRV of
1.88.

```sh
rustup component add rustfmt clippy
```

Node ≥ 22.18 is needed only for `browser-worker/`. Nothing else: Websift links no system
libraries and bundles its own SQLite.

## Repository layout

| Path | Responsibility |
| --- | --- |
| `src/main.rs` | CLI: `mcp`, `status`, `setup --lite`, `doctor`, `update` |
| `src/adapters/` | The MCP boundary — parameter validation, stable error codes, tool registration |
| `src/application/` | Transport-independent operations |
| `src/research/` | `deep_search`: plan queries, search, dedupe, rank, fetch |
| `src/crawl/` | Bounded BFS crawl, sitemap and link discovery |
| `src/fetch/` | Bounded HTTP client, search backends, static HTML extraction |
| `src/policy/` | Public-destination policy, redirect guard, `robots.txt` parser |
| `src/robots.rs` | The shared robots gate |
| `src/storage/` | Embedded SQLite, migrations, profile-scoped repositories |
| `src/worker/` | JSONL supervisor for the browser worker |
| `src/update.rs` | Checksum-verified self-update |
| `browser-worker/` | Node worker implementing protocol v1 — currently a stub |
| `npm/`, `packaging/` | Distribution: the npm installer, the Homebrew and Scoop templates |

[`CLAUDE.md`](CLAUDE.md) is the architectural reference and stays authoritative. `docs/` is
deliberately not in the repository, so that file is the whole story.

## Local development

```sh
cargo build
```

`.mcp.json` already points at `./target/debug/websift`, so a local MCP client drives the real
server with no further setup. For the CLI paths:

```sh
make run               # cargo run -- mcp --profile dev
cargo run -- doctor
cargo run -- status
```

A profile is an isolation namespace with its own database, so use a scratch one while developing
and your day-to-day crawl jobs and page cache stay untouched.

## Checks

```sh
make check
```

That is the gate. It runs, in order:

```sh
cargo fmt --check
cargo check --all-targets
cargo test
cargo clippy --all-targets -- -D warnings
npm test --prefix browser-worker
jq empty schemas/worker-v1.schema.json
node npm/install.js --selftest
```

CI runs the Rust chain on Linux, macOS, **and** Windows. All three are shipped targets, so
platform-specific code has to compile and pass on every change rather than first on a release tag.
`install.sh` is additionally checked with `shellcheck`, and `install.ps1` with `PSScriptAnalyzer`.

## Testing

Tests are inline `#[cfg(test)] mod tests` in the module they cover, named as sentences describing
the guarantee — `unreadable_rules_deny_instead_of_assuming_permission`, not `test_robots_3`.
Fixtures live in `src/fetch/fixtures/`.

Two rules here are worth more than the rest, because both cost real time to rediscover.

**Anything reachable through an MCP tool needs a test that enters through that tool.** Every crawl
test drove `CrawlService` directly, so the adapter path was never executed — and a self-deadlock
in `web_crawl_start` shipped and passed CI. Testing the service is not testing the tool.

**A localhost test server is unreachable, by design.** `src/policy/` rejects private addresses
*after* DNS resolution, which is what closes DNS rebinding, so nothing built by
`FetchClient::from_config` can reach 127.0.0.1. Either inject a resolver through
`FetchClient::with_resolver`, or use a host that never resolves.

Do not try to route around it with a loopback harness. `PublicUrl::parse` rejects non-default
ports and hyper-util overwrites the resolver's port with the one from the URI, so a server on an
ephemeral port cannot be reached even through the DNS override — which is exactly why
`FetchClient::with_dns_override` and the `server()` helper in `src/fetch/mod.rs` sit unused.

## Invariants

These are load-bearing. Do not relax one silently — if a change needs to, say so explicitly in the
commit and in the pull request.

- **Robots is deny-by-default.** An origin whose `robots.txt` cannot be read is denied. The one
  exception is 404 or 410: the site published nothing to obey, so the origin is allowed.
- **Private-address rejection happens after DNS resolution**, not by inspecting the hostname.
- **Every redirect hop is revalidated**, hops are bounded, HTTPS→HTTP downgrade is refused, and
  crawl and research recheck robots against the final URL.
- **Everything is bounded** — bytes, characters, depth, pages, hops, wall clock, concurrency. A
  new operation without a bound is incomplete.
- **All remote content is untrusted.** Nothing is rendered and no script is executed.
- `unsafe_code = "forbid"`, and clippy runs with `all` + `pedantic`.
- **The MCP process owns stdout.** Worker output must never reach it.

[`CLAUDE.md`](CLAUDE.md) carries the full list with the reasoning behind each.

## Two couplings that are easy to break

**Release asset names.** `update::asset_name` (`src/update.rs:109`), `npm/install.js`, and both
templates in `packaging/` all resolve the same exact string. Renaming an asset breaks self-update
and every package manager at once. `node npm/install.js --selftest` guards the mapping and runs in
CI.

**Profiles belong to the process, never to a tool call.** The profile is chosen at startup by
`--profile` or `WEBSIFT_PROFILE`. No tool accepts a profile argument, and none should gain one.

## Commit conventions

We follow [Conventional Commits](https://www.conventionalcommits.org/). `release-please` parses
them, so **the commit subject becomes the release note** — this is enforcement, not etiquette.

- Types, as configured in `release-please-config.json`: `feat`, `fix`, `perf`, and `revert` appear
  in the changelog; `chore`, `docs`, `style`, `refactor`, `test`, `build`, and `ci` are hidden but
  still valid.
- Subject ≤ 72 characters, imperative mood, no trailing period.
- Wrap the body at 72 and explain **why** the change exists — the diff already shows the what.
- One logical change per commit, each leaving the tree buildable, so `git revert` stays safe.

## Branching and pull requests

- Branch off `develop`, named for the type of its leading commit: `feat/…`, `fix/…`, `refactor/…`,
  `chore/…`, `docs/…`.
- Run `make check` before opening the pull request.
- Fill in the [pull request template](.github/PULL_REQUEST_TEMPLATE.md) — Summary, Changes, Test
  plan. Say what you actually ran, not what you intend to run.
- Keep a pull request to one logical change. `CODEOWNERS` requests review automatically.

## Releasing

Maintainers only, and fully automated. Versions are owned by `release-please`: never hand-edit
`Cargo.toml`, `npm/package.json`, or `CHANGELOG.md`, and never push a `v*` tag by hand — a manual
edit only creates a conflict for the next run. See the Releasing section of [`CLAUDE.md`](CLAUDE.md)
for the mechanics.

## Reporting bugs and requesting features

Use the issue forms under **Issues → New issue**. A bug report is most useful with the output of
`websift status`, your OS, and the exact tool call or CLI command.

Security vulnerabilities must **never** be filed as a public issue — see
[SECURITY.md](SECURITY.md) for the private reporting path and for what counts as a vulnerability
in a tool whose job is fetching untrusted content.

## License

By contributing you agree that your contributions are licensed under the [MIT License](LICENSE)
that covers this project.
