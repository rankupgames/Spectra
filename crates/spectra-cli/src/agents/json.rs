use std::{
    fs,
    path::{Path, PathBuf},
};

use serde_json::{Map, Value, json};

use super::{
    BoxError, JsonTarget, configured_command_is_spectra, resolved, spectra_executable, write_atomic,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Ownership {
    Current,
    Stale,
    Foreign,
}

pub(super) fn install(target: JsonTarget, dry_run: bool) -> Result<String, BoxError> {
    let path = target.path.clone();
    let executable = spectra_executable()?;
    let mut root = read_object(&path)?;
    let servers = object_field(&mut root, target.root_key, &path)?;
    let ownership = servers
        .get("spectra")
        .map(|entry| standard_ownership(entry, &executable, target.args))
        .transpose()?;
    if ownership == Some(Ownership::Foreign) {
        return Err(format!(
            "{} already has a non-Spectra-owned MCP entry named 'spectra' in {}; refusing to overwrite it",
            target.label,
            path.display()
        ).into());
    }
    if ownership == Some(Ownership::Current) {
        return Ok(format!(
            "{}: Spectra topology MCP is already configured.",
            target.label
        ));
    }
    let verb = if ownership.is_some() {
        "update"
    } else {
        "configure"
    };
    if dry_run {
        return Ok(format!(
            "{}: Would {verb} the Spectra topology MCP in {}.",
            target.label,
            path.display()
        ));
    }
    servers.insert("spectra".into(), standard_entry(&executable, target.args)?);
    write_json(&path, &Value::Object(root))?;
    Ok(format!(
        "{}: Spectra topology MCP {}. Restart the agent if it is running.",
        target.label,
        if ownership.is_some() {
            "updated"
        } else {
            "configured"
        }
    ))
}

pub(super) fn uninstall(target: JsonTarget, dry_run: bool) -> Result<String, BoxError> {
    let path = target.path.clone();
    let executable = spectra_executable()?;
    let mut root = read_object(&path)?;
    let Some(value) = root.get_mut(target.root_key) else {
        return Ok(format!("{}: Spectra is not configured.", target.label));
    };
    let servers = value.as_object_mut().ok_or_else(|| {
        format!(
            "{} field '{}' must be an object",
            path.display(),
            target.root_key
        )
    })?;
    let Some(entry) = servers.get("spectra") else {
        return Ok(format!("{}: Spectra is not configured.", target.label));
    };
    if standard_ownership(entry, &executable, target.args)? == Ownership::Foreign {
        return Err(format!(
            "{}'s 'spectra' MCP entry is not owned by Spectra; refusing to remove it",
            target.label
        )
        .into());
    }
    if dry_run {
        return Ok(format!(
            "{}: Would remove Spectra's topology MCP configuration.",
            target.label
        ));
    }
    servers.remove("spectra");
    write_json(&path, &Value::Object(root))?;
    Ok(format!(
        "{}: Removed Spectra's topology MCP configuration.",
        target.label
    ))
}

pub(super) fn status(target: JsonTarget) -> Result<String, BoxError> {
    let path = target.path.clone();
    let executable = spectra_executable()?;
    let root = read_object(&path)?;
    let entry = match root.get(target.root_key) {
        None => None,
        Some(value) => Some(value.as_object().ok_or_else(|| {
            format!(
                "{} field '{}' must be an object",
                path.display(),
                target.root_key
            )
        })?),
    }
    .and_then(|servers| servers.get("spectra"));
    let mcp = match entry {
        None => "missing",
        Some(entry) => match standard_ownership(entry, &executable, target.args)? {
            Ownership::Current => "current",
            Ownership::Stale => "stale",
            Ownership::Foreign => "foreign conflict",
        },
    };
    Ok(format!("{}: MCP={mcp}, Ledger=not available", target.label))
}

pub(super) fn standard_ownership(
    entry: &Value,
    current: &Path,
    expected_args: &[&str],
) -> Result<Ownership, BoxError> {
    let command = entry
        .get("command")
        .and_then(Value::as_str)
        .ok_or("stdio MCP configuration has no string command")?;
    let args = entry
        .get("args")
        .and_then(Value::as_array)
        .ok_or("stdio MCP configuration has no args array")?;
    let args_match = string_array_is(args, expected_args);
    let managed_args = args_match || string_array_is(args, &["serve", "--mcp"]);
    let configured = PathBuf::from(command);
    if args_match && resolved(&configured) == current {
        Ok(Ownership::Current)
    } else if managed_args && configured_command_is_spectra(&configured) {
        Ok(Ownership::Stale)
    } else {
        Ok(Ownership::Foreign)
    }
}

pub(super) fn opencode_ownership(entry: &Value, current: &Path) -> Result<Ownership, BoxError> {
    if entry.get("type").and_then(Value::as_str) != Some("local") {
        return Ok(Ownership::Foreign);
    }
    let command = entry
        .get("command")
        .and_then(Value::as_array)
        .ok_or("OpenCode local MCP configuration has no command array")?;
    let Some(executable) = command.first().and_then(Value::as_str) else {
        return Err("OpenCode local MCP command array has no executable".into());
    };
    let args_match = string_array_is(command, &[executable, "serve", "--mcp"]);
    let configured = PathBuf::from(executable);
    if args_match && resolved(&configured) == current {
        Ok(Ownership::Current)
    } else if args_match && configured_command_is_spectra(&configured) {
        Ok(Ownership::Stale)
    } else {
        Ok(Ownership::Foreign)
    }
}

pub(super) fn opencode_entry(executable: &Path) -> Result<Value, BoxError> {
    let executable = executable
        .to_str()
        .ok_or("Spectra executable path is not valid UTF-8")?;
    Ok(json!({
        "type": "local",
        "command": [executable, "serve", "--mcp"],
        "enabled": true
    }))
}

fn standard_entry(executable: &Path, args: &[&str]) -> Result<Value, BoxError> {
    let executable = executable
        .to_str()
        .ok_or("Spectra executable path is not valid UTF-8")?;
    Ok(json!({"command": executable, "args": args}))
}

fn string_array_is(values: &[Value], expected: &[&str]) -> bool {
    values.len() == expected.len()
        && values
            .iter()
            .zip(expected)
            .all(|(value, expected)| value.as_str() == Some(*expected))
}

fn read_object(path: &Path) -> Result<Map<String, Value>, BoxError> {
    if !path.exists() {
        return Ok(Map::new());
    }
    let bytes = fs::read(path)?;
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(Map::new());
    }
    serde_json::from_slice::<Value>(&bytes)
        .map_err(|error| format!("{} is not valid JSON: {error}", path.display()))?
        .as_object()
        .cloned()
        .ok_or_else(|| format!("{} must contain a JSON object", path.display()).into())
}

fn object_field<'a>(
    root: &'a mut Map<String, Value>,
    key: &str,
    path: &Path,
) -> Result<&'a mut Map<String, Value>, BoxError> {
    let value = root.entry(key).or_insert_with(|| json!({}));
    value
        .as_object_mut()
        .ok_or_else(|| format!("{} field '{key}' must be an object", path.display()).into())
}

fn write_json(path: &Path, value: &Value) -> Result<(), BoxError> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    write_atomic(path, &bytes)
}
