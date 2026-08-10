# Websift documentation

Websift has a runnable retrieval and crawl foundation. The MCP stdio server exposes configuration/status, `web_search`, `web_scrape`, `web_map`, and crawl lifecycle tools, and requires no environment configuration to start. Native HTTP fetching/extraction, URL policy, robots checks, background crawl execution, worker extraction mode, and durable SQLite state are implemented. Browser rendering, full scheduler recovery/concurrency behavior, packaging/CLI, and complete worker integration remain gaps.

## Documents

- [Product and technical specification](SPEC.md) — product contract and phased acceptance criteria.
- [Architecture](ARCHITECTURE.md) — component boundaries and security invariants.
- [Installation and distribution](INSTALLATION.md) — target installation experience and packaging contract.

## Implementation status

| Area | Status |
| --- | --- |
| MCP stdio server and status tool | Implemented |
| Profile validation and runtime configuration | Implemented |
| Public URL policy primitives | Implemented |
| Configuration, profiles, and status | Implemented |
| Public URL policy and redirect primitives | Implemented |
| Native HTTP retrieval and bounded response handling | Implemented |
| HTML/plain-text extraction | Implemented |
| Zero-configuration `web_search` through the built-in backend | Implemented (live backend behavior unmeasured) |
| Optional SearXNG `web_search` backend | Implemented |
| `web_scrape` HTTP and worker extraction modes | Implemented (rendering unavailable) |
| `web_map` static links and XML sitemap input | Implemented (discovery limits remain) |
| Crawl lifecycle, robots checks, background execution | Implemented |
| Durable SQLite crawl/job storage | Implemented (full lease/recovery semantics remain) |
| Browser render / Playwright | Gap |
| Full scheduler leases, concurrency, retries, and resume | Gap |
| Packaging, installers, and management CLI | Gap |
| End-to-end worker integration and non-Markdown artifacts | Gap |

The status table is intentionally conservative: a design document is not evidence that a feature has shipped. Run the repository checks from the root `README.md` before relying on an implementation claim.
