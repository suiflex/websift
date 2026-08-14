<p align="center">
  <img src="./assets/websift-logo.png" alt="Websift logo" width="240">
</p>

<h1 align="center">Websift — web retrieval for AI agents</h1>

<p align="center">
  <strong>Search, research, scrape, map, and crawl the public web.<br>One Rust binary, no API key, no model calls in the core.</strong>
</p>

<p align="center">
  <a href="https://github.com/suiflex/websift/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/suiflex/websift/ci.yml?branch=develop&style=for-the-badge" alt="CI status"></a>
  <a href="https://github.com/suiflex/websift/releases"><img src="https://img.shields.io/github/v/tag/suiflex/websift?include_prereleases&style=for-the-badge&label=release" alt="Release"></a>
  <a href="https://modelcontextprotocol.io"><img src="https://img.shields.io/badge/MCP-stdio-4ade80?style=for-the-badge" alt="MCP stdio"></a>
</p>

Websift gives an agent harness a consistent way to reach the web. It runs as an MCP stdio server,
so your harness starts it when a tool is called and stops it afterwards — nothing to keep running,
no port to open, no account to create.

Search works out of the box through a built-in keyless backend. Point it at your own
[SearXNG](https://docs.searxng.org) instance when you want queries to stay private.

The core never calls a model. `web_deep_search` plans queries, ranks sources with explainable
signals, and returns those sources; your agent writes the answer.

[Install](#install) · [Tools](#tools) · [Configuration](#configuration) · [Changelog](./CHANGELOG.md)

---

## Project status

**Pre-v1.0, under active development.** Working today:

- `web_search` and `web_deep_search`, with retries, backend fallback, and a wall-clock budget.
- `web_scrape` and `web_map` over native HTTP, with bounded Markdown extraction.
- Asynchronous crawl jobs with status, pagination, cancellation, and robots checks.
- Durable SQLite state, a page cache, and structured stderr events.
- Checksum-verified installers and self-update for macOS, Linux, and Windows on x86-64 and arm64.

Known gaps:

- JavaScript rendering is not integrated; extraction is static only.
- Retrieval against the live public web is not yet verified in continuous integration.
- The full crawl scheduler — leases, resume, incremental recrawl — is incomplete.

## Install

**Package managers**

```sh
brew install suiflex/tap/websift          # macOS, Linux
npm install -g @suiflex/websift           # any platform with Node 18+
cargo install websift                     # builds from source
```

```powershell
scoop bucket add suiflex https://github.com/suiflex/scoop-bucket
scoop install websift
```

The npm package is a small installer: it downloads the prebuilt binary for your platform, so
nothing is compiled and `npx -y @suiflex/websift mcp` works without a global install.

**Install scripts — macOS and Linux**

```sh
curl -fsSL https://raw.githubusercontent.com/suiflex/websift/main/install.sh | sh
```

Installs to `~/.local/bin`. Add that directory to your `PATH` if the installer says so.

**Windows (PowerShell)**

```powershell
irm https://raw.githubusercontent.com/suiflex/websift/main/install.ps1 | iex
```

Installs to `%LOCALAPPDATA%\Programs\websift` and adds it to your user `PATH`. Reopen your
terminal afterwards.

Both installers verify the release checksum before extracting anything, and neither needs `sudo`
nor administrator rights.

### Register it with your harness

One command, once. Use the absolute path the installer prints: a desktop harness often does not
inherit your shell `PATH`.

```sh
claude mcp add --scope user websift -- ~/.local/bin/websift mcp --profile claude-code
codex mcp add websift -- ~/.local/bin/websift mcp --profile codex
```

Then confirm the installation:

```sh
websift doctor
```

Nothing runs in the background afterwards. Your harness spawns `websift mcp` when a tool is called.

### Update

```sh
websift update --check   # report whether a newer release exists; changes nothing
websift update           # download, verify the checksum, replace the binary
```

## Tools

| Tool | What it does |
| --- | --- |
| `web_search` | Search the public web and return ranked, deduplicated results |
| `web_deep_search` | Research one question: several bounded queries, ranked sources with explainable signals, top pages fetched. Returns sources, never an answer |
| `web_scrape` | Fetch one page and extract bounded Markdown, links, and metadata |
| `web_map` | Discover URLs from a sitemap and start-page links without scraping every page |
| `web_crawl_start` | Start a bounded crawl job |
| `web_crawl_status` | Inspect a job's counters and state |
| `web_crawl_results` | Read a page of results |
| `web_crawl_cancel` | Stop scheduling new pages |
| `websift_status` | Report the running version and profile |

Pass `"format": "compact"` to `web_deep_search` for numbered, cited text blocks instead of full
source records when your context window is tight.

Every fetch honors `robots.txt`, refuses private and link-local destinations, bounds redirects,
and caps response size. Page content is untrusted data; whatever reads it should treat it that way.

## Configuration

No variable is required. Every one below has a working default.

| Variable | Default | Meaning |
| --- | --- | --- |
| `WEBSIFT_SEARXNG_URL` | none | Your SearXNG instance. When set, it replaces the built-in backend |
| `WEBSIFT_SEARCH_FALLBACK` | `0` | Allow a failing SearXNG instance to fall back to the public backend. Off, so a private instance stays private |
| `WEBSIFT_TIMEOUT` | `10s` | Timeout per outbound request |
| `WEBSIFT_MAX_RESULTS` | `10` | Search results per query; ceiling `50` |
| `WEBSIFT_MAX_BYTES` | `2000000` | Maximum response bytes |
| `WEBSIFT_CRAWL_CONCURRENCY` | `4` | Global request limit; ceiling `32` |
| `WEBSIFT_PER_HOST_CONCURRENCY` | `2` | Request limit per host |
| `WEBSIFT_CACHE_TTL_MS` | `900000` | Page-cache lifetime; below one second disables the cache |
| `WEBSIFT_DEEP_SEARCH_BUDGET_MS` | `60000` | Wall-clock ceiling for one `web_deep_search` |
| `WEBSIFT_LOG` | `json` | `off` silences the structured stderr events |
| `WEBSIFT_PROFILE` | `default` | Local namespace, so two harnesses keep separate state |
| `WEBSIFT_DATA_DIR` | platform data directory | Where state and artifacts live |

## From source

```sh
cargo run -- mcp --profile codex
```

Checks that must pass before a change lands:

```sh
cargo fmt --check
cargo check --all-targets
cargo test
cargo clippy --all-targets -- -D warnings
npm test --prefix browser-worker
jq empty schemas/worker-v1.schema.json
```

Continuous integration runs these on Linux, macOS, and Windows.

## More

Every tool validates its arguments and returns stable error codes; call `websift_status` or
`websift doctor` to see what a given installation actually loaded. Release-by-release changes are
in the [changelog](./CHANGELOG.md).

## License

MIT. See [LICENSE](./LICENSE).
