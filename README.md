# Websift

Open-source, local-first web search and crawling for AI agents.

The runnable server exposes configuration/status plus bounded web retrieval tools: `web_search`, `web_deep_search`, `web_scrape`, `web_map`, and asynchronous crawl lifecycle tools. `web_deep_search` runs a bounded multi-query research pass — search, deduplicate, rank with explainable signals, fetch the top pages — and returns ranked sources rather than a synthesized answer. It retries transient failures, falls back to the built-in backend when a configured instance is blocked, honors `robots.txt` and per-host limits, caches extractions in SQLite, and stops at a wall-clock budget. Pass `"format": "compact"` for cited text blocks instead of full source records when context is tight. Nothing has to be configured: search uses a built-in keyless backend, and a self-hosted SearXNG instance is an optional privacy upgrade. Native HTTP fetching and extraction are implemented; browser rendering and the full production scheduler remain explicit gaps (see the status matrix).

## Install

macOS and Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/suiflex/websift/main/install.sh | sh
```

Windows (PowerShell):

```powershell
irm https://raw.githubusercontent.com/suiflex/websift/main/install.ps1 | iex
```

Both installers verify the release checksum before extracting anything, install to a per-user directory without sudo or administrator rights, and print the command that registers the server with your agent. Then:

```sh
claude mcp add --scope user websift -- websift mcp --profile claude-code
codex mcp add websift -- websift mcp --profile codex
```

No environment variable is required. Run `websift doctor` to check an installation.

Staying current:

```sh
websift update --check   # report whether a newer release exists; changes nothing
websift update           # download, verify the checksum, and replace the binary
```

From a source checkout:

```sh
cargo run -- mcp --profile codex
```

## Documentation

- [Documentation index](docs/README.md)
- [Product specification](docs/SPEC.md)
- [Architecture](docs/ARCHITECTURE.md)
- [Installation and distribution](docs/INSTALLATION.md)
- [Backlog](docs/BACKLOG.md)
- [Changelog](CHANGELOG.md)

## Development checks

```bash
cargo fmt --check
cargo check --all-targets
cargo test
cargo clippy --all-targets -- -D warnings
npm test --prefix browser-worker
jq empty schemas/worker-v1.schema.json
```
