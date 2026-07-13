# State Machine Ledger

The State Machine Ledger is Spectra's append-only context interceptor. It replaces replayed conversational and terminal history with a deterministic, bounded projection of what changed, what was verified, and what remains blocked. It does not summarize arbitrary chat or persist source, patch, prompt, assistant-message, or terminal-output bodies.

The Ledger is implemented in `spectra-core`; agent-specific lifecycle translation stays in the CLI layer.

## Event and state contract

Ledger events are immutable, versioned JSON Lines records stored at `.spectra/ledger-v1.jsonl`. Every accepted event receives a monotonic sequence number. State is always derived by replay rather than stored as mutable truth.

The reducer uses these explicit states:

- `Idle`
- `Observing`
- `AwaitingAuthorization`
- `Editing`
- `Verifying`
- `Blocked`
- `Complete`

Events cover repository observation, authorization, file mutation, verification, map generation, checkpoints, completion, and failure. Optional topology references retain a map ID, visual node ID, and source anchor without copying source or map metadata into later turns.

Invalid transitions are rejected before append. A truncated final JSONL record is recoverable, correlation IDs make hook retries idempotent, and a short cross-process lock serializes replay-and-append transactions.

## Redaction and projection

Spectra classifies command outcomes and preserves only compact facts such as command class, success, exit code, duration, changed paths, and a redacted diagnostic. Credential-shaped values and environment assignments are removed before persistence.

The model-facing projection contains only the current state, recent mutation, latest verification result, relevant topology anchors, and any unresolved blocker. The default projection budget is 150 estimated text tokens with a hard cap of 250.

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

The current Codex adapter translates supported lifecycle events:

- `SessionStart` and `UserPromptSubmit` inject the bounded projection.
- `PermissionRequest` records authorization state without making approval decisions.
- `PostToolUse` records `apply_patch` paths and recognized verification commands.
- `Stop` closes pending edit or verification state.

Hook failures are fail-open. Codex also requires a one-time trust review for non-managed hooks; Spectra does not bypass that security boundary.

Additional agent adapters must translate their lifecycle contracts into the same core events. Provider-specific configuration and wire formats do not belong in `spectra-core`.

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
- concurrent writers with contiguous sequence numbers
- ownership-aware install and uninstall behavior

The recorded Codex hook backtest additionally requires exact edited-path and verification fact retention, no duplicate immutable events after retry, and bounded reinjected context.

## Deliberate boundaries

The Ledger does not attempt transparent OS-wide process interception, conversational summarization, distributed or multi-user state, encryption or key management, a Tauri UI, or a public extension SDK.

Codex's lifecycle hooks do not currently expose every unified-shell operation. Spectra records only supported events and does not claim complete terminal interception. That limitation must remain explicit until the host contract changes.
