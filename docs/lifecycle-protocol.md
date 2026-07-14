# Lifecycle JSON v1 protocol

`spectra lifecycle ingest` is the stable compatibility boundary for any coding harness. It reads exactly one JSON envelope from standard input, writes exactly one JSON result to standard output, and exits nonzero for invalid canonical input. Integrators do not import Rust traits or copy a provider adapter.

```json
{
  "version": 1,
  "source": {
    "harness": "custom",
    "session_id": "opaque-session",
    "event_id": "stable-event-id"
  },
  "cwd": "/absolute/project/path",
  "event": {
    "type": "edit_observed",
    "paths": ["src/lib.rs"]
  }
}
```

`source.event_id` must be stable across provider retries. Ledger reduction is isolated by `harness` and `session_id`; repository synchronization, maps, mutations, verification results, and blockers remain available as the latest shared project facts. Existing source-less `ledger-v1.jsonl` records replay in a compatibility lane.

## Event types

| Type | Fields | Meaning |
| --- | --- | --- |
| `context_requested` | none | Request a bounded projection for this session. |
| `authorization_requested` | `action` | Record a pending permission decision. |
| `authorization_result` | `allowed`, `action` | Resolve the pending permission decision. |
| `edit_observed` | `paths` | Record a host-observed edit without inventing approval. |
| `verification_observed` | `command`, `success`, optional `exit_code`, `output_bytes` | Retain a bounded verification fact without output contents. |
| `turn_finished` | `outcome` (`completed` or `blocked`), `summary` | Close or block the current session lane. |
| `blocked` | `reason` | Record an explicit blocker. |

Unknown event types and unsupported versions are rejected. Unknown envelope and event fields are ignored for compatible extension. Input is capped at 1 MiB; identifiers, summaries, blocker reasons, commands, and path arrays are bounded before normalization. Raw prompts, assistant responses, patches, terminal output, and provider payloads are never persisted.

## Result

Every valid request returns JSON shaped as follows:

```json
{
  "version": 1,
  "accepted": true,
  "duplicate": false,
  "sequence": 12,
  "state": "editing",
  "context": {
    "text": "S12 EDITING\nedit src/lib.rs outcome=observed",
    "estimated_tokens": 13
  }
}
```

`context` is present for `context_requested` and stays within the Ledger projection budget. A duplicate retry returns `accepted: true`, `duplicate: true`, and the existing sequence without appending another immutable record.

Example:

```sh
printf '%s\n' '{"version":1,"source":{"harness":"custom","session_id":"s1","event_id":"e1"},"cwd":"/repo","event":{"type":"context_requested"}}' \
  | spectra lifecycle ingest
```

Installed provider hooks use `spectra hook --agent <agent>` and deliberately fail open so a Spectra error cannot block the host agent. Bare `spectra hook` remains a Codex compatibility alias. Canonical protocol callers should treat a nonzero exit as rejected input and decide their own fail-open policy.
