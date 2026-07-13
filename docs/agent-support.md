# Agent support contract

Spectra's initial compatibility target is informed by CodeGraph's public eight-agent support list. That list is useful evidence of which local agents people actually expect graph tooling to work with. Agent-specific configuration belongs behind adapters; the topology engine and MCP server remain agent-neutral.

## Target matrix

| Target | Detection/config surface | MCP configuration | Ledger capability |
| --- | --- | --- | --- |
| Claude Code | `~/.claude.json` | `mcpServers.spectra` | Planned: documented lifecycle hooks |
| Cursor | `~/.cursor/mcp.json` | `mcpServers.spectra`, with `${workspaceFolder}` path injection | Topology until a stable lifecycle surface is verified |
| Codex | `codex mcp` plus `~/.codex/hooks.json` | CLI-managed stdio server | Implemented and backtested |
| OpenCode | `$XDG_CONFIG_HOME/opencode/opencode.jsonc` | `mcp.spectra`, local command array | Topology until a stable lifecycle surface is verified |
| Hermes Agent | `$HERMES_HOME/config.yaml` | `mcp_servers.spectra` plus `mcp-spectra` CLI toolset | Topology until a stable lifecycle surface is verified |
| Gemini CLI | `~/.gemini/settings.json` | `mcpServers.spectra` | Topology until official hook coverage is verified |
| Antigravity | unified or legacy Gemini MCP config | `mcpServers.spectra`, no `type` field | Topology until a stable lifecycle surface is verified |
| Kiro | `~/.kiro/settings/mcp.json` | `mcpServers.spectra` | Topology until a stable lifecycle surface is verified |

## Installer contract

`spectra install` will default to `--target auto`, detect every installed target, and configure all detections in one run. It will also accept `--target all`, `--target none`, or a comma-separated target list.

Every adapter must provide:

- stable target ID and display name
- global installation detection
- exact configuration paths and schema
- current/stale/foreign ownership classification
- idempotent install, status, dry-run, and uninstall
- atomic writes that preserve unrelated settings
- restart, trust, or enablement notes specific to the agent
- a declared capability level: `topology` or `topology+ledger`

An adapter may claim `topology+ledger` only when its lifecycle integration is based on a documented, versioned surface and passes a recorded-wire replay. MCP-only agents remain fully useful for visual topology but must be reported honestly as `topology`.

## Implementation sequence

1. Extract the existing Codex logic behind the common adapter interface.
2. Add the standard JSON family: Claude Code, Cursor, Gemini CLI, Antigravity, and Kiro.
3. Add comment-preserving OpenCode JSONC support.
4. Add surgical Hermes YAML and toolset support.
5. Add automatic detection and multi-target transaction reporting.
6. Backtest every adapter against created, existing, stale, conflicting, malformed, and uninstall fixtures.
7. Add Ledger adapters only after verifying each agent's official lifecycle contract.

## Compatibility references

- [CodeGraph supported-agent registry](https://github.com/colbymchenry/codegraph/tree/main/src/installer/targets)
- [CodeGraph installation behavior](https://github.com/colbymchenry/codegraph#2-wire-up-your-agents)

These references document ecosystem expectations; Spectra does not copy CodeGraph's installer implementation. Its adapters are Rust-native, independently maintained, and tested against each agent's own configuration contract.
