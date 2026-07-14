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
    let config = project.join(".codex/config.toml");
    let hooks_path = project.join(".codex/hooks.json");
    let executable = spectra_executable()?;
    let current = if config.exists() {
        fs::read_to_string(&config)?
    } else {
        String::new()
    };
    if current.contains("[mcp_servers.spectra]") && !current.contains(START) {
        return Err(format!(
            "Codex already has a non-Spectra-owned project MCP entry named 'spectra' in {}",
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
    let updated = replace_block(&current, &block);
    let snapshot = (!dry_run)
        .then(|| ConfigSnapshot::capture(&config))
        .transpose()?;
    if !dry_run && updated != current {
        write_atomic(&config, updated.as_bytes())?;
    }
    let mut message = if dry_run {
        format!(
            "Codex: Would configure project MCP in {}.",
            config.display()
        )
    } else {
        format!("Codex: Project MCP configured in {}.", config.display())
    };
    if let Some(target) = &hook_target {
        message.push('\n');
        match hooks::install(target, dry_run) {
            Ok(hook_message) => message.push_str(&hook_message),
            Err(error) => {
                if let Some(snapshot) = snapshot {
                    snapshot.restore().map_err(|rollback| {
                        format!("{error}; additionally failed to roll back Codex project configuration: {rollback}")
                    })?;
                }
                return Err(error);
            }
        }
    }
    Ok(message)
}

pub(super) fn uninstall_codex(project: &Path, dry_run: bool) -> Result<String, BoxError> {
    let config = project.join(".codex/config.toml");
    let hooks_path = project.join(".codex/hooks.json");
    let current = if config.exists() {
        fs::read_to_string(&config)?
    } else {
        String::new()
    };
    if current.contains("[mcp_servers.spectra]") && !current.contains(START) {
        return Err(format!(
            "Codex project MCP entry named 'spectra' in {} is not owned by Spectra",
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
    let updated = remove_block(&current);
    if dry_run {
        return Ok(format!(
            "Codex: Would remove project MCP and owned Ledger hooks from {}.",
            project.display()
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
                    "{error}; additionally failed to roll back Codex project configuration: {rollback}"
                )
            })?;
            return Err(error);
        }
    };
    Ok(format!(
        "Codex: Removed project MCP configuration.\n{hook_message}"
    ))
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
