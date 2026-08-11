# Websift — Installation and Distribution

Status: **binary installers shipped; npm distribution and management CLI remain unimplemented**  
Last updated: 2026-08-11

> Released binaries are published for macOS, Linux, and Windows on x86_64 and aarch64, and the `install.sh` / `install.ps1` scripts install them with checksum verification. Configuration/profile handling, native retrieval, extraction, search, mapping, crawl lifecycle, worker extraction, and embedded SQLite state are available. The CLI provides machine-readable `status`, `doctor`, and configuration-only `setup --lite`. `websift update` and `websift update --check` are implemented. npm distribution, browser/Playwright setup, automatic client registration, full `setup`, `install`, cache, and purge commands are not shipped.

## 1. Installation promise

A user should not need to understand Rust, TypeScript, Playwright, Chromium, SQLite, MCP JSON, or agent configuration.

macOS and Linux:

```bash
curl -fsSL https://raw.githubusercontent.com/suiflex/websift/main/install.sh | sh
```

Windows (PowerShell):

```powershell
irm https://raw.githubusercontent.com/suiflex/websift/main/install.ps1 | iex
```

Both scripts detect the platform, download the matching release archive, **verify its SHA-256 before extracting**, and install into a per-user directory (`~/.local/bin`, or `%LOCALAPPDATA%\Programs\websift`). Neither needs sudo or administrator rights. Set `VERSION` / `WEBSIFT_VERSION` to pin a tag, and `INSTALL_DIR` / `WEBSIFT_INSTALL_DIR` to relocate the binary.

Registration is one command per agent, printed by the installer:

```bash
claude mcp add --scope user websift -- websift mcp --profile claude-code
codex mcp add websift -- websift mcp --profile codex
```

Nothing else has to be configured; `web_search` works immediately through the built-in backend.

For a source checkout, start the MCP server with:

```bash
cargo run -- mcp --profile codex
```

`websift install <client>` — a single command that performs setup, registers the client, and verifies the handshake — remains target behavior and is not shipped.

## 2. What gets installed

```text
websift executable
├── Rust core
├── bundled TypeScript worker
├── private Node runtime when installed natively
├── managed Chromium build in full mode
└── one automatically managed local state file
```

There is no external database. SQLite is compiled/linked into the application and stored as one file under the operating system's normal application-data directory. The user does not install SQLite, create tables, choose a port, set credentials, or run migrations.

The application owns:

- State-file creation and schema migrations.
- Chromium download and compatible-version selection.
- Worker startup and shutdown.
- Cache and artifact directories.
- MCP registration and verification.

## 3. Modes

Full packaging behavior remains a target. The source CLI implements only the safe lite setup check described below.

### Full mode — target default

The target mode includes search integration when configured, static scraping, JavaScript rendering, mapping, and crawling. Setup would download a compatible Chromium build once and reuse it. Chromium rendering is not available in the current source build.

```bash
websift setup
```

### Lite mode — current effective source mode

The current source build is effectively lite: it uses native HTTP and the worker extraction path, without Chromium. Search, static HTTP scraping, mapping, and bounded crawl operations remain available; JavaScript-only pages cannot be rendered.

```bash
websift setup --lite
```

Lite mode exists for servers, CI, minimal containers, and agents that only need documentation/static pages. It is not a separate product or codebase.

### Container mode

For operators who want SearXNG and all dependencies isolated:

```bash
docker compose up -d
```

The Compose profile includes Websift, managed Chromium, a persistent state volume, and optional SearXNG. Container mode is not required for ordinary local agent use.

## 4. Supported installation channels

### Native installer

Target:

```bash
curl -fsSL https://<project-domain>/install.sh | sh
```

The installer:

1. Detects supported OS and CPU architecture.
2. Downloads a versioned release archive, never source code to compile.
3. Verifies checksum and signed provenance before activation.
4. Installs into a user-writable location by default; no `sudo` requirement.
5. Includes the Rust executable, worker bundle, and private runtime needed by that worker.
6. Runs `websift doctor --quick`.

It must support a pinned version and non-interactive CI use. It must not silently edit agent configuration unless a `--client` option is explicitly provided.

### npm

Target:

```bash
npm install -g websift
websift install codex
```

or without a global installation:

```bash
npx -y websift install codex
```

The npm package is a thin launcher plus the TypeScript worker. Platform-specific optional packages carry prebuilt Rust executables; installation must not require Cargo, a C compiler, or `node-gyp`.

Shared/team configuration should pin a version rather than launch `@latest` on every agent startup. Global personal installation may follow the stable channel.

### Release archive

GitHub releases provide signed archives and checksums for supported platforms. This is the fallback for offline or managed environments.

### Docker

A pinned image and Compose file support Linux servers and reproducible self-hosting. `latest` is convenient for experiments but versioned tags are used in durable configurations.

## 5. Agent integration

The stable process contract is:

```bash
websift mcp --profile <client>
```

It starts an MCP stdio server. stdout is protocol-only; setup progress and diagnostics go to stderr. The client installer sets a stable profile automatically; users do not manage it.

### Unified installer

```bash
websift install codex
websift install claude-code
websift install hermes
websift install openclaw
websift install --detected
```

Rules:

- Detect the installed client and version before changing anything.
- Prefer the client's supported CLI over direct configuration-file edits.
- Default to user/global scope so the server is available across projects.
- Show the exact planned change and request confirmation unless `--yes` is provided.
- Remain idempotent: update the existing `websift` entry instead of duplicating it.
- Verify by starting MCP, listing tools, then stopping cleanly.
- On failure, leave the previous client configuration intact.

### Codex

The installer uses the supported equivalent of:

```bash
codex mcp add websift -- websift mcp --profile codex
```

### Claude Code

The installer uses user scope:

```bash
claude mcp add --scope user websift -- websift mcp --profile claude-code
```

### Hermes Agent

The installer uses its MCP manager:

```bash
hermes mcp add websift --command websift --args mcp --profile hermes
```

### OpenClaw

The preferred integration is a thin OpenClaw plugin whose manifest contributes a static stdio MCP definition:

```json
{
  "mcpServers": {
    "websift": {
      "transport": "stdio",
      "command": "websift",
      "args": ["mcp", "--profile", "openclaw"]
    }
  }
}
```

The plugin contains no crawler implementation. It only declares the already installed executable and can be published alongside the main release.

### Generic MCP client

For any other harness:

```json
{
  "mcpServers": {
    "websift": {
      "command": "websift",
      "args": ["mcp", "--profile", "default"]
    }
  }
}
```

## 6. First-run behavior

`websift install <client>` runs setup before registering the MCP process. This prevents a client startup timeout while Chromium is downloading.

Setup sequence:

1. Resolve writable binary, data, cache, and temporary directories.
2. Create/migrate embedded state.
3. Verify the bundled worker protocol.
4. In full mode, install or verify the pinned Chromium build.
5. Test native HTTP and browser extraction against a local fixture; no public website is contacted.
6. Check optional SearXNG configuration.
7. Register the selected client.
8. Start MCP, list tools, and stop it.

If Chromium setup fails, the user can choose lite mode; static functionality remains available. Failure is never hidden behind an empty scrape result.

## 7. Session and harness behavior

- Multiple calls in one agent session may run concurrently; each has an independent deadline and cancellation token.
- Multiple sessions of the same harness share that harness profile and durable crawl results safely.
- Different harnesses use different profiles by default, so their jobs, results, and cache are not visible to one another.
- All local profiles share the installed executable, worker bundle, Chromium files, and machine-wide concurrency ceiling.
- Closing an agent session stops its MCP process. Completed data remains; durable crawl work resumes when a process for that profile is available again.
- A crash cannot commit the same canonical page twice because URL leases expire and result commits are unique/idempotent.

This provides local session isolation, not protection between hostile users sharing one OS account. A future remote multi-user service requires authentication and tenant authorization.

## 8. Search backend experience

Every tool, including `web_search`, works immediately after install. Search uses a built-in keyless backend, so a user is never asked for a search URL, an account, or an API key.

A self-hosted SearXNG instance is an optional privacy upgrade for users who want their queries to leave their own infrastructure:

```bash
# Existing trusted SearXNG (target CLI; not yet shipped)
websift config set searxng.url https://search.example.com

# Local SearXNG through Docker when available (target CLI; not yet shipped)
websift setup --searxng
```

Until those commands ship, the same effect is available by setting `WEBSIFT_SEARXNG_URL` in the MCP client's `env` block. The instance must have `json` enabled under `search.formats` in its `settings.yml`; the default SearXNG configuration serves HTML only.

The project must not silently rotate through public SearXNG instances. A probe of ten popular public instances on 2026-08-11 found zero that answered `format=json`: most returned `429`, `403`, or HTML. Public instances may disable JSON, rate-limit automated use, disappear, or prohibit the workload.

Backend selection is automatic and reported, never guessed by the caller:

- `websift status` shows `search_backend` as `duckduckgo` or `searxng`.
- `web_search` results carry the serving backend in `source` and `meta.provider`.
- `websift doctor` notes that the built-in backend is in use and how to switch to SearXNG.

## 9. State, cache, and cleanup

Normal users never manage the state file directly.

```bash
websift status        # installation, browser, state size, search backend
websift doctor        # actionable health checks
websift cache clean   # remove expired cache only
websift uninstall     # remove client registrations; show package removal command
websift purge         # remove state/browser/cache after explicit confirmation
```

Requirements:

- State uses the platform-standard application-data directory.
- Temporary artifacts are removed after success/failure.
- Cache and crawl retention have safe defaults and size ceilings.
- Upgrades back up state before a non-trivial migration.
- A failed migration leaves the previous state usable.
- `purge` clearly lists paths and requires confirmation because it is destructive.

## 10. Updates and compatibility

- No background self-update in v1: an update happens only when the user asks for it.
- `websift update` compares the running version with the latest published release, downloads that
  release's binary for the running platform, verifies its SHA-256 against the checksum published
  beside it, and only then replaces the executable through a same-directory rename. A failed
  download, checksum, or rename leaves the installed binary untouched.
- `websift update --check` performs no mutation and reports `update_available`.
- Both print one JSON object, so a harness can act on the result without parsing prose.
- The updater resolves symlinks first, so it replaces the real binary rather than a link to it.
- Windows cannot overwrite a running image, so the previous executable is moved to `.old` and
  removed on a later run.
- Downloads go through the same public-address policy as retrieval: an update cannot be redirected
  to a private or loopback address, and HTTPS is never downgraded.
- The executable and worker are always upgraded as one release unit.
- Chromium compatibility is tied to that release and repaired by `websift setup`.
- Rollback retains a compatible state backup when a migration changed storage.
- MCP tool schemas follow the compatibility policy in `SPEC.md`.

## 11. Supported platforms for first release

Minimum release target:

- macOS ARM64 and x86_64.
- Linux x86_64 and ARM64 using glibc-compatible distributions.
- Windows x86_64 through npm and PowerShell; WSL is supported through the Linux path.

A platform is not listed as supported until CI installs, runs `doctor`, starts MCP, performs static and browser fixture extraction, and uninstalls it on that platform.

## 12. Packaging security

- Release archives, npm packages, and containers originate from the same tagged commit.
- npm publishes with provenance and platform packages use exact integrity metadata.
- Native installers verify archive checksum and signature before replacing an executable.
- Install scripts are short, readable, versioned, and support download-only inspection.
- No installation method runs a compiler or arbitrary dependency lifecycle script for the Rust binary.
- Browser download sources and hashes are pinned through the chosen Playwright release.
- Release artifacts include SBOM and third-party license notices.

## 13. Installation acceptance criteria

The project is not ready for public release until automated clean-machine tests prove:

- npm and native installation require no Rust toolchain or external database.
- Lite installation starts MCP without Chromium.
- Full setup installs Chromium before client registration and survives a second idempotent run.
- Codex, Claude Code, and Hermes registration produces exactly one working global entry.
- Two sessions of one harness and two different harness profiles run concurrently without duplicate work, cross-profile visibility, or exceeding the browser ceiling.
- The OpenClaw plugin resolves the packaged command without duplicating implementation.
- MCP starts without writing non-protocol text to stdout.
- Paths containing spaces and non-ASCII characters work.
- Upgrade preserves state; forced migration failure restores the previous usable state.
- Uninstall removes registrations but preserves user data unless purge is explicitly requested.
- Offline failure messages explain which previously downloaded features remain usable.

## 14. Explicit non-goals

- Requiring Docker for local MCP clients.
- Asking users to install or administer SQLite, PostgreSQL, or Redis.
- Compiling Rust or native npm modules on the user's machine.
- Downloading Chromium during an MCP client's startup handshake.
- Editing unknown third-party configuration formats when a supported client command or plugin exists.
- Offering an unmaintained public SearXNG instance as a hidden default.

## 15. References

- [Claude Code MCP installation](https://docs.anthropic.com/id/docs/claude-code/mcp)
- [OpenClaw MCP server manifest](https://docs.openclaw.ai/plugins/manifest#mcp-server-reference)
- Codex and Hermes command examples were verified against their installed CLI help on 2026-08-10; integration tests must recheck supported commands before release.
