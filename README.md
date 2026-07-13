# Spectra

**A smaller, more useful memory for local AI coding agents.**

[![Rust 1.88+](https://img.shields.io/badge/Rust-1.88%2B-CE412B?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Version: 0.2.0](https://img.shields.io/badge/Version-0.2.0-38BDF8.svg)](https://github.com/rankupgames/Spectra/releases/tag/v0.2.0)
[![License: MIT](https://img.shields.io/badge/License-MIT-22C55E.svg)](LICENSE)
[![Status: Prototype](https://img.shields.io/badge/Status-Prototype-F59E0B.svg)](#project-status)

AI coding agents are good at working with code. They are less good at remembering a whole codebase without repeatedly loading file trees, source dumps, terminal logs, and old conversation into context.

Spectra is an experiment in fixing that. It gives an agent two things: a visual map of the codebase and a small, durable record of what has already happened. The goal is simple: spend fewer tokens rediscovering context, and more of them doing the actual work.

```text
Polyglot repository ──code adapters──▶ topology graph ──▶ PNG map + exact anchors
Agent lifecycle ──adapter hooks──▶ immutable ledger ──▶ bounded state context
```

Instead of dumping source up front, Spectra lets the model see the shape of the system, choose an exact `path:start-end` anchor, and read code once it knows what it is looking for.

> [!IMPORTANT]
> Spectra is an early prototype. The v0.2 registry covers the complete CodeGraph v1.3.0 language and extension surface with 39 adapters, the matching framework/bridge packs are implemented, and the pinned representative-repository gates pass. Automatic topology setup supports eight local agents, while Lifecycle Ledger integration is currently limited to Codex. See [Project status](#project-status) before relying on it in production.

The [agent support contract](docs/agent-support.md) tracks topology and Ledger support separately so an MCP integration is never mistaken for lifecycle coverage.

## Why Spectra?

Most code-context tools answer with source and explanation together. That can be useful, but it also means paying for the same code again when the conversation moves on. Spectra separates finding the right code from reading it:

- **See the system first.** Spectra turns the relevant part of the architecture into a deterministic 1536×1024 map.
- **Read with a purpose.** Every visual ID points back to an exact file and line range.
- **Keep the answer small.** Maps show 48 nodes by default and never more than 96.
- **Stay current.** The MCP server watches served projects in the background, and every map retains a synchronous refresh fallback.
- **Remember outcomes, not noise.** The Ledger keeps edits, test results, and blockers without saving full conversations or terminal output.
- **Keep it local.** Parsing, indexing, rendering, selection, and replay all happen on your machine.

### Prototype results

| Evaluation | Result |
| --- | ---: |
| Median provider-input reduction | **88.3%** |
| Composite recall-proxy retention | **118.0%** |
| Expected-anchor recall | **85.2%** (CodeGraph: 68.5%) |
| Ledger median estimated-token reduction | **93.4%** |
| Ledger fact retention | **100%** |
| Maximum Ledger projection | **57 tokens** |

These numbers come from nine frozen prompts across pinned ripgrep, Tokio, and rust-analyzer repositories using Grok 4.5. They are encouraging, but they are still prototype results—not a promise that every model and repository will behave the same way. The reproducible methodology, prompts, and limitations are documented in the [benchmark protocol](benchmarks/README.md). Generated result artifacts stay local and are not committed.

## Quickstart

Spectra does not have prebuilt binaries yet, but Cargo can install the tagged v0.2.0 release directly from GitHub:

### Requirements

- Rust 1.88 or newer
- At least one supported local agent: Claude Code, Cursor, Codex, OpenCode, Hermes Agent, Gemini CLI, Antigravity, or Kiro
- A repository containing at least one supported source language

Any MCP client can also run `spectra serve --mcp` manually.

### 1. Install Spectra

```sh
cargo install --git https://github.com/rankupgames/Spectra.git --tag v0.2.0 --bin spectra --locked
```

### 2. Connect your agents

```sh
spectra install
```

Spectra detects every supported agent installed on your machine and configures all of them in one pass. Restart any agents it reports. If Codex is among them, open `/hooks` and review the **Spectra context ledger** hook once; Codex keeps that trust decision in your hands.

Confirm the installation:

```sh
spectra status
```

Expected output:

```text
Claude Code: MCP=current, Ledger=not available
Codex: MCP=current, Ledger hooks=current
```

Your output only lists the agents Spectra detected. Every supported agent gets visual topology; Codex also gets the lifecycle Ledger. Spectra will not claim Ledger support for another agent until that agent's lifecycle protocol is documented and replay-tested.

After that, there is nothing to babysit. The long-lived MCP process watches every project it serves, reconciles changes after a short debounce, and performs a startup catch-up before accepting requests. Maps still check for changes synchronously, so a degraded or unavailable watcher cannot silently return a stale topology. Concurrent MCP servers, CLI commands, and fallback hooks coordinate through a heartbeat-backed project lock. You do not need to run a separate daemon, remember a `sync` command, or initialize each repository by hand.

### 3. Use it

Ask your agent an architecture or navigation question, for example:

```text
Use Spectra to map how request routing reaches persistence.
```

Or render a map directly:

```sh
spectra map "how does request routing reach persistence" --path /path/to/project
```

Spectra returns a PNG and compact semantic metadata:

```text
N1=method impl Router::dispatch @ src/router.rs:18-47
N2=method impl Store::save @ src/store.rs:9-31
flow N1 -calls-> N2
nodes=42 truncated=false index=v4
```

From there, the agent can pick an anchor and open the part of the source that actually matters. MCP responses append watcher health as `autosync=active|degraded pending=N`; direct CLI maps omit that server-only line.

## What happens behind the scenes

Normal use creates a project-local `.spectra/` directory containing generated state:

```text
.spectra/
├── index-v4.json          incremental polyglot code index
├── ledger-v1.jsonl        append-only context ledger
└── artifacts/             generated PNG and SVG maps
```

When the MCP server starts, Spectra registers a recursive filesystem watcher before its initial catch-up refresh. Supported source changes, directory changes, and ignore-rule changes are collected for two seconds and reconciled as one batch. Failed reconciliations retain their pending paths and retry; watcher setup failures are reported as degraded, retried on the next request, and backed by request-time refresh. Runtime writes under `.spectra/` and internal Git activity do not trigger feedback loops.

Index reconciliation is serialized across processes. The lock records a unique owner, refreshes a heartbeat during long scans, recovers abandoned locks, and remains held until the matching RepositorySynced Ledger event is committed. This allows several agent MCP processes and Git hooks to share one project index without corrupting the cache or duplicating material sync events.

Whenever an agent asks for a map, Spectra:

1. Traverses the repository while respecting `.gitignore`.
2. Detects each supported file through the adapter registry, reuses unchanged fragments, and reparses changed files with the matching grammar.
3. Resolves high-confidence structure and relationships.
4. Marks uncertain calls as dashed boundaries instead of presenting guesses as facts.
5. Selects and renders a query-focused subgraph.
6. Records the synchronization and map outcome in the Ledger.

On Codex, the Ledger also notices supported approvals, `apply_patch` edits, verification commands, and turn completion. When a new session or prompt begins, the agent receives a short state projection instead of a replay of everything Spectra observed.

## CLI reference

```text
spectra install [--agent auto|all|claude|cursor|codex|open-code|hermes|gemini|antigravity|kiro] [--dry-run]
spectra status [--agent auto|all|claude|cursor|codex|open-code|hermes|gemini|antigravity|kiro]
spectra uninstall [--agent auto|all|claude|cursor|codex|open-code|hermes|gemini|antigravity|kiro] [--dry-run]

spectra init [PATH]
spectra sync [PATH] [--quiet]
spectra autosync install [PATH]
spectra autosync status [PATH]
spectra autosync remove [PATH]
spectra map <QUERY> [--path PATH] [--max-nodes 1..96] [--out DIR]
spectra serve --mcp
```

`spectra init` is an optional eager-indexing command for diagnostics and benchmarks. `spectra sync` exposes the same reconciliation path used by the watcher and is intentionally quiet-capable for automation. Neither command is required during ordinary MCP use.

On filesystems where native recursive watching is unavailable or unreliable, `spectra autosync install` adds marked blocks to the repository's `post-commit`, `post-merge`, and `post-checkout` hooks. Each hook launches `spectra sync --quiet` in the background. Installation is idempotent, honors Git's resolved hooks directory, preserves existing hook bodies, and `spectra autosync remove` deletes only Spectra-owned blocks.

`--agent auto` is the default and configures every detected agent. `--agent all` attempts every adapter; agents that expose configuration only through their own CLI must already be installed.

The installer is idempotent and ownership-aware: it updates stale Spectra registrations, preserves unrelated settings and comments, writes configuration atomically, and refuses to overwrite or remove a foreign MCP entry named `spectra`.

## MCP interface

Spectra keeps the default MCP surface to one primary visual tool, matching CodeGraph's one-tool default:

```text
spectra_map(query, projectPath?, maxNodes?)
```

Its response contains an `image/png` content block followed by compact anchor metadata. It never includes source bodies. The legacy snake-case parameter spellings remain accepted.

The full CodeGraph-parity query pack is available without rebuilding. Set `SPECTRA_MCP_TOOLS=all`, or provide a comma-separated short-name allowlist such as `map,explore,node,status`. The available tools are:

```text
spectra_explore(query, maxFiles?, projectPath?)
spectra_search(query, kind?, limit?, projectPath?)
spectra_callers(symbol, file?, limit?, projectPath?)
spectra_callees(symbol, file?, limit?, projectPath?)
spectra_impact(symbol, file?, depth?, projectPath?)
spectra_node(symbol?, file?, includeCode?, offset?, limit?, symbolsOnly?, line?, projectPath?)
spectra_status(projectPath?)
spectra_files(path?, pattern?, format?, includeMetadata?, maxDepth?, projectPath?)
```

These tools provide bounded line-numbered source exploration, symbol and relationship queries, impact traversal, source/file views, project inventory, and index health. All support cross-project queries. Configuration values in YAML and properties files are withheld from source responses.

The final metadata line reports watcher health as `autosync=active|degraded pending=N`. Watch registration honors repository ignore rules so generated build trees do not consume native watcher resources; macOS also uses an ignore-aware source polling fallback if FSEvents misses a change. Set `SPECTRA_WATCH_DEBOUNCE_MS` to a value from 100 through 60000 to override the default 2000 ms debounce window for unusually bursty repositories.

Automatic configuration is recommended. For a manual setup, register an MCP server named `spectra` that runs:

```text
/absolute/path/to/spectra serve --mcp
```

Equivalent Codex configuration:

```toml
[mcp_servers.spectra]
command = "/absolute/path/to/spectra"
args = ["serve", "--mcp"]
```

## Architecture

The workspace is intentionally split into two small layers:

- **`spectra-core`:** packed graph primitives, language-adapter extraction and resolution, deterministic selection and rendering, incremental indexing, and the State Machine Ledger.
- **`spectra`:** CLI commands, stdio MCP transport, watcher-backed automatic synchronization, multi-agent installation, Codex lifecycle-hook translation, and benchmark runners.

The internal graph kernel is domain-neutral:

- contiguous `NodeId` and `EdgeId` arrays
- interned `AtomId` values
- typed scalar attributes
- adjacency indexes and invariant validation
- code-specific `SourceSpan` data kept in a separate sidecar

Every adapter maps parser-backed or structured extraction into the same graph vocabulary. The current v0.2 packs cover the complete CodeGraph v1.3.0 language surface, including structural symbols, imports, calls, inheritance, implementations, configuration references, templates, infrastructure blocks, and legacy execution edges where applicable. Web components additionally parse embedded JavaScript or TypeScript, connect template events to script handlers, resolve rendered components, and model SvelteKit, Nuxt, Astro, Razor, Drupal, and ArkUI routes. Native adapters connect Objective-C interfaces, protocols, implementations, and message sends, and distinguish CUDA and Metal entry-point kernels from ordinary functions. Structured adapters cover Terraform/OpenTofu, Nix, YAML, XML/MyBatis, properties, Twig, CFML queries, and COBOL/CICS. Ambiguous targets remain explicit uncertain boundaries. Rendering condenses cycles, layers nodes, clusters related code, routes typed edges, and emits deterministic SVG and PNG artifacts.

The adapter contract, functional acceptance bar, and CodeGraph parity matrix are tracked in [Code adapters](docs/code-adapters.md).

See the [Ledger design and maintenance boundaries](docs/state-machine-ledger.md) for the state-machine contract.

## Privacy and safety

Spectra should not become another transcript database. It deliberately keeps less:

- Source bodies are excluded from topology responses.
- Prompts, assistant messages, patch bodies, and terminal output bodies are not written to the Ledger.
- Credential-shaped values are redacted before persistence.
- Hook retries use correlation IDs so immutable events are not duplicated.
- Index writers use an ownership-checked heartbeat lock across MCP, CLI, and Git-hook processes.
- Cross-process transactions serialize concurrent Ledger writers.
- Malformed or unsupported hook events fail open and cannot block the agent loop.
- The `.env` file and `.spectra/` runtime data are ignored by Git.

Codex currently documents incomplete hook coverage for richer unified shell execution. Spectra records only supported lifecycle events and does not claim to intercept every OS process or terminal operation.

## Development and verification

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release --workspace
```

The benchmark protocol, frozen prompts, raw evaluation data, and replay fixtures live under [`benchmarks/`](benchmarks/README.md).

## Project status

Implemented:

- adapter-driven topology extraction and incremental indexing across all 39 CodeGraph v1.3.0 language families and dialect adapters
- CodeGraph-parity server framework routes plus React/Next, SwiftUI, React Native, Expo Module, and Fabric client/native bridges
- embedded JavaScript/TypeScript bridges, component rendering and event bindings, and conventional SvelteKit, Nuxt, and Astro page routes
- query-focused deterministic PNG and SVG rendering
- bounded MCP image and anchor responses
- the complete CodeGraph v1.3.0 MCP query capability set, with a one-tool default and allowlist-enabled explore/search/traversal/node/files/status tools
- automatic MCP installation for Claude Code, Cursor, Codex, OpenCode, Hermes Agent, Gemini CLI, Antigravity, and Kiro
- automatic Codex lifecycle-hook installation
- append-only State Machine Ledger with replay, recovery, redaction, concurrency control, and bounded projection
- deterministic, provider-backed, and recorded-hook regression suites
- pinned real-repository parity gates covering framework routes and multimodal topology quality

Not yet implemented:

- non-Codex Ledger adapters without a verified lifecycle protocol and recorded-wire replay
- complete unified-shell interception
- packaged release binaries and automatic updater
- Tauri observability UI
- public graph-extension SDK

The v0.2 release delivers functional CodeGraph language parity: adapters, ecosystem routing, cross-language bridges, CodeGraph-parity MCP queries, seamless autosync, semantic map metadata, and measured real-repository coverage. Packaged installers that do not require a Rust toolchain follow that work.

## Contributing

Spectra is young, and thoughtful help is welcome. Small, focused pull requests are much easier to review than sweeping rewrites. Please keep features modular, preserve deterministic output, and do not add source or terminal bodies to model-facing payloads. New dependencies need a quick maintenance and release-health check before they come in.

Before opening a pull request, run the full verification commands above and include regression evidence for changes affecting selection, rendering, indexing, installation, or the Ledger.

## Support Spectra

If Spectra saves you some tokens, time, or frustration, and you would like to help fund the next round of work, you can send a SOL donation here:

**Network:** Solana Mainnet  
**Asset:** Native SOL  
**Recipient:**

```text
5bK9UNxJoaENxTYh2ZFMpuhuu8iA2MNfBoWGMzrshH96
```

[Verify the address on Solana Explorer](https://explorer.solana.com/address/5bK9UNxJoaENxTYh2ZFMpuhuu8iA2MNfBoWGMzrshH96)

If your wallet supports Solana Pay, this amount-free request lets you choose the donation amount:

```text
solana:5bK9UNxJoaENxTYh2ZFMpuhuu8iA2MNfBoWGMzrshH96?label=Spectra&message=Support%20Spectra%20development
```

> [!CAUTION]
> Send only native SOL on the Solana network. Always verify the entire recipient address in your wallet before confirming a transaction. Cryptocurrency transfers are irreversible.

No pressure, of course. Using the project, reporting a bug, or sharing a benchmark is valuable too. Thank you for helping Spectra grow in the open.

## License

Spectra is available under the [MIT License](LICENSE).
