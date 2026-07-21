use std::{fs, path::Path};

use super::{
    BoxError, ConfigSnapshot,
    hooks::{self, HookSchema, HookTarget},
    spectra_executable, write_atomic,
};

const START: &str = "# >>> spectra managed MCP >>>";
const END: &str = "# <<< spectra managed MCP <<<";
const CODEX_EVENTS: &[(&str, Option<&str>)] = &[
    ("SessionStart", Some("startup|resume|clear|compact")),
    ("UserPromptSubmit", None),
    ("PermissionRequest", Some("Bash|apply_patch|Edit|Write")),
    ("PostToolUse", Some("Bash|apply_patch|Edit|Write")),
    ("Stop", None),
];

pub(super) fn install_codex(
    project: &Path,
    dry_run: bool,
    topology_only: bool,
) -> Result<String, BoxError> {
    install_codex_at(
        project.join(".codex/config.toml"),
        project.join(".codex/hooks.json"),
        dry_run,
        topology_only,
        "project",
    )
}

pub(super) fn install_codex_global(dry_run: bool, topology_only: bool) -> Result<String, BoxError> {
    let root = codex_home()?;
    install_codex_at(
        root.join("config.toml"),
        root.join("hooks.json"),
        dry_run,
        topology_only,
        "user",
    )
}

fn install_codex_at(
    config: std::path::PathBuf,
    hooks_path: std::path::PathBuf,
    dry_run: bool,
    topology_only: bool,
    scope: &str,
) -> Result<String, BoxError> {
    let executable = spectra_executable()?;
    let current = if config.exists() {
        fs::read_to_string(&config)?
    } else {
        String::new()
    };
    let owns_unmarked = owned_unmarked_block(&current);
    if current.contains("[mcp_servers.spectra]") && !current.contains(START) && !owns_unmarked {
        return Err(format!(
            "Codex already has a non-Spectra-owned {scope} MCP entry named 'spectra' in {}",
            config.display()
        )
        .into());
    }
    let executable = serde_json::to_string(
        executable
            .to_str()
            .ok_or("Spectra executable path is not valid UTF-8")?,
    )?;
    let block = format!(
        "{START}\n[mcp_servers.spectra]\ncommand = {executable}\nargs = [\"serve\", \"--mcp\"]\n{END}"
    );
    let hook_target = (!topology_only).then_some(HookTarget {
        label: "Codex",
        agent: "codex",
        path: hooks_path,
        schema: HookSchema::Grouped,
        events: CODEX_EVENTS,
    });
    if let Some(target) = &hook_target {
        hooks::install(target, true)?;
    }
    let base = if owns_unmarked {
        remove_unmarked_block(&current)
    } else {
        current.clone()
    };
    let updated = replace_block(&base, &block);
    let snapshot = (!dry_run)
        .then(|| ConfigSnapshot::capture(&config))
        .transpose()?;
    if !dry_run && updated != current {
        write_atomic(&config, updated.as_bytes())?;
    }
    let mut message = if dry_run {
        format!(
            "Codex: Would configure {scope} MCP in {}.",
            config.display()
        )
    } else {
        format!("Codex: {scope} MCP configured in {}.", config.display())
    };
    if let Some(target) = &hook_target {
        message.push('\n');
        match hooks::install(target, dry_run) {
            Ok(hook_message) => message.push_str(&hook_message),
            Err(error) => {
                if let Some(snapshot) = snapshot {
                    snapshot.restore().map_err(|rollback| {
                        format!("{error}; additionally failed to roll back Codex {scope} configuration: {rollback}")
                    })?;
                }
                return Err(error);
            }
        }
    }
    Ok(message)
}

pub(super) fn uninstall_codex(project: &Path, dry_run: bool) -> Result<String, BoxError> {
    uninstall_codex_at(
        project.join(".codex/config.toml"),
        project.join(".codex/hooks.json"),
        dry_run,
        "project",
    )
}

pub(super) fn uninstall_codex_global(dry_run: bool) -> Result<String, BoxError> {
    let root = codex_home()?;
    uninstall_codex_at(
        root.join("config.toml"),
        root.join("hooks.json"),
        dry_run,
        "user",
    )
}

fn uninstall_codex_at(
    config: std::path::PathBuf,
    hooks_path: std::path::PathBuf,
    dry_run: bool,
    scope: &str,
) -> Result<String, BoxError> {
    let current = if config.exists() {
        fs::read_to_string(&config)?
    } else {
        String::new()
    };
    let owns_unmarked = owned_unmarked_block(&current);
    if current.contains("[mcp_servers.spectra]") && !current.contains(START) && !owns_unmarked {
        return Err(format!(
            "Codex {scope} MCP entry named 'spectra' in {} is not owned by Spectra",
            config.display()
        )
        .into());
    }
    let target = HookTarget {
        label: "Codex",
        agent: "codex",
        path: hooks_path,
        schema: HookSchema::Grouped,
        events: CODEX_EVENTS,
    };
    let updated = if owns_unmarked {
        remove_unmarked_block(&current)
    } else {
        remove_block(&current)
    };
    if dry_run {
        return Ok(format!(
            "Codex: Would remove {scope} MCP and owned Ledger hooks from {}.",
            config.display()
        ));
    }
    let snapshot = ConfigSnapshot::capture(&config)?;
    if updated != current {
        write_atomic(&config, updated.as_bytes())?;
    }
    let hook_message = match hooks::uninstall(&target, false) {
        Ok(message) => message,
        Err(error) => {
            snapshot.restore().map_err(|rollback| {
                format!(
                    "{error}; additionally failed to roll back Codex {scope} configuration: {rollback}"
                )
            })?;
            return Err(error);
        }
    };
    Ok(format!(
        "Codex: Removed {scope} MCP configuration.\n{hook_message}"
    ))
}

pub(super) fn codex_global_status() -> Result<String, BoxError> {
    let root = codex_home()?;
    let config = root.join("config.toml");
    let current = if config.exists() {
        fs::read_to_string(&config)?
    } else {
        String::new()
    };
    let executable = spectra_executable()?;
    let command = serde_json::to_string(
        executable
            .to_str()
            .ok_or("Spectra executable path is not valid UTF-8")?,
    )?;
    let expected = format!("command = {command}\nargs = [\"serve\", \"--mcp\"]");
    let mcp = if (current.contains(START) && current.contains(&expected))
        || unmarked_command(&current).is_some_and(|configured| {
            let configured = std::path::PathBuf::from(configured);
            configured.canonicalize().unwrap_or(configured) == executable
        }) {
        "current"
    } else if current.contains(START) || owned_unmarked_block(&current) {
        "stale"
    } else if current.contains("[mcp_servers.spectra]") {
        "foreign conflict"
    } else {
        "missing"
    };
    let hook_target = HookTarget {
        label: "Codex",
        agent: "codex",
        path: root.join("hooks.json"),
        schema: HookSchema::Grouped,
        events: CODEX_EVENTS,
    };
    let hooks = hooks::status(&hook_target)?;
    Ok(format!(
        "Codex CLI/Desktop: MCP={mcp}, Ledger hooks={hooks}, Capability=topology+ledger"
    ))
}

fn codex_home() -> Result<std::path::PathBuf, BoxError> {
    if let Some(path) =
        std::env::var_os("SPECTRA_CODEX_HOME").or_else(|| std::env::var_os("CODEX_HOME"))
    {
        return Ok(path.into());
    }
    std::env::var_os("SPECTRA_HOME")
        .or_else(|| std::env::var_os("HOME"))
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .map(|path| path.join(".codex"))
        .ok_or_else(|| "unable to locate the Codex home directory".into())
}

fn replace_block(current: &str, block: &str) -> String {
    let without = remove_block(current);
    let trimmed = without.trim_end();
    if trimmed.is_empty() {
        format!("{block}\n")
    } else {
        format!("{trimmed}\n\n{block}\n")
    }
}

fn remove_block(current: &str) -> String {
    if let Some(start) = current.find(START) {
        let end = current[start..]
            .find(END)
            .map(|end| start + end + END.len())
            .unwrap_or(current.len());
        format!("{}{}", &current[..start], &current[end..])
    } else {
        current.to_owned()
    }
}

fn unmarked_block_range(current: &str) -> Option<std::ops::Range<usize>> {
    let start = current.find("[mcp_servers.spectra]")?;
    let remainder = &current[start + "[mcp_servers.spectra]".len()..];
    let end = remainder.find("\n[").map_or(current.len(), |offset| {
        start + "[mcp_servers.spectra]".len() + offset + 1
    });
    Some(start..end)
}

fn unmarked_command(current: &str) -> Option<String> {
    let range = unmarked_block_range(current)?;
    let block = &current[range];
    let compact: String = block
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    if !compact.contains("args=[\"serve\",\"--mcp\"]") {
        return None;
    }
    block.lines().find_map(|line| {
        let value = line.trim().strip_prefix("command")?.trim_start();
        let value = value.strip_prefix('=')?.trim();
        serde_json::from_str(value).ok()
    })
}

fn owned_unmarked_block(current: &str) -> bool {
    unmarked_command(current).is_some_and(|command| {
        std::path::Path::new(&command)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| stem.eq_ignore_ascii_case("spectra"))
    })
}

fn remove_unmarked_block(current: &str) -> String {
    let Some(range) = unmarked_block_range(current) else {
        return current.to_owned();
    };
    format!("{}{}", &current[..range.start], &current[range.end..])
}
