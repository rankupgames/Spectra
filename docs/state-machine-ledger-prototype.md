# State Machine Ledger prototype plan

## Prototype objective

Build the second Spectra core as an append-only context interceptor that replaces replayed conversational history with a deterministic, bounded state projection. It records agent/tool activity and system outcomes; it does not summarize arbitrary chat or duplicate terminal/source bodies.

The ledger will reference topology artifacts directly. An edit decision can retain `map_id`, visual node ID, and exact source anchor without copying the map metadata or source into every subsequent turn.

## Modules

### `ledger::event`

Define the immutable domain-neutral primitives:

- `SequenceId` and `EventId`
- `EventKind`: observation, authorization, tool invocation, file mutation, verification, checkpoint, and failure
- typed scalar attributes using the existing atom/interner approach
- optional topology reference: map ID, visual node ID, and source span
- payload references for large terminal output rather than embedded output bodies

Events receive monotonic sequence numbers. Previously appended records are never rewritten.

### `ledger::machine`

Implement a strict transition reducer with explicit states:

- `Idle`
- `Observing`
- `AwaitingAuthorization`
- `Editing`
- `Verifying`
- `Blocked`
- `Complete`

The reducer accepts an event and returns the next state or a typed invalid-transition error. State is derived by replay, never stored as mutable truth.

### `ledger::store`

Persist versioned JSON Lines under `.spectra/ledger-v1.jsonl` with crash-safe append semantics. Large outputs go into separate content-addressed blobs with byte count, media type, digest, and a small redacted diagnostic excerpt in the event.

Before selecting a digest crate, verify its current maintenance and release status as required by Spectra's dependency policy. No new dependency is approved by this plan.

### `ledger::redact`

Apply deterministic secret and noise suppression before persistence:

- redact recognized credentials and environment assignments
- collapse repeated terminal lines
- classify command outcomes such as pass, fail, timeout, cancellation, and interrupted
- preserve exit code, duration, byte count, and blob reference
- never persist `.env` values

### `ledger::project`

Produce the LLM-facing compact projection. Default budget: 150 estimated text tokens, hard cap: 250. A projection contains:

- current state and sequence
- active objective/checkpoint
- most recent authorized mutation
- changed paths and topology anchors
- latest verification outcome
- unresolved blocker, if any

Example shape:

```text
S42 VERIFYING goal=selector-v7
edit src/select.rs@N4 outcome=applied
check cargo-test pass 22/22
check grok-bench pass input=-89.7% quality=98.3%
next blind-review
```

### `ledger::intercept`

The ledger is lazily created by normal Spectra operations. `spectra map` and `spectra_map` incrementally refresh the repository immediately before selection, append synchronization and topology events, and require no `init`, `record`, daemon, or `sync` command. This just-in-time contract guarantees that a context response cannot use a stale index.

`spectra install` registers both the stdio MCP server and a Codex lifecycle hook adapter. The adapter consumes Codex's documented JSON hook input and never reads the unstable transcript format. It observes:

- `SessionStart` and `UserPromptSubmit` to inject the current bounded projection
- `PermissionRequest` to record authorization state without making approval decisions
- `PostToolUse` for `apply_patch` paths and recognized verification commands
- `Stop` to close pending edit or verification state

Hook failures are fail-open and produce no output. Patch bodies, prompts, assistant messages, and terminal output bodies are never persisted. Tool retries use stable correlation IDs, and a short cross-process lock serializes hook writers before replay and append.

Codex requires a one-time trust review for non-managed hooks. The installer cannot safely or legitimately bypass that product security boundary; after installation, the user reviews the Spectra hook in `/hooks`. Raw event-history exposure remains prohibited; agents receive only the bounded projection.

## First implementation slice

1. Event schema and state reducer in the reusable core library.
2. Lazy append/replay store with invalid-transition tests.
3. JSONL persistence with crash/truncated-tail recovery.
4. Automatic repository/map synchronization through normal map calls.
5. Redacted command outcome classification.
6. Bounded compact projection.
7. Ownership-aware Codex installer and automatic lifecycle interception; no manual mutation surface.

## Acceptance criteria

- deterministic replay yields the same final state and projection
- invalid transitions are rejected without appending
- prior events cannot be mutated through the public API
- a truncated final JSONL record is detected and recoverable
- secrets from fixture environment/output never appear in the ledger or projection
- terminal output bodies remain outside the event stream
- default projection remains below 150 estimated text tokens
- topology references round-trip without copying source bodies
- end-to-end flow covers authorize → edit → test failure → repair → tests pass → complete
- editing a source file followed by an ordinary map call records the incremental sync without explicit initialization or synchronization
- duplicate hook delivery does not duplicate events
- concurrent hook writers replay with contiguous sequence numbers and no lost records
- install and uninstall preserve unrelated user hooks

## Maintenance boundaries and explicit deferrals

- Codex documents that richer unified shell execution is not completely intercepted by hooks yet. Spectra records supported `Bash`, `apply_patch`, and lifecycle events and does not claim full OS process interception.
- Additional agent adapters must translate into the existing Ledger events; provider-specific wire shapes stay outside `spectra-core`.
- The optional `correlation_id` is backward-compatible with existing v1 JSONL records and provides adapter retry idempotency without a mutable side database.
- Hook configuration is merged and removed by ownership marker; unrelated hooks are preserved.
- transparent OS-wide shell/process injection
- conversational natural-language summarization
- distributed or multi-user ledgers
- encryption/key management
- Tauri observability UI
- public extension SDK
- automatic installation into agents other than Codex
