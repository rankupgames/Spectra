# Agent support contract

Spectra keeps agent-specific setup behind small adapters. The topology engine and MCP server do not depend on any one agent, so adding a client never forks the graph or indexing logic.

## Supported agents

| Agent | Global configuration | MCP schema | Capability |
| --- | --- | --- | --- |
| Claude Code | `~/.claude.json` | `mcpServers.spectra` | Topology |
| Cursor | `~/.cursor/mcp.json` | `mcpServers.spectra` | Topology |
| Codex | `codex mcp` and `~/.codex/hooks.json` | CLI-managed stdio server | Topology + Ledger |
| OpenCode | `$XDG_CONFIG_HOME/opencode/opencode.json` | `mcp.spectra`, local command array | Topology |
| Hermes Agent | `$HERMES_HOME/config.yaml` | `mcp_servers.spectra` | Topology |
| Gemini CLI | `~/.gemini/settings.json` | `mcpServers.spectra` | Topology |
| Antigravity | `~/.gemini/config/mcp_config.json` | `mcpServers.spectra` | Topology |
| Kiro | `~/.kiro/settings/mcp.json` | `mcpServers.spectra` | Topology |

Run `spectra install` with no flags to detect and configure every supported agent already present on the machine. Use `--agent <name>` for one agent or `--agent all` to attempt every adapter. A failure in one target is reported without preventing the other selected targets from being processed.

Every adapter provides:

- installation and configuration-file detection
- current, stale, foreign, and missing ownership states
- idempotent install, status, dry-run, and uninstall behavior
- atomic writes that preserve unrelated settings
- comment-preserving JSONC edits for OpenCode
- surgical YAML edits for Hermes Agent
- an explicit `topology` or `topology+ledger` capability level

Spectra never overwrites or removes an entry named `spectra` unless its command shape clearly belongs to Spectra. Malformed configuration is reported and left byte-for-byte unchanged.

## Ledger policy

An adapter may report `topology+ledger` only when its lifecycle integration uses a documented surface and passes a recorded-wire replay. MCP-only agents still receive the complete visual topology workflow; they simply do not receive claims about lifecycle events Spectra cannot reliably observe.

Codex is currently the only `topology+ledger` adapter. Its recorded session replay covers authorization, editing, verification, session reinjection, duplicate delivery, and malformed input. Other agents remain topology-only until they meet the same bar.

## Configuration references

- [Claude Code MCP](https://code.claude.com/docs/en/mcp)
- [Cursor MCP](https://docs.cursor.com/context/model-context-protocol)
- [OpenCode configuration](https://dev.opencode.ai/docs/config)
- [OpenCode MCP servers](https://thdxr.dev.opencode.ai/docs/mcp-servers/)
- [Hermes Agent MCP](https://github.com/NousResearch/hermes-agent/blob/main/website/docs/user-guide/features/mcp.md)
- [Gemini CLI MCP](https://github.com/google-gemini/gemini-cli/blob/main/docs/tools/mcp-server.md)
- [Antigravity MCP](https://antigravity.google/docs/mcp)
- [Kiro MCP configuration](https://kiro.dev/docs/cli/mcp/configuration/)
