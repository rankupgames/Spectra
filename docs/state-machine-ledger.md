# State Machine Ledger

The State Machine Ledger is Spectra's append-only context interceptor. It replaces replayed conversational and terminal history with a deterministic, bounded projection of what changed, what was verified, and what remains blocked. It does not summarize arbitrary chat or persist source, patch, prompt, assistant-message, or terminal-output bodies.

The Ledger is implemented in `spectra-core`; the public harness-neutral JSON boundary and private provider translations stay in the CLI layer.

## Event and state contract

Ledger events are immutable, versioned JSON Lines records stored at `.spectra/ledger-v1.jsonl`. Every accepted event receives a monotonic project sequence number. State is always derived by replay rather than stored as mutable truth. New records may include bounded `harness` and `session_id` source metadata; source-less v1 records remain valid and replay in a compatibility lane.

The reducer uses these explicit states:

- `Idle`
- `Observing`
- `AwaitingAuthorization`
- `Editing`
- `Verifying`
- `Blocked`
- `Complete`

Events cover repository observation, authorization, observed or authorized file mutation, verification, map generation, checkpoints, completion, and failure. `EditObserved` represents hosts that expose a mutation without an authorization event; Spectra never fabricates an approval. Optional topology references retain a map ID, visual node ID, and source anchor without copying source or map metadata into later turns.

Invalid transitions are rejected before append within the requesting harness session. Concurrent sessions reduce independently, so one agent cannot put another agent into an invalid transition. Repository synchronization and topology events remain shared. A truncated final JSONL record is recoverable, correlation IDs make hook retries idempotent, and a short cross-process lock serializes replay-and-append transactions.

## Redaction and projection

Spectra classifies command outcomes and preserves only compact facts such as command class, success, exit code, duration, changed paths, and a redacted diagnostic. Credential-shaped values and environment assignments are removed before persistence.

The model-facing projection contains the requesting session's state plus the latest project-wide mutation, verification, map, and blocker facts. The default projection budget is 150 estimated text tokens with a hard cap of 250.

Example:

```text
S42 VERIFYING goal=selector-v7
edit src/select.rs@N4 outcome=applied
check cargo-test pass 22/22
next blind-review
```

Raw event-history exposure is intentionally not part of the agent interface.

## Automatic operation

The Ledger is lazily created through normal Spectra use. `spectra map` and the `spectra_map` MCP tool refresh the repository immediately before selection, then append synchronization and map events. Users do not need an initialization command, daemon, or manual synchronization step.

Verified provider adapters translate documented lifecycle events into the same Ledger facts:

- Codex preserves its `SessionStart`, `UserPromptSubmit`, permission, post-tool, and stop behavior.
- Claude Code covers `SessionStart`, `UserPromptSubmit`, permission/post-tool events, and `Stop`.
- Gemini CLI covers `SessionStart`, `BeforeAgent`, before/after-tool events, and `AfterAgent`.
- Cursor records edit, shell/tool, verification, and completion events, but reinjects only at `sessionStart` and is labeled partial.

Hook failures are fail-open. Provider-specific stdout remains valid even for unsupported or malformed payloads. Codex also requires a one-time trust review for non-managed hooks; Spectra does not bypass that security boundary.

Additional harnesses should use the stable [`spectra lifecycle ingest` JSON v1 protocol](lifecycle-protocol.md). Provider-specific configuration and wire formats do not belong in `spectra-core`; Rust adapter traits remain private in v0.3.

## Verified invariants

The regression suite covers:

- deterministic replay and projection
- invalid-transition rejection
- immutable prior events
- truncated-tail recovery
- secret redaction and exclusion of terminal-output bodies
- bounded projections below the default token budget
- topology-reference round trips without source bodies
- authorize → edit → failed verification → repair → pass → complete
- incremental synchronization during ordinary map calls
- duplicate hook delivery
- mixed legacy and source-aware replay
- independent concurrent harness sessions with shared project facts
- concurrent writers with contiguous sequence numbers
- ownership-aware install and uninstall behavior

Recorded Codex, Claude Code, and Gemini CLI backtests additionally require equivalent edit → failed verification → repair → pass → complete facts, no duplicate immutable events after retry, secret exclusion, bounded projections, and provider-valid stdout. Cursor is tested separately for ingestion and session-start-only reinjection.

## Deliberate boundaries

The Ledger does not attempt transparent OS-wide process interception, conversational summarization, distributed or multi-user state, encryption or key management, a Tauri UI, or a public extension SDK.

Provider lifecycle hooks do not expose every unified-shell operation. Spectra records only supported events and does not claim complete terminal interception. Cursor's lack of reliable per-prompt injection must remain explicit until the host contract changes.
