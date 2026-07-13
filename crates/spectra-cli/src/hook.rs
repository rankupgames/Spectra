//! Codex lifecycle hook adapter. Inputs are the documented JSON objects on stdin.

use std::{
    io::{self, Read},
    path::{Path, PathBuf},
};

use serde_json::{Value, json};
use spectra_core::{LedgerEventKind, LedgerState, LedgerStore, ledger::redact_text};

pub fn run_stdin() {
    let mut input = Vec::new();
    let output = io::stdin()
        .read_to_end(&mut input)
        .ok()
        .and_then(|_| handle(&input).ok())
        .flatten();
    if let Some(output) = output {
        println!("{output}");
    }
}

fn handle(input: &[u8]) -> Result<Option<Value>, Box<dyn std::error::Error>> {
    let event: Value = serde_json::from_slice(input)?;
    let event_name = string(&event, "hook_event_name").unwrap_or_default();
    let cwd = string(&event, "cwd")
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let project = project_root(&cwd);

    match event_name {
        "SessionStart" | "UserPromptSubmit" => context_output(&project, event_name),
        "PermissionRequest" => {
            record_permission(&project, &event)?;
            Ok(None)
        }
        "PostToolUse" => {
            record_tool_result(&project, &event)?;
            Ok(None)
        }
        "Stop" => {
            record_stop(&project, &event)?;
            // Stop requires JSON on stdout even when the hook has no decision.
            Ok(Some(json!({})))
        }
        _ => Ok(None),
    }
}

fn context_output(
    project: &Path,
    event_name: &str,
) -> Result<Option<Value>, Box<dyn std::error::Error>> {
    let projection = LedgerStore::transaction(project, |ledger| Ok(ledger.projection()))?;
    if projection.sequence == 0 {
        return Ok(Some(json!({})));
    }
    Ok(Some(json!({
        "hookSpecificOutput": {
            "hookEventName": event_name,
            "additionalContext": format!("Spectra state ledger (bounded, replay-derived):\n{}", projection.text)
        }
    })))
}

fn record_permission(project: &Path, event: &Value) -> Result<(), Box<dyn std::error::Error>> {
    let tool = string(event, "tool_name").unwrap_or("tool");
    let action = if tool == "apply_patch" {
        command(event)
            .map(extract_patch_paths)
            .map(|paths| format!("edit {}", paths.join(",")))
            .unwrap_or_else(|| "Codex file edit".into())
    } else {
        command(event)
            .map(|command| redact_text(&command))
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| tool.to_owned())
    };
    let base = correlation(event, &format!("permission:{tool}:{action}"));
    LedgerStore::transaction(project, |ledger| {
        if matches!(
            ledger.state(),
            LedgerState::Idle
                | LedgerState::Observing
                | LedgerState::Blocked
                | LedgerState::Complete
        ) {
            ledger.append_once(
                format!("{base}:requested"),
                LedgerEventKind::AuthorizationRequested { action },
            )?;
        }
        Ok(())
    })?;
    Ok(())
}

fn record_tool_result(project: &Path, event: &Value) -> Result<(), Box<dyn std::error::Error>> {
    let tool = string(event, "tool_name").unwrap_or_default();
    let base = correlation(event, &format!("post:{tool}"));
    if tool == "apply_patch" {
        let paths = command(event)
            .map(extract_patch_paths)
            .unwrap_or_else(|| vec!["<edit>".into()]);
        LedgerStore::transaction(project, |ledger| {
            enter_editing(ledger, &base)?;
            ledger.append_once(
                format!("{base}:applied"),
                LedgerEventKind::EditApplied { paths },
            )?;
            Ok(())
        })?;
    } else if tool == "Bash" {
        let Some(command) = command(event).filter(|command| is_verification(command)) else {
            return Ok(());
        };
        let response = &event["tool_response"];
        let exit_code = find_i64(response, &["exit_code", "exitCode"])
            .and_then(|value| i32::try_from(value).ok());
        let success = exit_code == Some(0)
            || (exit_code.is_none()
                && !find_bool(response, &["is_error", "isError"]).unwrap_or(false));
        let output_bytes = response_bytes(response);
        LedgerStore::transaction(project, |ledger| {
            if ledger.state() != LedgerState::Editing {
                return Ok(());
            }
            ledger.append_once(
                format!("{base}:started"),
                LedgerEventKind::VerificationStarted {
                    command: command.clone(),
                },
            )?;
            ledger.append_once(
                format!("{base}:finished"),
                LedgerEventKind::VerificationFinished {
                    command,
                    success,
                    exit_code,
                    output_bytes,
                },
            )?;
            Ok(())
        })?;
    }
    Ok(())
}

fn enter_editing(ledger: &mut LedgerStore, base: &str) -> spectra_core::Result<()> {
    if ledger.state() == LedgerState::Verifying {
        ledger.append_once(
            format!("{base}:interrupted"),
            LedgerEventKind::Blocked {
                reason: "edit interrupted verification".into(),
            },
        )?;
    }
    if matches!(
        ledger.state(),
        LedgerState::Idle | LedgerState::Observing | LedgerState::Blocked | LedgerState::Complete
    ) {
        ledger.append_once(
            format!("{base}:implicit-request"),
            LedgerEventKind::AuthorizationRequested {
                action: "Codex file edit".into(),
            },
        )?;
    }
    if ledger.state() == LedgerState::AwaitingAuthorization {
        ledger.append_once(
            format!("{base}:authorized"),
            LedgerEventKind::EditAuthorized {
                action: "Codex file edit".into(),
            },
        )?;
    }
    Ok(())
}

fn record_stop(project: &Path, event: &Value) -> Result<(), Box<dyn std::error::Error>> {
    let base = correlation(event, "stop");
    LedgerStore::transaction(project, |ledger| {
        match ledger.state() {
            LedgerState::Editing => {
                ledger.append_once(
                    format!("{base}:complete"),
                    LedgerEventKind::Completed {
                        summary: "Codex turn stopped after edits".into(),
                    },
                )?;
            }
            LedgerState::AwaitingAuthorization | LedgerState::Verifying => {
                ledger.append_once(
                    format!("{base}:blocked"),
                    LedgerEventKind::Blocked {
                        reason: "Codex turn stopped with pending work".into(),
                    },
                )?;
            }
            _ => {}
        }
        Ok(())
    })?;
    Ok(())
}

fn project_root(cwd: &Path) -> PathBuf {
    cwd.ancestors()
        .find(|path| path.join(".git").exists())
        .unwrap_or(cwd)
        .to_path_buf()
}

fn command(event: &Value) -> Option<String> {
    let input = &event["tool_input"];
    ["command", "cmd"]
        .into_iter()
        .find_map(|key| string(input, key))
        .map(ToOwned::to_owned)
}

fn extract_patch_paths(patch: String) -> Vec<String> {
    let mut paths: Vec<String> = patch
        .lines()
        .filter_map(|line| {
            ["*** Add File: ", "*** Update File: ", "*** Delete File: "]
                .into_iter()
                .find_map(|prefix| line.strip_prefix(prefix))
                .map(redact_text)
        })
        .collect();
    paths.sort();
    paths.dedup();
    if paths.is_empty() {
        paths.push("<edit>".into());
    }
    paths
}

fn is_verification(command: &str) -> bool {
    let command = command.to_ascii_lowercase();
    [
        "cargo test",
        "cargo check",
        "cargo clippy",
        "cargo build",
        "npm test",
        "npm run test",
        "pnpm test",
        "yarn test",
        "pytest",
        "go test",
        "swift test",
    ]
    .iter()
    .any(|marker| command.contains(marker))
}

fn correlation(event: &Value, suffix: &str) -> String {
    let session = string(event, "session_id").unwrap_or("session");
    let turn = string(event, "turn_id").unwrap_or("turn");
    let tool_use = string(event, "tool_use_id").unwrap_or(suffix);
    format!("codex:{session}:{turn}:{tool_use}:{suffix}")
}

fn string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn find_i64(value: &Value, keys: &[&str]) -> Option<i64> {
    match value {
        Value::Object(map) => keys
            .iter()
            .find_map(|key| map.get(*key).and_then(Value::as_i64))
            .or_else(|| map.values().find_map(|value| find_i64(value, keys))),
        Value::Array(values) => values.iter().find_map(|value| find_i64(value, keys)),
        _ => None,
    }
}

fn find_bool(value: &Value, keys: &[&str]) -> Option<bool> {
    match value {
        Value::Object(map) => keys
            .iter()
            .find_map(|key| map.get(*key).and_then(Value::as_bool))
            .or_else(|| map.values().find_map(|value| find_bool(value, keys))),
        Value::Array(values) => values.iter().find_map(|value| find_bool(value, keys)),
        _ => None,
    }
}

fn response_bytes(value: &Value) -> usize {
    match value {
        Value::String(text) => text.len(),
        Value::Array(values) => values.iter().map(response_bytes).sum(),
        Value::Object(map) => map.values().map(response_bytes).sum(),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn fixture() -> PathBuf {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("spectra-hook-{}-{id}", std::process::id()));
        fs::create_dir_all(root.join(".git")).unwrap();
        root
    }

    #[test]
    fn records_edit_verification_and_reinjects_bounded_state() {
        let root = fixture();
        let edit = json!({
            "session_id":"s1", "turn_id":"t1", "hook_event_name":"PostToolUse",
            "cwd":root, "tool_name":"apply_patch", "tool_use_id":"edit1",
            "tool_input":{"command":"*** Update File: src/lib.rs\n"}, "tool_response":{}
        });
        handle(&serde_json::to_vec(&edit).unwrap()).unwrap();
        // Hook retries must not duplicate immutable events.
        handle(&serde_json::to_vec(&edit).unwrap()).unwrap();
        let check = json!({
            "session_id":"s1", "turn_id":"t1", "hook_event_name":"PostToolUse",
            "cwd":root, "tool_name":"Bash", "tool_use_id":"check1",
            "tool_input":{"command":"cargo test --workspace"},
            "tool_response":{"exit_code":0,"output":"all tests passed"}
        });
        handle(&serde_json::to_vec(&check).unwrap()).unwrap();

        let store = LedgerStore::open(edit["cwd"].as_str().unwrap().as_ref()).unwrap();
        assert_eq!(store.events().len(), 5);
        assert_eq!(store.state(), LedgerState::Complete);
        let prompt =
            json!({"hook_event_name":"UserPromptSubmit","cwd":edit["cwd"],"prompt":"next"});
        let output = handle(&serde_json::to_vec(&prompt).unwrap())
            .unwrap()
            .unwrap();
        let context = output["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(context.contains("edit src/lib.rs"));
        assert!(context.contains("success=true"));
        assert!(context.chars().count() < 700);
        fs::remove_dir_all(edit["cwd"].as_str().unwrap()).unwrap();
    }

    #[test]
    fn malformed_input_fails_open_at_the_stdio_boundary() {
        assert!(handle(b"not json").is_err());
    }

    #[test]
    fn permission_events_store_paths_but_never_patch_bodies() {
        let root = fixture();
        let event = json!({
            "session_id":"s1", "turn_id":"t1", "hook_event_name":"PermissionRequest",
            "cwd":root, "tool_name":"apply_patch",
            "tool_input":{"command":"*** Update File: src/lib.rs\n@@\n+fn proprietary_body() {}\n+XAI_KEY=secret\n"}
        });
        handle(&serde_json::to_vec(&event).unwrap()).unwrap();
        let persisted = fs::read_to_string(root.join(".spectra/ledger-v1.jsonl")).unwrap();
        assert!(persisted.contains("edit src/lib.rs"));
        assert!(!persisted.contains("proprietary_body"));
        assert!(!persisted.contains("secret"));
        fs::remove_dir_all(root).unwrap();
    }
}
