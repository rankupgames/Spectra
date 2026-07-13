use std::{
    fmt, fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use atomic_write_file::AtomicWriteFile;
use serde_json::Value;

const SERVER_NAME: &str = "spectra";
const HOOK_STATUS: &str = "Spectra context ledger";
const HOOK_EVENTS: [(&str, Option<&str>); 5] = [
    ("SessionStart", Some("startup|resume|clear|compact")),
    ("UserPromptSubmit", None),
    ("PermissionRequest", Some("Bash|apply_patch|Edit|Write")),
    ("PostToolUse", Some("Bash|apply_patch|Edit|Write")),
    ("Stop", None),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Ownership {
    Current,
    Stale,
    Foreign,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstallOutcome {
    Installed,
    Updated,
    AlreadyInstalled,
    WouldInstall,
    WouldUpdate,
}

impl fmt::Display for InstallOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Installed => {
                "Codex configured for Spectra maps and Ledger hooks. Restart Codex, then review the Spectra hook once in /hooks."
            }
            Self::Updated => {
                "Codex Spectra MCP and Ledger hook configuration updated. Restart Codex, then review changed hooks in /hooks."
            }
            Self::AlreadyInstalled => {
                "Codex is already configured for this Spectra binary and its Ledger hooks."
            }
            Self::WouldInstall => {
                "Would configure Codex with the current Spectra binary and Ledger hooks."
            }
            Self::WouldUpdate => {
                "Would update the existing Spectra-owned Codex MCP or Ledger hook configuration."
            }
        };
        formatter.write_str(text)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UninstallOutcome {
    Removed,
    NotInstalled,
    WouldRemove,
}

impl fmt::Display for UninstallOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Removed => "Removed Spectra's Codex MCP and Ledger hook configuration.",
            Self::NotInstalled => "Spectra is not configured in Codex.",
            Self::WouldRemove => "Would remove Spectra's Codex MCP configuration.",
        })
    }
}

pub fn install_codex(dry_run: bool) -> Result<InstallOutcome, Box<dyn std::error::Error>> {
    let spectra = std::env::current_exe()?.canonicalize()?;
    let existing = get_codex_config()?;
    let ownership = existing
        .as_ref()
        .map(|config| ownership(config, &spectra))
        .transpose()?;
    if ownership == Some(Ownership::Foreign) {
        return Err("Codex already has a non-Spectra-owned MCP entry named 'spectra'; refusing to overwrite it".into());
    }
    let hooks_current = hooks_are_current(&spectra)?;
    let mcp_current = ownership == Some(Ownership::Current);
    let had_hook_file = hooks_file_exists()?;
    if mcp_current && hooks_current {
        return Ok(InstallOutcome::AlreadyInstalled);
    }
    if dry_run {
        return Ok(if existing.is_some() || had_hook_file {
            InstallOutcome::WouldUpdate
        } else {
            InstallOutcome::WouldInstall
        });
    }
    match ownership {
        Some(Ownership::Stale) => {
            checked(
                codex_command()
                    .args(["mcp", "remove", SERVER_NAME])
                    .output()?,
                "codex mcp remove",
            )?;
            add_codex_config(&spectra)?;
        }
        None => add_codex_config(&spectra)?,
        Some(Ownership::Current) => {}
        Some(Ownership::Foreign) => unreachable!(),
    }
    install_hooks(&spectra)?;
    Ok(if existing.is_some() || had_hook_file {
        InstallOutcome::Updated
    } else {
        InstallOutcome::Installed
    })
}

pub fn uninstall_codex(dry_run: bool) -> Result<UninstallOutcome, Box<dyn std::error::Error>> {
    let config = get_codex_config()?;
    let spectra = std::env::current_exe()?.canonicalize()?;
    if let Some(config) = &config {
        if ownership(config, &spectra)? == Ownership::Foreign {
            return Err(
                "Codex's 'spectra' MCP entry is not owned by Spectra; refusing to remove it".into(),
            );
        }
    }
    let has_hooks = has_owned_hooks()?;
    if config.is_none() && !has_hooks {
        return Ok(UninstallOutcome::NotInstalled);
    }
    if dry_run {
        return Ok(UninstallOutcome::WouldRemove);
    }
    if config.is_some() {
        checked(
            codex_command()
                .args(["mcp", "remove", SERVER_NAME])
                .output()?,
            "codex mcp remove",
        )?;
    }
    uninstall_hooks()?;
    Ok(UninstallOutcome::Removed)
}

pub fn codex_status() -> Result<String, Box<dyn std::error::Error>> {
    let spectra = std::env::current_exe()?.canonicalize()?;
    let mcp = match get_codex_config()? {
        None => "missing",
        Some(config) => match ownership(&config, &spectra)? {
            Ownership::Current => "current",
            Ownership::Stale => "stale",
            Ownership::Foreign => "foreign conflict",
        },
    };
    let hooks = if hooks_are_current(&spectra)? {
        "current"
    } else if has_owned_hooks()? {
        "stale"
    } else {
        "missing"
    };
    Ok(format!("Codex: MCP={mcp}, Ledger hooks={hooks}"))
}

fn add_codex_config(spectra: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let executable = spectra
        .to_str()
        .ok_or("Spectra executable path is not valid UTF-8")?;
    checked(
        codex_command()
            .args([
                "mcp",
                "add",
                SERVER_NAME,
                "--",
                executable,
                "serve",
                "--mcp",
            ])
            .output()?,
        "codex mcp add",
    )?;
    Ok(())
}

fn hooks_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(path) =
        std::env::var_os("SPECTRA_CODEX_HOME").or_else(|| std::env::var_os("CODEX_HOME"))
    {
        return Ok(PathBuf::from(path).join("hooks.json"));
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or("unable to locate the Codex home directory")?;
    Ok(PathBuf::from(home).join(".codex/hooks.json"))
}

fn hooks_file_exists() -> Result<bool, Box<dyn std::error::Error>> {
    Ok(hooks_path()?.exists())
}

fn read_hooks() -> Result<Value, Box<dyn std::error::Error>> {
    let path = hooks_path()?;
    if !path.exists() {
        return Ok(serde_json::json!({"hooks": {}}));
    }
    let bytes = fs::read(&path)?;
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(serde_json::json!({"hooks": {}}));
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("{} is not valid JSON: {error}", path.display()))?;
    if !value.is_object() {
        return Err(format!("{} must contain a JSON object", path.display()).into());
    }
    Ok(value)
}

fn hook_command(spectra: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let path = spectra
        .to_str()
        .ok_or("Spectra executable path is not valid UTF-8")?;
    #[cfg(windows)]
    return Ok(format!("\"{}\" hook", path.replace('"', "\\\"")));
    #[cfg(not(windows))]
    return Ok(format!("'{}' hook", path.replace('\'', "'\\''")));
}

fn owned_handler(handler: &Value) -> bool {
    handler["type"] == "command"
        && handler["statusMessage"] == HOOK_STATUS
        && handler["command"]
            .as_str()
            .is_some_and(|command| command.ends_with(" hook"))
}

fn has_owned_hooks() -> Result<bool, Box<dyn std::error::Error>> {
    let hooks = read_hooks()?;
    Ok(hooks["hooks"]
        .as_object()
        .into_iter()
        .flat_map(|events| events.values())
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(|group| group["hooks"].as_array())
        .flatten()
        .any(owned_handler))
}

fn hooks_are_current(spectra: &Path) -> Result<bool, Box<dyn std::error::Error>> {
    let hooks = read_hooks()?;
    let command = hook_command(spectra)?;
    Ok(HOOK_EVENTS.iter().all(|(event, matcher)| {
        hooks["hooks"][event]
            .as_array()
            .into_iter()
            .flatten()
            .any(|group| {
                let matcher_matches = match matcher {
                    Some(expected) => group["matcher"] == *expected,
                    None => group.get("matcher").is_none(),
                };
                matcher_matches
                    && group["hooks"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|handler| owned_handler(handler) && handler["command"] == command)
            })
    }))
}

fn remove_owned_hooks(value: &mut Value) -> bool {
    let Some(events) = value.get_mut("hooks").and_then(Value::as_object_mut) else {
        return false;
    };
    let mut changed = false;
    for groups in events.values_mut().filter_map(Value::as_array_mut) {
        for group in groups.iter_mut() {
            if let Some(handlers) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                let before = handlers.len();
                handlers.retain(|handler| !owned_handler(handler));
                changed |= handlers.len() != before;
            }
        }
        groups.retain(|group| {
            group["hooks"]
                .as_array()
                .is_none_or(|hooks| !hooks.is_empty())
        });
    }
    events.retain(|_, groups| groups.as_array().is_none_or(|groups| !groups.is_empty()));
    changed
}

fn install_hooks(spectra: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut value = read_hooks()?;
    remove_owned_hooks(&mut value);
    let events = value
        .as_object_mut()
        .ok_or("hooks.json root must be an object")?
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or("hooks.json 'hooks' field must be an object")?;
    let command = hook_command(spectra)?;
    for (event, matcher) in HOOK_EVENTS {
        let groups = events
            .entry(event)
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .ok_or_else(|| format!("hooks.json event '{event}' must be an array"))?;
        let mut group = serde_json::json!({
            "hooks": [{
                "type": "command",
                "command": command,
                "timeout": 5,
                "statusMessage": HOOK_STATUS
            }]
        });
        if let Some(matcher) = matcher {
            group["matcher"] = Value::String(matcher.into());
        }
        groups.push(group);
    }
    write_hooks(&value)
}

fn uninstall_hooks() -> Result<(), Box<dyn std::error::Error>> {
    let path = hooks_path()?;
    if !path.exists() {
        return Ok(());
    }
    let mut value = read_hooks()?;
    if remove_owned_hooks(&mut value) {
        write_hooks(&value)?;
    }
    Ok(())
}

fn write_hooks(value: &Value) -> Result<(), Box<dyn std::error::Error>> {
    let path = hooks_path()?;
    let parent = path.parent().ok_or("hooks.json has no parent directory")?;
    fs::create_dir_all(parent)?;
    let mut encoded = serde_json::to_vec_pretty(value)?;
    encoded.push(b'\n');
    let destination = if path.is_symlink() {
        path.canonicalize()?
    } else {
        path.to_path_buf()
    };
    let mut file = AtomicWriteFile::open(destination)?;
    file.write_all(&encoded)?;
    file.commit()?;
    Ok(())
}

fn get_codex_config() -> Result<Option<Value>, Box<dyn std::error::Error>> {
    let output = codex_command()
        .args(["mcp", "get", SERVER_NAME, "--json"])
        .output()
        .map_err(|error| format!("unable to run Codex CLI: {error}"))?;
    if output.status.success() {
        return Ok(Some(serde_json::from_slice(&output.stdout)?));
    }
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if diagnostic.contains("No MCP server named 'spectra' found") {
        Ok(None)
    } else {
        Err(format!("codex mcp get failed: {}", diagnostic.trim()).into())
    }
}

fn ownership(config: &Value, current: &Path) -> Result<Ownership, Box<dyn std::error::Error>> {
    let transport = &config["transport"];
    if transport["type"] != "stdio" {
        return Ok(Ownership::Foreign);
    }
    let command = transport["command"]
        .as_str()
        .ok_or("Codex stdio MCP configuration has no command")?;
    let args = transport["args"]
        .as_array()
        .ok_or("Codex stdio MCP configuration has no args")?;
    let expected_args = ["serve", "--mcp"];
    let args_match = args
        .iter()
        .filter_map(Value::as_str)
        .eq(expected_args.iter().copied());
    let configured = PathBuf::from(command);
    let resolved = configured
        .canonicalize()
        .unwrap_or_else(|_| configured.clone());
    if args_match && resolved == current {
        return Ok(Ownership::Current);
    }
    let looks_owned = configured
        .file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem == "spectra")
        && args_match;
    if !looks_owned {
        return Ok(Ownership::Foreign);
    }
    Ok(Ownership::Stale)
}

fn codex_command() -> Command {
    Command::new(std::env::var_os("SPECTRA_CODEX_BIN").unwrap_or_else(|| "codex".into()))
}

fn checked(output: Output, label: &str) -> Result<Output, Box<dyn std::error::Error>> {
    if output.status.success() {
        Ok(output)
    } else {
        Err(format!(
            "{label} failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config(command: &str, args: &[&str]) -> Value {
        json!({
            "name": "spectra",
            "enabled": true,
            "transport": {"type": "stdio", "command": command, "args": args}
        })
    }

    #[test]
    fn recognizes_current_stale_and_foreign_entries() {
        let current = std::env::current_exe().unwrap().canonicalize().unwrap();
        assert_eq!(
            ownership(
                &config(current.to_str().unwrap(), &["serve", "--mcp"]),
                &current
            )
            .unwrap(),
            Ownership::Current
        );
        assert_eq!(
            ownership(
                &config("/old/location/spectra", &["serve", "--mcp"]),
                &current
            )
            .unwrap(),
            Ownership::Stale
        );
        assert_eq!(
            ownership(&config("/usr/bin/other", &["serve", "--mcp"]), &current).unwrap(),
            Ownership::Foreign
        );
        assert_eq!(
            ownership(&config("/old/location/spectra", &["different"]), &current).unwrap(),
            Ownership::Foreign
        );
    }
}
