# MCP Search — Architecture

Status: **approved target architecture; partial implementation**  
Last updated: 2026-08-10  
Related: [Product and technical specification](SPEC.md) · [Installation and distribution](INSTALLATION.md)

> **Implementation snapshot.** The current Rust core provides configuration/profile handling, public URL policy primitives, bounded native HTTP and HTML/plain-text extraction, zero-configuration search with an optional SearXNG backend, static mapping, crawl lifecycle/background execution with robots checks, worker handshake plus extraction mode, and embedded SQLite persistence. Playwright/Chromium rendering is not implemented; scheduler leases, global/per-host concurrency, retries, crash resume, and complete worker artifact integration are still target behavior. Packaging, installers, and the management CLI are also not implemented. The sections below preserve the target design and must not be read as shipped capability unless the snapshot or status matrix says so.

## 1. Decision summary

MCP Search uses two languages inside one product:

- **Rust core:** MCP and CLI adapters, configuration, search client, URL/network policy, native HTTP fetching, crawl orchestration, SQLite state, cache policy, pagination, observability, and worker supervision.
- **TypeScript content worker (target):** Playwright/Chromium rendering, DOM readiness, main-content extraction, HTML-to-Markdown conversion, screenshots, and browser artifact creation. The current worker supports the extraction protocol path but does not yet provide Playwright rendering or the full artifact set.

The TypeScript worker is an implementation detail, not a public network service. Its current integration is limited to the validated extraction path; browser rendering remains target architecture. Rust starts and supervises it locally. Control messages use versioned JSON Lines over stdin/stdout; large or binary artifacts use a restricted per-request spool directory.

Why this split:

- Rust has the stronger MCP, concurrency, correctness, and long-running service foundation.
- Playwright's primary ecosystem is TypeScript/Node, which avoids maintaining an unofficial browser automation layer.
- One process boundary isolates Chromium failures without turning the project into a distributed system.
- Zig is not used in v1 because it would add protocol, async, database, and browser-integration work without improving the network/Chromium-bound workload.

## 2. Architecture principles

1. **Rust is authoritative.** The worker cannot create jobs, choose crawl scope, persist state, or change policy.
2. **One product, one public contract.** MCP and CLI expose the product; internal worker details never leak into tool schemas.
3. **Fast path first.** Native HTTP handles ordinary pages. Chromium is used only when requested or when deterministic signals show rendering is required.
4. **Untrusted by default.** URLs, SearXNG responses, HTTP bodies, DOM, extracted text, worker messages, and future model output are validated before trusted use.
5. **Bound everything.** Every job has page, depth, time, byte, redirect, retry, concurrency, artifact, and output limits.
6. **Durable, not operationally visible.** An embedded SQLite file provides single-node recovery. Users never configure or run a database service.
7. **At-least-once work, idempotent results.** A page may be attempted again after a crash, but one canonical URL is committed once per crawl job.
8. **Evidence over claims.** Extraction quality, safety, and recovery claims are gated by a reproducible local corpus.
9. **Session-safe by construction.** Request cancellation is connection-scoped; durable crawl ownership is profile-scoped; process coordination uses atomic leases rather than in-memory assumptions.

## 3. System context

```text
Claude / Codex / OpenCode / shell
                 │
         MCP stdio or CLI
                 │
                 ▼
┌────────────────────────────────────────────────────┐
│ Rust core                                          │
│                                                    │
│ adapters → application services → crawl scheduler  │
│                 │          │             │         │
│                 ▼          ▼             ▼         │
│           SearXNG client  SQLite     worker manager │
│                 │          ▲             │         │
│                 ▼          │             ▼         │
│          safe HTTP fetch ───┘      JSONL control    │
│                 │                        │          │
│                 └──────────┬─────────────┘          │
│                            ▼                        │
│                     egress policy                   │
└────────────────────────────┬───────────────────────┘
                             │ allowed public HTTP(S)
                ┌────────────┴────────────┐
                ▼                         ▼
          SearXNG instance           Public websites

             supervised locally
                     │
                     ▼
┌────────────────────────────────────────────────────┐
│ TypeScript content worker                          │
│ Playwright + Chromium + DOM extraction             │
│ No job database, MCP endpoint, or policy authority │
└────────────────────────────────────────────────────┘
```

## 4. Component boundaries

### 4.1 MCP and CLI adapters

Responsibilities:

- Parse transport-specific input.
- Validate input against the public schema.
- Call one application operation.
- Map domain results and stable errors into MCP or CLI output.
- Propagate cancellation and deadlines.

They must not contain crawling, URL, storage, retry, or extraction rules.

MCP stdio is the only v1 MCP transport. Explicit crawl job tools remain available even when MCP task support exists because not all clients implement long-running tasks consistently. Native MCP progress and cancellation are used when negotiated.

### 4.2 Application services

The Rust application layer exposes these operations:

- `search`
- `scrape`
- `map`
- `start_crawl`
- `get_crawl`
- `list_crawl_results`
- `cancel_crawl`

Each operation receives validated values plus a deadline and cancellation token. It returns domain data or a stable error code; it does not know whether the caller is MCP or CLI.

### 4.3 Search client

One client owns both backends because they share every bound that matters: validating DNS resolver, redirect policy, timeout, response-size cap, and result validation. Only the request URL and the response parser differ.

- Selects the backend from configuration, never from tool arguments: a configured SearXNG base URL when present, otherwise the built-in keyless backend.
- Requests JSON explicitly from SearXNG and rejects unexpected media types or malformed schemas.
- Parses the built-in backend's HTML as untrusted data, reading only result anchors and snippet text, and unwrapping the backend's redirect links to their real destinations.
- Treats an unparsable response as a failure rather than an empty result list, so a markup change surfaces as `provider_unavailable` instead of silently reporting zero results.
- Validates every result URL through the shared URL policy before returning it.
- Preserves provider order and provenance, and reports which backend served the request; it does not invent relevance probabilities.

The backend split is an enum inside the client, not a provider trait. A generic provider abstraction waits until a third backend exists.

### 4.4 URL policy and egress guard

This is one shared Rust policy used by search-result validation, native HTTP, redirects, sitemap discovery, crawl links, and browser egress.

It must:

- Accept only public `http` and `https` URLs.
- Reject embedded credentials, invalid hosts, unsafe ports, and malformed encodings.
- Resolve all addresses and reject loopback, private, link-local, multicast, unspecified, reserved, and metadata destinations for IPv4 and IPv6.
- Revalidate redirects and connected addresses to resist DNS rebinding.
- Apply domain, subdomain, external-link, path, query, and robots scope.
- Return a reasoned allow/deny decision suitable for audit logs without leaking secrets.

Browser pages can initiate subresource requests that never pass through the original URL validator. Therefore:

- Chromium is configured to use the Rust-controlled egress path.
- Proxy-bypass lists are disabled and browser features that create uncontrolled peer-to-peer traffic are disabled.
- Browser request interception additionally rejects non-HTTP schemes, downloads, popups, WebRTC, and unnecessary resource types.
- Redirects, frames, workers, service workers, WebSockets, and subresources remain subject to egress policy.

The security claim covers page-controlled browser traffic, not a Chromium or Node sandbox escape. Operators needing defense against a compromised browser process must also enforce an infrastructure egress firewall or forward proxy. Release tests verify that ordinary page JavaScript cannot bypass the controlled path.

### 4.5 Native HTTP fetcher

- Uses pooled connections with explicit connect, header, body, and total deadlines.
- Streams bodies through compressed and decoded byte ceilings; it never buffers an unbounded response.
- Supports conditional requests with `ETag` and `Last-Modified` when cache policy permits.
- Captures redirect chain, final URL, status, media type, declared charset, bytes, and timing.
- Writes content to a Rust-owned spool artifact when extraction is required.

The native path never executes JavaScript.

### 4.6 Crawl scheduler

The scheduler owns the frontier and fairness:

- Discovers URLs from the seed, sitemaps, and committed page links.
- Normalizes URLs before uniqueness checks.
- Applies scope and crawler-trap policy before queueing.
- Leases bounded work to fetch/render execution.
- Enforces global and per-host concurrency, crawl delay, retry budget, `Retry-After`, and host backoff.
- Stops at page, depth, duration, byte, or cancellation limits.
- Reclaims expired leases after process failure.

The scheduler is event-driven. It does not poll in tight loops and does not hold a database transaction across network or worker I/O.

### 4.7 SQLite store

SQLite is an implementation detail embedded in the Rust process, not an external dependency. The application creates and migrates one local state file automatically; there is no database daemon, port, account, connection string, or operator setup. WAL mode and short transactions allow concurrent readers with one controlled writer path.

Conceptual records:

- `runtime_instances`: random process instance ID, profile, heartbeat, start time, and expiry.
- `resource_leases`: globally bounded browser/scheduler slots owned by a live instance with expiry.
- `crawl_jobs`: profile, immutable request, resolved budgets, state, counters, timestamps, terminal reason.
- `crawl_urls`: profile, job, normalized URL, discovered-from URL, depth, state, attempts, lease owner/expiry, next-attempt time, final URL, error.
- `documents`: profile, job, canonical URL, metadata, content hash, extraction version, artifact references.
- `cache_entries`: profile, request identity, validators, freshness, content hash, size, last access.
- `artifacts`: owner, relative path, media type, size, hash, retention deadline.

Required constraints:

- Unique crawl identity for idempotent job creation when an idempotency key is supplied.
- Unique `(job_id, normalized_url)` frontier entry.
- Unique committed `(job_id, canonical_url)` document.
- Every profile-owned query includes the profile predicate; profile is never accepted from an individual tool call.
- Foreign keys enabled.
- Stable result ordering by commit sequence plus unique ID.

Schema changes use ordered migrations applied transactionally at startup. Startup refuses an unknown newer schema rather than attempting downgrade or destructive repair.

### 4.8 Session and multi-process coordination

One harness session normally owns one MCP connection to one Rust process, but multiple sessions or different harnesses may start multiple processes simultaneously. Correctness cannot depend on only one process existing.

Identity levels:

- **Profile:** stable local namespace selected only at process startup, such as `codex` or `claude-code`. It scopes jobs, results, cache, and tool visibility.
- **Runtime instance:** random ID created for each Rust process. It owns heartbeats and expiring work/resource leases.
- **MCP connection:** transport lifetime. Disconnecting it cancels request-scoped work but does not corrupt or silently delete durable crawl jobs.
- **Request:** unique ID plus cancellation token and deadline. Parallel requests in one connection are independent.

Coordination rules:

- SQLite is shared by processes running as the same OS user, with WAL, busy timeout, and short transactions.
- Frontier claims use one atomic conditional write; two processes cannot own the same live URL lease.
- Browser and scheduler capacity use shared expiring resource leases, so total concurrency remains bounded across all harness sessions.
- A process renews only its own leases. After heartbeat expiry, another process may reclaim them within retry policy.
- Job and result operations always filter by the startup profile. A Codex process cannot list or cancel Claude Code jobs by guessing an ID.
- Browser binaries are shared read-only. Installation uses an inter-process lock and atomic directory rename.
- Spool and artifact paths are job/request scoped and never reused concurrently.
- Shutdown releases owned leases when possible; crash recovery relies on expiry and idempotent commit constraints.

Profiles are a local privacy boundary, not authentication or sandboxing. Processes running as the same OS account can still access the state file directly. Remote multi-user isolation requires the deferred authenticated service architecture.

### 4.9 Worker manager

Rust owns worker processes and capacity:

- Starts a fixed-size pool from configuration.
- Requires a successful protocol handshake before accepting work.
- Sends one request to one worker at a time in v1.
- Tracks heartbeat, request deadline, process RSS, page count, and Chromium health.
- Sends cancellation, then kills the process tree after a grace period.
- Recycles workers after configurable page count, memory ceiling, crash, or protocol violation.
- Restarts with bounded exponential backoff and exposes `browser_unavailable` when capacity cannot recover.

Worker crashes never crash the Rust process or directly mutate crawl state.

### 4.10 TypeScript content worker

The worker supports two commands:

- `extract`: read a Rust-fetched HTML artifact and produce normalized content artifacts without navigation.
- `render`: navigate Chromium through the controlled egress path, wait for bounded readiness, then extract.

Responsibilities:

- Create a fresh browser context per request.
- Apply viewport, locale, resource-blocking, timeout, and readiness settings.
- Close pages and contexts in `finally` paths.
- Produce requested Markdown, cleaned HTML, raw HTML, links, images, metadata, or screenshot artifacts.
- Return an artifact manifest, warnings, timings, renderer version, and deterministic extraction version.

It does not:

- Open an MCP or public HTTP server.
- Read the SQLite database.
- Choose new crawl URLs or follow links beyond the requested page.
- Accept arbitrary JavaScript.
- Persist cookies between jobs.
- Decide whether a URL is safe.

## 5. Rust ↔ TypeScript protocol

### 5.1 Transport

- stdin/stdout with one UTF-8 JSON object per line.
- stdout is protocol-only; worker diagnostics use stderr.
- Maximum control-frame size is fixed and validated on both sides.
- HTML, screenshots, and other large data never travel inline.
- Rust creates a request spool directory with restrictive permissions and passes its opaque identifier during the request. Its root and the worker command are internal variables (`MCP_SEARCH_SPOOL_ROOT`, `MCP_SEARCH_WORKER_PROGRAM`, `MCP_SEARCH_WORKER_ARGS`) used by development and packaging, not part of the user-facing configuration contract.
- Worker manifests contain relative paths only. Rust rejects absolute paths, traversal, symlinks, unknown files, size mismatches, and hash mismatches.

### 5.2 Handshake

```json
{"type":"hello","protocol_version":1,"worker_version":"0.1.0","capabilities":["extract","render","screenshot"]}
```

Rust rejects incompatible protocol versions or missing required capabilities before dispatching work.

### 5.3 Request

```json
{
  "type":"request",
  "protocol_version":1,
  "request_id":"01J...",
  "operation":"render",
  "url":"https://example.com",
  "deadline_ms":30000,
  "spool_id":"01J...",
  "options":{
    "formats":["markdown","links","metadata"],
    "only_main_content":true,
    "wait_for_ms":0,
    "max_output_chars":30000
  }
}
```

### 5.4 Result

```json
{
  "type":"result",
  "protocol_version":1,
  "request_id":"01J...",
  "status":"ok",
  "final_url":"https://example.com/",
  "artifacts":[
    {
      "kind":"markdown",
      "path":"content.md",
      "media_type":"text/markdown",
      "bytes":1234,
      "sha256":"..."
    }
  ],
  "warnings":[],
  "timing_ms":{"navigation":250,"extraction":20}
}
```

Errors use stable internal worker codes. Rust maps them to the public error contract; raw stack traces remain in redacted diagnostics.

### 5.5 Cancellation and liveness

- Rust sends `{"type":"cancel","request_id":"..."}`.
- Worker acknowledges cancellation and closes the browser context.
- Periodic heartbeat frames are allowed only while idle or during long operations.
- A missed heartbeat or deadline triggers process-tree termination and lease recovery.

## 6. Core data flow

### 6.1 Search

```text
MCP/CLI input
  → schema validation
  → SearXNG request
  → provider response validation
  → URL normalization and public-host checks
  → deduplication preserving provider rank
  → bounded result
```

### 6.2 Scrape with automatic rendering

```text
validated URL
  → robots/site policy where applicable
  → native HTTP fetch
  → classify response
      ├─ useful HTML → worker.extract
      └─ JS shell / forced render → worker.render
  → validate artifact manifest
  → normalize provenance and warnings
  → cache eligible artifacts
  → bounded response
```

Automatic browser escalation happens at most once. Signals and thresholds are versioned and tested against the corpus; no domain-specific workaround is silently added.

### 6.3 Crawl

```text
start request
  → validate and persist job + seed atomically
  → return job ID

scheduler loop
  → lease eligible URL
  → enforce host budget
  → scrape pipeline
  → transaction:
       commit document or page error
       mark leased URL terminal
       insert newly discovered in-scope URLs
       update job counters
  → repeat until frontier empty or budget reached
  → mark terminal state
```

If the process stops after fetching but before commit, the lease expires and the page may be fetched again. Unique constraints and content hashes prevent duplicate committed output.

### 6.4 Result pagination

Results use opaque cursors based on stable commit ordering, not offset pagination. Cursor payloads are versioned and integrity-protected so callers cannot inject database predicates.

## 7. State machines

### Crawl job

```text
queued → running → completed
             ├──→ failed
             └──→ cancelled
```

- `completed` may include per-page failures.
- `failed` means the job cannot make useful progress, not that one page failed.
- `cancelled` is terminal and idempotent.

### Crawl URL

```text
discovered → queued → running → succeeded
                         ├──→ retry_wait → queued
                         ├──→ failed
                         └──→ skipped
```

Transitions are validated centrally and written atomically. Recovery changes expired `running` leases to `queued` or `failed` according to retry budget.

## 8. Failure policy

| Failure | Behavior |
| --- | --- |
| SearXNG timeout or invalid response | Stable provider error; retry only when policy marks it safe |
| HTTP timeout or reset | Bounded retry with jitter; preserve attempt error |
| `429` or `503` with `Retry-After` | Delay that host without blocking other hosts |
| Robots disallow | Mark URL skipped with `robots_disallowed` |
| Redirect to unsafe destination | Abort before connection and record `blocked_url` |
| Oversized body/artifact | Abort stream, delete partial artifact, return size error |
| Worker protocol violation | Kill/recycle worker; reject all unvalidated artifacts |
| Chromium crash | Retry once on a fresh worker when budget permits |
| Extraction failure | Preserve fetch metadata and per-page failure; do not fabricate empty content |
| SQLite busy | Short bounded retry; never hold transaction during I/O |
| Disk full | Stop accepting crawl work, fail active writes safely, preserve existing committed data |
| Process crash | Recover expired leases and resume non-terminal jobs |
| Two harness sessions claim the same URL | Atomic lease permits one owner; the other takes different work |
| Many sessions start Chromium together | Shared resource leases enforce the configured machine-wide ceiling |
| Cancellation | Stop new scheduling, cancel in-flight work, retain committed partial results |

## 9. AI and content safety boundary

The crawler is a retrieval tool, not an autonomous browser agent.

- Page content is returned with explicit provenance and treated as untrusted.
- Instructions inside pages never alter crawler configuration, tool selection, scope, or policy.
- v1 does not call an LLM and does not emit synthetic confidence.
- Future schema-guided extraction must validate model output against JSON Schema plus business limits before returning it.
- Future model configuration must define provider, model, timeout, retry, token, usage, retention, and fallback policy.
- Stable chunk IDs use document content hash plus deterministic chunk position so citations can be rechecked.
- Cached content carries fetch time, validators, extraction version, and stale state.

## 10. Security invariants

The implementation is not releasable unless these remain true:

1. Every application-controlled outbound destination is policy-checked, including DNS results, redirects, frames, workers, WebSockets, and browser subresources.
2. The browser worker cannot mutate job state or bypass public tool validation.
3. No secret, cookie, full query string, page body, or raw stack trace appears in default logs.
4. Tool inputs, worker messages, provider responses, database rows, cursors, and artifact manifests are schema-validated.
5. No network or worker operation is unbounded or holds an open database transaction.
6. Partial files are not exposed as completed artifacts.
7. Raw HTML is never trusted-rendered by the product.
8. MCP stdout contains MCP messages only; worker stdout contains worker protocol only.

## 11. Deployment topology

### Local development

```text
Rust process ──spawns──> TypeScript worker ──spawns──> Chromium
     │
     └── SQLite + spool directory
```

This mode optimizes iteration speed. Network isolation is best effort and must be labeled accordingly.

### Primary end-user installation

The primary product is a prebuilt `mcp-search` command installed through npm or a verified native installer. The package contains or retrieves the matching Rust executable and content worker; users do not compile either language. Browser setup and MCP client registration are owned by `mcp-search setup` and `mcp-search install <client>`.

SQLite state is created lazily in the platform data directory. Chromium is installed into a managed cache only for full mode. See `INSTALLATION.md` for the user-facing contract.

### Recommended self-hosted deployment

```text
Docker Compose
├── app: Rust core + supervised TypeScript worker + pinned Chromium
│        └── SQLite/artifact volume + Rust egress guard
└── searxng: optional bundled search backend
```

Keeping the worker beside the core preserves the stdio protocol and one-image installation. Chromium is forced through the Rust egress guard by launch configuration and request interception. Deployments with a stronger threat model can additionally place the app behind an infrastructure egress firewall. No Redis, PostgreSQL, Kubernetes, or service mesh is required.

### Scaling ceiling

The initial architecture is single-node. Increase worker count only within CPU, memory, file-descriptor, and SQLite write limits. Introduce a remote queue or database only after measurements show the embedded design cannot meet a documented target; doing so is a separate architecture revision.

## 12. Repository layout

```text
.
├── Cargo.toml
├── src/
│   ├── adapters/       # MCP and CLI only
│   ├── application/    # use cases
│   ├── crawl/          # scheduler and state transitions
│   ├── fetch/          # SearXNG and native HTTP
│   ├── policy/         # URL, egress, robots, budgets
│   ├── storage/        # SQLite migrations and repositories
│   └── worker/         # process supervision and protocol
├── browser-worker/
│   ├── package.json
│   └── src/            # current protocol/extraction worker; Playwright is target
├── schemas/            # public and worker JSON schemas
├── migrations/         # ordered SQLite migrations
├── tests/fixtures/     # target deterministic web/adversarial corpus (not checked in yet)
└── docs/
    ├── SPEC.md
    ├── ARCHITECTURE.md
    └── INSTALLATION.md
```

Start with one Rust crate and one TypeScript package. Split crates or services only when independent build/release or dependency boundaries become real.

## 13. Dependency policy

Before addition, a dependency must have:

- A compatible open-source license.
- Recent maintenance and a credible security response path.
- A purpose not safely covered by the standard library or an existing dependency.
- Locked versions and reproducible installation.
- Corpus or conformance evidence for critical extraction/MCP behavior.

The current implementation uses maintained dependencies for async runtime/HTTP, SQLite, DOM parsing, Markdown conversion, JSON Schema validation, and tracing. Playwright/Chromium, browser artifact generation, and release packaging dependencies are target additions, not shipped capabilities. Exact packages are chosen during implementation preflight and recorded in lockfiles; speculative wrappers are not created.

The target architecture pins Chromium and Node to a Playwright-compatible release in the container. Release artifacts should include dependency licenses, SBOM, checksums, and provenance once packaging exists.

## 14. Verification strategy

### Deterministic corpus

A local fixture server covers:

- Static HTML, SPA, delayed/lazy content, frames, service workers, redirects, sitemaps, canonical URLs, tables, code, malformed markup, encodings, and multiple languages.
- Robots policies, rate limits, retries, crawl delay, duplicate paths, query explosions, infinite calendars, and pagination traps.
- IPv4/IPv6 private targets, metadata targets, DNS rebinding, redirect-to-private, unsafe ports/schemes, WebSockets, and malicious artifact manifests.
- Worker hangs, crashes, malformed frames, oversized frames, wrong hashes, path traversal, disk-full simulation, and process restart.

### Required checks

- Rust unit tests for pure normalization, policy, budgets, and state transitions.
- TypeScript tests for extraction and artifact manifests.
- Contract tests generated from the same worker protocol schema in both languages.
- Integration tests with local SearXNG and fixture servers.
- Container security tests proving the browser cannot bypass controlled egress.
- MCP conformance tests plus stdout purity checks.
- Forced-kill recovery test proving committed pages survive and expired leases resume.
- Multi-process tests with parallel same-profile and cross-profile MCP sessions, including cancellation and browser-slot ceilings.
- Benchmark report for extraction success, content recall, boilerplate ratio, latency, memory, and browser escalation rate.

Live websites are opt-in smoke tests only; they cannot gate releases because their content and defenses change independently.

## 15. Delivery slices

1. **Foundation:** schemas, Rust binary, worker handshake, fixture server, CI, and no real crawling.
2. **Static scrape:** URL policy, native HTTP, worker extraction, Markdown provenance, and bounded MCP/CLI output.
3. **Dynamic scrape:** Playwright rendering, controlled egress, worker lifecycle, and browser safety tests.
4. **Search integration:** SearXNG results routed through the same URL policy.
5. **Map:** sitemap/link discovery, normalization, scope filters, and trap protection.
6. **Crawl jobs:** SQLite frontier, leases, pagination, cancel, resume, retries, and partial results.
7. **Release:** npm/native installers, agent registration, optional Compose, documentation, benchmark report, SBOM, signed artifacts, and compatibility policy.

Each slice must leave one runnable vertical check. Search is integrated only after safe retrieval primitives so provider results cannot bypass the URL and egress boundary.

## 16. Deferred decisions

- Remote MCP/REST authentication and multi-tenancy.
- Distributed queue or external database.
- Paid search providers and generic provider interfaces.
- Authenticated crawling and host-scoped secret storage.
- Declarative browser actions.
- PDF/document parsing.
- Optional LLM-based structured extraction.
- Dashboard and hosted control plane.

These are excluded from the initial architecture, not reserved extension points requiring empty interfaces today.

## 17. Architecture fitness criteria

The architecture is accepted when implementation evidence shows:

- Rust can kill and replace a hung worker without losing the MCP process.
- Static pages avoid Chromium; dynamic fixtures escalate once and extract correctly.
- Browser traffic cannot reach private or metadata destinations in the hardened topology.
- A forced crash resumes a crawl without duplicate committed canonical URLs.
- Concurrent harness sessions cannot exceed global worker limits or read/cancel another profile's jobs.
- Crawl cancellation is bounded and preserves partial results.
- Memory and MCP output remain bounded as crawl size grows.
- The same application operation produces equivalent CLI and MCP data.
- The evaluation gates in `SPEC.md` pass with recorded commands and results.
