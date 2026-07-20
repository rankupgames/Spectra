# Spectra

**An adaptive context runtime for local AI coding agents.**

[![Rust 1.88+](https://img.shields.io/badge/Rust-1.88%2B-CE412B?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Version: 0.4.0](https://img.shields.io/badge/Version-0.4.0-38BDF8.svg)](https://github.com/rankupgames/Spectra/releases/tag/v0.4.0)
[![License: MIT](https://img.shields.io/badge/License-MIT-22C55E.svg)](LICENSE)
[![Status: Prototype](https://img.shields.io/badge/Status-Prototype-F59E0B.svg)](#project-status)

AI coding agents are good at working with code. They are less good at remembering a whole codebase without repeatedly loading file trees, source dumps, terminal logs, and old conversation into context.

Spectra is an experiment in fixing that. Its adaptive context runtime selects the smallest useful packet for the next decision, remembers which evidence an exact agent session has already received, and creates a visual map only when one is requested. The goal is simple: spend fewer tokens rediscovering context, and more of them doing the actual work.

```text
Query + lifecycle ──▶ adaptive selector ──▶ budgeted evidence packet
Polyglot repository ──code adapters──▶ topology graph ──▶ exact anchors + optional PNG
Agent lifecycle ──adapter hooks──▶ immutable ledger ──▶ bounded continuity
```

Instead of dumping source up front, Spectra lets the model see the shape of the system, choose an exact `path:start-end` anchor, and read code once it knows what it is looking for.

> [!IMPORTANT]
> Spectra is an early prototype. The v0.4 runtime retains the complete CodeGraph v1.3.0 language and extension surface with 39 adapters. The harness-neutral Ledger is verified for Codex, Claude Code, and Gemini CLI; Cursor support is intentionally partial because it can reinject continuity only at session start. See [Project status](#project-status) before relying on it in production.

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

### Requirements

- At least one supported local agent: Claude Code, Cursor, Codex, OpenCode, Hermes Agent, Gemini CLI, Antigravity, or Kiro
- A repository containing at least one supported source language

Any MCP client can also run `spectra serve --mcp` manually.

### 1. Install Spectra

macOS and Linux:

```sh
curl --proto '=https' --tlsv1.2 -fsSL https://raw.githubusercontent.com/rankupgames/Spectra/v0.4.0/install.sh | sh
```

Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/rankupgames/Spectra/v0.4.0/install.ps1 | iex
```

Package-manager and source-install alternatives:

```sh
brew install rankupgames/tap/spectra
scoop bucket add rankupgames https://github.com/rankupgames/scoop-bucket
scoop install rankupgames/spectra
cargo install --git https://github.com/rankupgames/Spectra.git --tag v0.4.0 --bin spectra --locked
```

Cargo installation requires Rust 1.88 or newer. Every release archive is covered by the published `SHA256SUMS`, Sigstore bundle, and build provenance; both direct installers verify the archive checksum before installing it.

### 2. Connect your agents

```sh
spectra install
```

In a terminal, Spectra opens a short wizard that scans for supported agents, shows each capability tier, asks where to install, preflights conflicts, and confirms before writing. For unattended global installation of every detected target, use `spectra install --yes`. Restart any agents it reports. If Codex is among them, review the **Spectra context ledger** hook once; Codex keeps that trust decision in your hands.

Confirm the installation:

```sh
spectra status
```

Expected output:

```text
Claude Code: MCP=current, Ledger hooks=current, Capability=topology+ledger
Codex: MCP=current, Ledger hooks=current, Capability=topology+ledger
Cursor: MCP=current, Ledger hooks=current, Capability=topology+ledger-partial
Gemini CLI: MCP=current, Ledger hooks=current, Capability=topology+ledger
```

Your output only lists the agents Spectra detected. Codex, Claude Code, and Gemini CLI have recorded-wire `topology+ledger` coverage. Cursor is visibly labeled `topology+ledger-partial`: it records lifecycle facts but can reinject context only at `sessionStart`, not before every prompt.

After that, there is nothing to babysit. The long-lived MCP process watches every project it serves, reconciles changes after a short debounce, and performs a startup catch-up before accepting requests. Maps still check for changes synchronously, so a degraded or unavailable watcher cannot silently return a stale topology. Concurrent MCP servers, CLI commands, and fallback hooks coordinate through a heartbeat-backed project lock. You do not need to run a separate daemon, remember a `sync` command, or initialize each repository by hand.

### 3. Use it

Ask your agent an architecture or navigation question, for example:

```text
Use Spectra to find how request routing reaches persistence.
```

Or inspect the adaptive packet directly:

```sh
spectra context "how does request routing reach persistence" --path /path/to/project
```

Render a map only when the visual topology is useful:

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
├── context-receipts-v1.json hashed per-session evidence receipts
├── metrics-v1.json        local aggregate efficiency counters
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

Provider adapters for Codex, Claude Code, Gemini CLI, and Cursor normalize supported authorization, edit, verification, completion, and blocker events into the same private boundary. Codex, Claude, and Gemini receive a short session-aware projection at their documented context hooks. Cursor records the same bounded facts but reinjects only at session start.

## CLI reference

```text
spectra install [--agent AGENT] [--location global|local] [--path REPO] [--yes] [--topology-only] [--dry-run] [--no-color]
spectra status [--agent AGENT] [--path REPO] [--json]
spectra uninstall [--agent AGENT] [--location global|local] [--path REPO] [--dry-run]

spectra init [PATH] [--force] [--json] [--no-color]
spectra sync [PATH] [--quiet]
spectra autosync install [PATH]
spectra autosync status [PATH]
spectra autosync remove [PATH]
spectra context <QUERY> [--path PATH] [--token-budget 128..2000] [--intent auto|resume|locate|flow|change|inspect]
                [--representation text|map] [--delivery delta|full]
                [--source-harness HARNESS --session-id ID] [--cursor CURSOR]
spectra map <QUERY> [--path PATH] [--max-nodes 1..96] [--out DIR]
spectra stats [--path PATH] [--json] [--reset]
spectra serve --mcp
spectra lifecycle ingest
spectra hook [--agent codex|claude|gemini|cursor]
```

`spectra init` is an optional eager-indexing command for diagnostics and benchmarks. It reports index version, file/node/edge totals, database size, node kinds, languages, synchronization state, and elapsed time; `--json` emits the stable report. Home and filesystem roots require `--force`. `spectra sync` exposes the same reconciliation path used by the watcher and is intentionally quiet-capable for automation. Neither command is required during ordinary MCP use.

On filesystems where native recursive watching is unavailable or unreliable, `spectra autosync install` adds marked blocks to the repository's `post-commit`, `post-merge`, and `post-checkout` hooks. Each hook launches `spectra sync --quiet` in the background. Installation is idempotent, honors Git's resolved hooks directory, preserves existing hook bodies, and `spectra autosync remove` deletes only Spectra-owned blocks.

`--agent auto` is the default and configures every detected agent. `--agent all` attempts every adapter; agents that expose configuration only through their own CLI must already be installed. Non-interactive use requires an explicit `--agent` or `--yes`, so it never waits for wizard input. Verified local configuration is available for Codex, Claude Code, Gemini CLI, and Cursor.

The installer is idempotent and ownership-aware: it updates stale Spectra registrations, preserves unrelated settings and comments, writes configuration atomically, and refuses to overwrite or remove a foreign MCP entry named `spectra`.

## MCP interface

Spectra advertises one tool by default:

```text
spectra_context(
  query, projectPath?, tokenBudget?, intent?, representation?,
  delivery?, source?, cursor?
)
```

`spectra_context` routes `auto`, `resume`, `locate`, `flow`, `change`, and `inspect` intents through the existing Ledger and graph engines. It returns atomic continuity, anchor, relation, change, test, boundary, source-window, and next-action evidence as compact Context Packet v1 text. The default budget is 600 estimated tokens. Continuation cursors bind the query, intent, and index version, and fail as `cursor_stale` instead of mixing changed results.

Text is the default. `representation=map` appends the existing PNG content block and identifies its cost as provider-controlled; the text packet remains budgeted independently. With an exact `{harness, sessionId}` source, `delivery=delta` suppresses evidence already delivered to that session. Without an exact source, Spectra safely returns a full packet and performs no deduplication. `delivery=full` resets the session baseline.

All twelve v0.3 tools remain available without rebuilding. Set `SPECTRA_MCP_TOOLS=all`, or provide a comma-separated short-name allowlist such as `context,brief,map,changes,path,explore`. Existing allowlists and snake-case aliases remain valid. The legacy tools are:

```text
spectra_brief(query, projectPath?, tokenBudget?, detail?, source?)
spectra_map(query, projectPath?, maxNodes?)
spectra_changes(projectPath?, base?, paths?, depth?, includeTests?, tokenBudget?)
spectra_path(from, to, fromFile?, toFile?, mode?, maxHops?, projectPath?)
spectra_explore(query, maxFiles?, projectPath?, tokenBudget?)
spectra_search(query, kind?, limit?, projectPath?)
spectra_callers(symbol, file?, limit?, projectPath?)
spectra_callees(symbol, file?, limit?, projectPath?)
spectra_impact(symbol, file?, depth?, projectPath?)
spectra_node(symbol?, file?, includeCode?, offset?, limit?, symbolsOnly?, line?, projectPath?)
spectra_status(projectPath?)
spectra_files(path?, pattern?, format?, includeMetadata?, maxDepth?, projectPath?)
```

Use `spectra_changes` for worktree-to-symbol impact and ranked test discovery; explicit paths work without Git. Use `spectra_path` for up to three deterministic directed execution or dependency paths with exact anchors. Use `spectra_explore` for a deeper bounded source-and-call-path read after brief identifies an anchor. The remaining tools provide focused symbol, relationship, file-tree, project inventory, and index-health queries. All support cross-project queries. Configuration values in YAML and properties files are withheld from source responses.

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
- **`spectra`:** CLI commands, stdio MCP transport, watcher-backed automatic synchronization, multi-agent installation, harness-neutral lifecycle ingestion, private provider-hook translation, and benchmark runners.

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
- Context receipts store only a salted session-key digest, evidence hashes, sequence metadata, and access metadata; corruption and write failure fail open to a full response.
- Efficiency metrics are local aggregate counters and are never networked. Set `SPECTRA_METRICS=off` to disable collection, inspect them with `spectra stats`, or explicitly clear them with `spectra stats --reset`.
- Credential-shaped values are redacted before persistence.
- Hook retries use correlation IDs so immutable events are not duplicated.
- Index writers use an ownership-checked heartbeat lock across MCP, CLI, and Git-hook processes.
- Cross-process transactions serialize concurrent Ledger writers.
- Malformed or unsupported hook events fail open and cannot block the agent loop.
- The `.env` file and `.spectra/` runtime data are ignored by Git.

Provider hooks remain fail-open and record only their documented lifecycle surfaces. Spectra does not claim to intercept every OS process or terminal operation. Cursor's session-start-only reinjection remains an explicit partial capability.

## Development and verification

```sh
cargo fmt --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --release --workspace --locked
cargo run -p spectra-context --bin spectra-v04-gate -- benchmarks/results/reviewed-v0.4.json
```

The benchmark protocol, frozen prompts, raw evaluation data, and replay fixtures live under [`benchmarks/`](benchmarks/README.md).

## Project status

Implemented:

- adapter-driven topology extraction and incremental indexing across all 39 CodeGraph v1.3.0 language families and dialect adapters
- CodeGraph-parity server framework routes plus React/Next, SwiftUI, React Native, Expo Module, and Fabric client/native bridges
- embedded JavaScript/TypeScript bridges, component rendering and event bindings, and conventional SvelteKit, Nuxt, and Astro page routes
- query-focused deterministic PNG and SVG rendering
- budgeted Context Packet v1 responses with deterministic intent routing, atomic evidence packing, bounded source windows, stale-safe continuation cursors, and explicit-only maps
- session-durable evidence deduplication with hashed receipts, bounded LRU storage, concurrent writers, corruption recovery, and fail-open delivery
- local privacy-safe efficiency metrics with opt-out, inspection, and explicit reset
- the complete CodeGraph v1.3.0 MCP query capability set, with `spectra_context` as the one-tool default and all twelve v0.3 tools behind compatible allowlists
- automatic MCP installation for Claude Code, Cursor, Codex, OpenCode, Hermes Agent, Gemini CLI, Antigravity, and Kiro
- automatic lifecycle-hook installation for Codex, Claude Code, Gemini CLI, and Cursor
- append-only, per-session State Machine Ledger with replay, recovery, redaction, concurrency control, cross-harness project facts, and bounded projection
- stable harness-neutral `spectra lifecycle ingest` JSON v1 protocol
- deterministic, provider-backed, and recorded-hook regression suites
- pinned real-repository parity gates covering framework routes and multimodal topology quality
- cross-platform release archives, checksums, Sigstore signing/provenance, checksum-verifying installers, and generated Homebrew/Scoop manifests
- a reviewed-report gate enforcing the v0.4 polyglot efficiency, solve-rate, repetition, call-count, budget, and privacy thresholds

Not yet implemented:

- per-prompt Cursor context reinjection (the host currently exposes reliable reinjection only at session start)
- complete unified-shell interception
- automatic updater
- Tauri observability UI
- public graph-extension SDK

The v0.4 release turns that topology and continuity foundation into an adaptive, text-first context runtime. Existing index-v4, ledger-v1, lifecycle-v1, MCP commands, hook installations, and legacy tool response contracts remain compatible.

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
