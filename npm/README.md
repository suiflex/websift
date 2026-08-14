# @suiflex/websift

Bounded web search, research, mapping, scraping, and crawling, exposed as an
[MCP](https://modelcontextprotocol.io) server. No API key, no cloud service — Websift runs as a
local process and talks to your agent over stdio.

This package is a small installer. On install it downloads the prebuilt `websift` binary for your
platform from the matching [GitHub Release](https://github.com/suiflex/websift/releases) and puts
a `websift` command on your PATH. The binary is native Rust; nothing is compiled at install time.

## Install

```sh
npm install -g @suiflex/websift
```

Or run it without installing:

```sh
npx -y @suiflex/websift status
```

## Use it as an MCP server

Point your client at the `websift mcp` command. For Claude Code:

```sh
claude mcp add websift -- npx -y @suiflex/websift mcp --profile claude-code
```

Or, in a client that reads a JSON config:

```json
{
  "mcpServers": {
    "websift": {
      "command": "npx",
      "args": ["-y", "@suiflex/websift", "mcp", "--profile", "claude-code"]
    }
  }
}
```

`--profile` names an isolation namespace. Each profile gets its own database, so two agents on one
machine never see each other's crawl jobs, documents, or page cache.

## Tools

| Tool | What it does |
| --- | --- |
| `web_search` | Search the public web through the built-in backend, or your own SearXNG instance |
| `web_deep_search` | Research one question: several bounded queries, deduplicated and ranked sources, top pages fetched |
| `web_map` | Discover URLs from a sitemap and page links |
| `web_scrape` | Fetch and statically extract one page |
| `web_crawl_start` / `_status` / `_cancel` / `_results` | Bounded crawl jobs |
| `websift_status` | Report the running version and profile |

Every fetch honors `robots.txt`, refuses private and link-local destinations after DNS resolution,
revalidates each redirect hop, and bounds response size, extraction length, crawl depth, and
wall-clock time.

## Supported platforms

macOS (arm64, x64), Linux (x64, arm64), Windows (x64, arm64). Node 18 or newer.

## Other install methods

```sh
brew install suiflex/tap/websift     # macOS, Linux
scoop bucket add suiflex https://github.com/suiflex/scoop-bucket && scoop install websift
cargo install websift                # builds from source
```

## Links

- [Repository and full documentation](https://github.com/suiflex/websift)
- [Changelog](https://github.com/suiflex/websift/blob/develop/CHANGELOG.md)
- [Report a security issue](https://github.com/suiflex/websift/security/advisories/new)

MIT licensed.
