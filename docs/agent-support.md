# Agent support contract

Spectra keeps agent-specific setup behind small adapters. The topology engine and MCP server do not depend on any one agent, so adding a client never forks the graph or indexing logic.

## Supported agents

| Agent | Global configuration | MCP schema | Capability |
| --- | --- | --- | --- |
| Claude Code | `~/.claude.json` and `~/.claude/settings.json` | `mcpServers.spectra` plus lifecycle hooks | `topology+ledger` |
| Cursor | `~/.cursor/mcp.json` and `~/.cursor/hooks.json` | `mcpServers.spectra` plus lifecycle hooks | `topology+ledger-partial` |
| Codex | `codex mcp` and `~/.codex/hooks.json` | CLI-managed stdio server plus lifecycle hooks | `topology+ledger` |
| OpenCode | `$XDG_CONFIG_HOME/opencode/opencode.json` | `mcp.spectra`, local command array | Topology |
| Hermes Agent | `$HERMES_HOME/config.yaml` | `mcp_servers.spectra` | Topology |
| Gemini CLI | `~/.gemini/settings.json` | `mcpServers.spectra` plus lifecycle hooks | `topology+ledger` |
| Antigravity | `~/.gemini/config/mcp_config.json` | `mcpServers.spectra` | Topology |
| Kiro | `~/.kiro/settings/mcp.json` | `mcpServers.spectra` | Topology |

Run `spectra install` in a TTY for a guided scan, target/location choice, conflict preflight, confirmation, and final capability report. Use `--agent <name>` for one agent, `--agent all` to attempt every adapter, or `--yes` for unattended detected-target installation with global defaults. Non-interactive input requires an explicit target or `--yes` and never waits for prompts. A failure in one target is reported without preventing the other selected targets from being processed.

Codex, Claude Code, Gemini CLI, and Cursor also support `--location local --path <repo>` through their documented project configuration layers. Codex uses `.codex/config.toml` and `.codex/hooks.json`; Claude uses `.mcp.json` and `.claude/settings.json`; Gemini uses `.gemini/settings.json`; Cursor uses `.cursor/mcp.json` and `.cursor/hooks.json`. Unsupported local combinations are rejected before mutation.

Every adapter provides:

- installation and configuration-file detection
- current, stale, foreign, and missing ownership states
- idempotent install, status, dry-run, and uninstall behavior
- atomic writes that preserve unrelated settings
- comment-preserving JSONC edits for OpenCode
- surgical YAML edits for Hermes Agent
- an explicit `topology`, `topology+ledger`, or `topology+ledger-partial` capability level

Spectra never overwrites or removes an entry named `spectra` unless its command shape clearly belongs to Spectra. Malformed configuration is reported and left byte-for-byte unchanged.

## Ledger policy

An adapter may report `topology+ledger` only when its lifecycle integration uses a documented surface and passes a recorded-wire replay. MCP-only agents still receive the complete visual topology workflow; they simply do not receive claims about lifecycle events Spectra cannot reliably observe.

Codex, Claude Code, and Gemini CLI are verified `topology+ledger` adapters. Their recorded sessions cover context requests, authorization where the host exposes it, editing, failed verification, repair, successful verification, completion, duplicate delivery, redaction, and bounded provider-valid output.

Cursor is deliberately `topology+ledger-partial`. It records file edits, shell/tool results, verification, and completion, and reinjects the bounded projection at `sessionStart`. Cursor does not currently expose reliable per-prompt context injection, so Spectra does not claim per-prompt continuity there.

Any other harness can integrate through the stable [`spectra lifecycle ingest` JSON v1 protocol](lifecycle-protocol.md) without importing Spectra internals.

## Configuration references

- [Claude Code MCP](https://code.claude.com/docs/en/mcp)
- [Claude Code hooks](https://code.claude.com/docs/en/hooks)
- [Cursor MCP](https://docs.cursor.com/context/model-context-protocol)
- [Cursor hooks](https://cursor.com/docs/hooks)
- [OpenCode configuration](https://dev.opencode.ai/docs/config)
- [OpenCode MCP servers](https://thdxr.dev.opencode.ai/docs/mcp-servers/)
- [Hermes Agent MCP](https://github.com/NousResearch/hermes-agent/blob/main/website/docs/user-guide/features/mcp.md)
- [Gemini CLI MCP](https://github.com/google-gemini/gemini-cli/blob/main/docs/tools/mcp-server.md)
- [Gemini CLI hooks](https://geminicli.com/docs/hooks/reference/)
- [Antigravity MCP](https://antigravity.google/docs/mcp)
- [Kiro MCP configuration](https://kiro.dev/docs/cli/mcp/configuration/)
