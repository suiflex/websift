# MCP Search

Open-source, local-first web search and crawling for AI agents.

The runnable server exposes configuration/status plus bounded web retrieval tools: `web_search`, `web_scrape`, `web_map`, and asynchronous crawl lifecycle tools. Nothing has to be configured: search uses a built-in keyless backend, and a self-hosted SearXNG instance is an optional privacy upgrade. Native HTTP fetching and extraction are implemented; browser rendering and the full production scheduler remain explicit gaps (see the status matrix).

```sh
cargo run -- mcp --profile codex
```

## Documentation

- [Documentation index](docs/README.md)
- [Product specification](docs/SPEC.md)
- [Architecture](docs/ARCHITECTURE.md)
- [Installation and distribution](docs/INSTALLATION.md)

## Development checks

```bash
cargo fmt --check
cargo check --all-targets
cargo test
cargo clippy --all-targets -- -D warnings
npm test --prefix browser-worker
jq empty schemas/worker-v1.schema.json
```
