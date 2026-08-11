# Backlog

Work identified while making `web_deep_search` production grade, not yet done. Ordered by value
per unit of effort. Each item states the gap, why it matters, and what "done" looks like, so it
can be picked up without rereading the diff that created it.

Last updated: 2026-08-11.

Items already closed are listed at the bottom so the same ground is not re-litigated.

## 1. Request identifier and log correlation

**Gap.** `docs/SPEC.md` §11 allows a locally generated request ID returned in errors for
correlation. Nothing generates one. Structured events on stderr carry counters but no identity, so
under a harness that issues several tool calls at once, a log line cannot be tied to the call that
produced it, and a user reporting a failure has nothing to quote.

**Why it matters.** This is the first thing missing during an incident. Every other observability
improvement is worth less without it.

**Done when.** Each tool call generates a short random ID, every `observe::event` for that call
carries it, and error strings end with a correlation suffix. No caller input is echoed into it.

## 2. Cap total upstream requests per operation

**Gap.** Retries and backend fallback multiply. Worst case for one `web_deep_search` call is
5 queries × 2 backends × 3 attempts = 30 search requests, plus up to 10 page fetches with their own
retries. The wall-clock budget bounds duration but not request count, so a slow-failing backend can
absorb the whole budget in retries.

**Why it matters.** Politeness toward upstream services, and predictable cost when a paid backend
is added later.

**Done when.** One operation carries a request counter shared by the search and fetch stages, the
ceiling is derived from the existing bounds rather than a new environment variable, and exceeding
it produces a warning rather than an error.

## 3. Caller-controlled compact budget

**Gap.** `format: "compact"` bounds its output at `max_chars * 4`. That multiplier is invented.
A caller that knows its own context window cannot express it.

**Done when.** An optional `compact_max_chars` bounds the rendered text directly, validated like
every other bound, with the current derivation as its default.

## 4. Request coalescing

**Gap.** Two identical `web_deep_search` calls running concurrently do all the work twice. The page
cache only helps once the first call has finished writing.

**Why it matters.** Agent harnesses retry and fan out; duplicate work is common in practice.

**Done when.** In-flight operations are keyed so the second caller awaits the first result instead
of issuing its own requests, with a bounded map that cannot leak entries on failure.

## 5. Near-duplicate detection

**Gap.** Duplicate suppression compares body hashes exactly. Mirrors that differ by a timestamp,
an ad slot, or a trailing newline are treated as distinct sources and consume separate slots.

**Done when.** A shingling or simhash comparison over extracted Markdown marks near-duplicates,
with a threshold justified by measurement rather than taste. Deliberately deferred until mirrors
are shown to matter for real questions.

## 6. Cache maintenance beyond write-time purging

**Gap.** Expired `page_cache` rows are deleted only when something is written for that profile. A
profile that stops fetching keeps its rows until the next write. Stale entries are never served —
expiry is enforced on read — so this is disk usage, not correctness.

**Done when.** `websift doctor` reports cache size and a management command can purge it, which
folds into the "remaining management CLI" gap the specification already tracks.

## 7. Refresh `docs/ARCHITECTURE.md`

**Gap.** The module tree there predates `research/`, `robots.rs`, `observe.rs`, and `testing.rs`,
and the document does not describe the research pipeline, the page cache, or the shared robots
gate. A reader onboarding from that document builds a wrong mental model.

**Done when.** The tree and the component descriptions match the crate, including where the
`PageStore` seam sits and why research never touches SQLite directly.

## 8. Report new configuration in `status` and `doctor`

**Gap.** `websift status` and `websift doctor` do not mention `WEBSIFT_CACHE_TTL_MS`,
`WEBSIFT_DEEP_SEARCH_BUDGET_MS`, or `WEBSIFT_LOG`. An operator cannot confirm what the process
actually loaded.

**Done when.** Both commands include the effective values, and `doctor` notes when the cache is
disabled.

## 9. Live retrieval verification

**Gap.** Everything is verified against loopback servers and unit tests. No run against the real
public web has succeeded in the development environment used so far, because outbound TLS is
blocked there (`curl https://lite.duckduckgo.com` fails to connect, and plain `web_search` fails
identically to `web_deep_search`).

**Why it matters.** Loopback tests prove the pipeline; they cannot prove that the built-in backend
still parses, that live robots files behave, or that real extraction quality holds.

**Done when.** A networked run of `web_search` and `web_deep_search` is recorded, ideally as a
manually invoked test that is skipped by default so continuous integration stays hermetic.

## Closed

- **Deterministic `deep_search` bundle.** Query planning, dedup, explainable ranking, bounded
  fetch. See `docs/SPEC.md` §10.
- **Production hardening.** Retries with backoff, backend fallback, wall-clock budget, robots gate,
  global and per-host concurrency, durable page cache, exact-hash duplicate suppression, output
  sanitization, structured stderr events, and a compact output format.
- **Shared resilient search path.** `web_search` and `web_deep_search` use one implementation, and
  both report the backend that actually answered rather than the configured preference.
- **Version bump and changelog.** Released as `0.2.0`; `CHANGELOG.md` records the tool surface and
  configuration changes.
- **End-to-end tests.** Eight loopback tests cover the assembled pipeline: happy path, backend
  fallback, transient retry, cache hit, robots denial, mirror deduplication, per-host concurrency,
  and total backend failure. The loopback DNS seam is compiled only under `cfg(test)`, so no
  shipped build can reach a private address.
