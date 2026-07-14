use std::{
    fs,
    path::{Path, PathBuf},
};

use serde_json::{Map, Value, json};

use super::{BoxError, spectra_executable, write_atomic};

const STATUS: &str = "Spectra context ledger";

#[derive(Clone, Copy)]
pub(super) enum HookSchema {
    Grouped,
    Cursor,
}

#[derive(Clone)]
pub(super) struct HookTarget {
    pub label: &'static str,
    pub agent: &'static str,
    pub path: PathBuf,
    pub schema: HookSchema,
    pub events: &'static [(&'static str, Option<&'static str>)],
}

pub(super) fn install(target: &HookTarget, dry_run: bool) -> Result<String, BoxError> {
    let executable = spectra_executable()?;
    let command = hook_command(&executable, target.agent)?;
    let mut root = read_object(&target.path)?;
    let current = hooks_are_current(&root, target, &command)?;
    if current {
        return Ok(format!(
            "{}: Ledger hooks are already current.",
            target.label
        ));
    }
    if dry_run {
        return Ok(format!(
            "{}: Would configure {} Ledger hooks in {}.",
            target.label,
            target.events.len(),
            target.path.display()
        ));
    }
    remove_owned(&mut root, target.schema);
    if matches!(target.schema, HookSchema::Cursor) {
        root.entry("version").or_insert(json!(1));
    }
    let hooks = object_field(&mut root, "hooks", &target.path)?;
    for (event, matcher) in target.events {
        let groups = hooks.entry(*event).or_insert_with(|| json!([]));
        let groups = groups
            .as_array_mut()
            .ok_or_else(|| format!("{} hook '{event}' must be an array", target.path.display()))?;
        let handler = match target.schema {
            HookSchema::Grouped => {
                let mut group = json!({
                    "hooks": [{"type":"command","command":command,"timeout":5,"statusMessage":STATUS}]
                });
                if let Some(matcher) = matcher {
                    group["matcher"] = json!(matcher);
                }
                group
            }
            HookSchema::Cursor => json!({"command":command}),
        };
        groups.push(handler);
    }
    write_json(&target.path, &Value::Object(root))?;
    Ok(format!("{}: Ledger hooks configured.", target.label))
}

pub(super) fn uninstall(target: &HookTarget, dry_run: bool) -> Result<String, BoxError> {
    let mut root = read_object(&target.path)?;
    if !contains_owned(&root, target.schema) {
        return Ok(format!(
            "{}: Ledger hooks are not configured.",
            target.label
        ));
    }
    if dry_run {
        return Ok(format!("{}: Would remove Ledger hooks.", target.label));
    }
    remove_owned(&mut root, target.schema);
    write_json(&target.path, &Value::Object(root))?;
    Ok(format!("{}: Removed Ledger hooks.", target.label))
}

pub(super) fn status(target: &HookTarget) -> Result<&'static str, BoxError> {
    let executable = spectra_executable()?;
    let command = hook_command(&executable, target.agent)?;
    let root = read_object(&target.path)?;
    if hooks_are_current(&root, target, &command)? {
        Ok("current")
    } else if contains_owned(&root, target.schema) {
        Ok("stale")
    } else {
        Ok("missing")
    }
}

fn hooks_are_current(
    root: &Map<String, Value>,
    target: &HookTarget,
    command: &str,
) -> Result<bool, BoxError> {
    let Some(hooks) = root.get("hooks").and_then(Value::as_object) else {
        return Ok(false);
    };
    Ok(target.events.iter().all(|(event, matcher)| {
        hooks
            .get(*event)
            .and_then(Value::as_array)
            .is_some_and(|groups| {
                groups.iter().any(|group| match target.schema {
                    HookSchema::Cursor => {
                        group.get("command").and_then(Value::as_str) == Some(command)
                    }
                    HookSchema::Grouped => {
                        let matcher_matches = matcher
                            .map(|expected| {
                                group.get("matcher").and_then(Value::as_str) == Some(expected)
                            })
                            .unwrap_or_else(|| group.get("matcher").is_none());
                        matcher_matches
                            && group.get("hooks").and_then(Value::as_array).is_some_and(
                                |handlers| {
                                    handlers.iter().any(|handler| {
                                        owned_grouped(handler)
                                            && handler.get("command").and_then(Value::as_str)
                                                == Some(command)
                                    })
                                },
                            )
                    }
                })
            })
    }))
}

fn contains_owned(root: &Map<String, Value>, schema: HookSchema) -> bool {
    root.get("hooks")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|hooks| hooks.values())
        .filter_map(Value::as_array)
        .flatten()
        .any(|entry| match schema {
            HookSchema::Cursor => entry
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(owned_command),
            HookSchema::Grouped => entry
                .get("hooks")
                .and_then(Value::as_array)
                .is_some_and(|handlers| handlers.iter().any(owned_grouped)),
        })
}

fn remove_owned(root: &mut Map<String, Value>, schema: HookSchema) {
    let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) else {
        return;
    };
    for groups in hooks.values_mut().filter_map(Value::as_array_mut) {
        match schema {
            HookSchema::Cursor => groups.retain(|entry| {
                !entry
                    .get("command")
                    .and_then(Value::as_str)
                    .is_some_and(owned_command)
            }),
            HookSchema::Grouped => {
                for group in groups.iter_mut() {
                    if let Some(handlers) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                        handlers.retain(|handler| !owned_grouped(handler));
                    }
                }
                groups.retain(|group| {
                    group
                        .get("hooks")
                        .and_then(Value::as_array)
                        .is_none_or(|v| !v.is_empty())
                });
            }
        }
    }
    hooks.retain(|_, value| value.as_array().is_none_or(|values| !values.is_empty()));
}

fn owned_grouped(value: &Value) -> bool {
    value.get("type").and_then(Value::as_str) == Some("command")
        && (value.get("statusMessage").and_then(Value::as_str) == Some(STATUS)
            || value
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(owned_command))
}

fn owned_command(command: &str) -> bool {
    command.contains("spectra") && command.contains(" hook")
}

fn hook_command(executable: &Path, agent: &str) -> Result<String, BoxError> {
    let path = executable
        .to_str()
        .ok_or("Spectra executable path is not valid UTF-8")?;
    #[cfg(windows)]
    return Ok(format!(
        "\"{}\" hook --agent {agent}",
        path.replace('"', "\\\"")
    ));
    #[cfg(not(windows))]
    Ok(format!(
        "'{}' hook --agent {agent}",
        path.replace('\'', "'\\''")
    ))
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
    root.entry(key).or_insert_with(|| json!({}));
    root.get_mut(key)
        .and_then(Value::as_object_mut)
        .ok_or_else(|| format!("{} field '{key}' must be an object", path.display()).into())
}

fn write_json(path: &Path, value: &Value) -> Result<(), BoxError> {
    let mut encoded = serde_json::to_vec_pretty(value)?;
    encoded.push(b'\n');
    write_atomic(path, &encoded)
}
