# Spectra

**A smaller, more useful memory for local AI coding agents.**

[![Rust 1.88+](https://img.shields.io/badge/Rust-1.88%2B-CE412B?logo=rust&logoColor=white)](https://www.rust-lang.org/)
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
> Spectra is an early prototype. The v0.2 adapter registry currently supports Rust, TypeScript/TSX, JavaScript/JSX, Python, Go, Java, C, C++, C#, PHP, Ruby, Swift, Kotlin, Scala, Dart, Lua, Luau, Svelte, Vue, Astro, Liquid, Objective-C, CUDA, and Metal; CodeGraph language parity is still in progress. Automatic topology setup supports eight local agents, while Lifecycle Ledger integration is currently limited to Codex. See [Project status](#project-status) before relying on it in production.

The [agent support contract](docs/agent-support.md) tracks topology and Ledger support separately so an MCP integration is never mistaken for lifecycle coverage.

## Why Spectra?

Most code-context tools answer with source and explanation together. That can be useful, but it also means paying for the same code again when the conversation moves on. Spectra separates finding the right code from reading it:

- **See the system first.** Spectra turns the relevant part of the architecture into a deterministic 1536×1024 map.
- **Read with a purpose.** Every visual ID points back to an exact file and line range.
- **Keep the answer small.** Maps show 48 nodes by default and never more than 96.
- **Stay current.** Changed, added, and deleted supported source files are refreshed before a map is returned.
- **Remember outcomes, not noise.** The Ledger keeps edits, test results, and blockers without saving full conversations or terminal output.
- **Keep it local.** Parsing, indexing, rendering, selection, and replay all happen on your machine.

### Prototype results

| Evaluation | Result |
| --- | ---: |
| Median provider-input reduction | **89.7%** |
| Composite-quality retention | **98.3%** |
| Ledger median estimated-token reduction | **93.4%** |
| Ledger fact retention | **100%** |
| Maximum Ledger projection | **57 tokens** |

These numbers come from nine frozen prompts across pinned ripgrep, Tokio, and rust-analyzer repositories using Grok 4.5. They are encouraging, but they are still prototype results—not a promise that every model and repository will behave the same way. The reproducible methodology, prompts, and limitations are documented in the [benchmark protocol](benchmarks/README.md). Generated result artifacts stay local and are not committed.

## Quickstart

Spectra does not have packaged binaries yet, but Cargo can install the current release candidate directly from GitHub:

### Requirements

- Rust 1.88 or newer
- At least one supported local agent: Claude Code, Cursor, Codex, OpenCode, Hermes Agent, Gemini CLI, Antigravity, or Kiro
- A repository containing at least one supported source language

Any MCP client can also run `spectra serve --mcp` manually.

### 1. Install Spectra

```sh
cargo install --git https://github.com/rankupgames/Spectra.git --bin spectra --locked
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

After that, there is nothing to babysit. Spectra creates project data when it is first needed and checks for changes before every map. You do not need to run a daemon, remember a `sync` command, or initialize each repository by hand.

### 3. Use it

Ask your agent an architecture or navigation question, for example:

```text
Use Spectra to map how request routing reaches persistence.
```

Or render a map directly:

```sh
spectra map "how does request routing reach persistence" --path /path/to/project
```

Spectra returns a PNG and compact metadata:

```text
N1=src/router.rs:18-47
N2=src/store.rs:9-31
nodes=42 truncated=false index=v2
```

From there, the agent can pick an anchor and open the part of the source that actually matters.

## What happens behind the scenes

Normal use creates a project-local `.spectra/` directory containing generated state:

```text
.spectra/
├── index-v3.json          incremental polyglot code index
├── ledger-v1.jsonl        append-only context ledger
└── artifacts/             generated PNG and SVG maps
```

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
spectra map <QUERY> [--path PATH] [--max-nodes 1..96] [--out DIR]
spectra serve --mcp
```

`spectra init` is an optional eager-indexing command for diagnostics and benchmarks. It is not required during ordinary use.

`--agent auto` is the default and configures every detected agent. `--agent all` attempts every adapter; agents that expose configuration only through their own CLI must already be installed.

The installer is idempotent and ownership-aware: it updates stale Spectra registrations, preserves unrelated settings and comments, writes configuration atomically, and refuses to overwrite or remove a foreign MCP entry named `spectra`.

## MCP interface

Spectra exposes one stdio MCP tool:

```text
spectra_map(query, project_path?, max_nodes?)
```

Its response contains an `image/png` content block followed by compact anchor metadata. It never includes source bodies.

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
- **`spectra`:** CLI commands, stdio MCP transport, multi-agent installation, Codex lifecycle-hook translation, and benchmark runners.

The internal graph kernel is domain-neutral:

- contiguous `NodeId` and `EdgeId` arrays
- interned `AtomId` values
- typed scalar attributes
- adjacency indexes and invariant validation
- code-specific `SourceSpan` data kept in a separate sidecar

Every adapter maps its grammar into the same graph vocabulary. The current v0.2 packs cover Rust, TypeScript/TSX, JavaScript/JSX, Python, Go, Java, C, C++, C#, PHP, Ruby, Swift, Kotlin, Scala, Dart, Lua, Luau, Svelte, Vue, Astro, Liquid, Objective-C, CUDA, and Metal, including structural symbols, containment, imports, calls, inheritance, and implementations where the language exposes them. Web components additionally parse embedded JavaScript or TypeScript, connect template events to script handlers, resolve rendered components, and model SvelteKit, Nuxt, and Astro page routes. Native adapters connect Objective-C interfaces, protocols, implementations, and message sends, and distinguish CUDA and Metal entry-point kernels from ordinary functions. Ambiguous targets remain explicit uncertain boundaries. Rendering condenses cycles, layers nodes, clusters related code, routes typed edges, and emits deterministic SVG and PNG artifacts.

The adapter contract, functional acceptance bar, and CodeGraph parity matrix are tracked in [Code adapters](docs/code-adapters.md).

See the [Ledger design and maintenance boundaries](docs/state-machine-ledger.md) for the state-machine contract.

## Privacy and safety

Spectra should not become another transcript database. It deliberately keeps less:

- Source bodies are excluded from topology responses.
- Prompts, assistant messages, patch bodies, and terminal output bodies are not written to the Ledger.
- Credential-shaped values are redacted before persistence.
- Hook retries use correlation IDs so immutable events are not duplicated.
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

- adapter-driven topology extraction and incremental indexing for Rust, TypeScript/TSX, JavaScript/JSX, Python, Go, Java, C, C++, C#, PHP, Ruby, Swift, Kotlin, Scala, Dart, Lua, Luau, Svelte, Vue, Astro, Liquid, Objective-C, CUDA, and Metal
- embedded JavaScript/TypeScript bridges, component rendering and event bindings, and conventional SvelteKit, Nuxt, and Astro page routes
- query-focused deterministic PNG and SVG rendering
- bounded MCP image and anchor responses
- automatic MCP installation for Claude Code, Cursor, Codex, OpenCode, Hermes Agent, Gemini CLI, Antigravity, and Kiro
- automatic Codex lifecycle-hook installation
- append-only State Machine Ledger with replay, recovery, redaction, concurrency control, and bounded projection
- deterministic, provider-backed, and recorded-hook regression suites

Not yet implemented:

- remaining CodeGraph-parity language adapters
- framework routing and cross-language bridges beyond the common semantic resolver
- non-Codex Ledger adapters without a verified lifecycle protocol and recorded-wire replay
- complete unified-shell interception
- packaged release binaries and automatic updater
- Tauri observability UI
- public graph-extension SDK

The v0.2 milestone is functional CodeGraph language parity: adapters, ecosystem routing, cross-language bridges, and measured real-repository coverage. Packaged installers that do not require a Rust toolchain follow that work.

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
