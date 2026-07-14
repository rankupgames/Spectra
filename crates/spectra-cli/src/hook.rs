//! Fail-open lifecycle adapters for supported agent hook wire formats.

use std::{
    io::{self, Read},
    path::{Path, PathBuf},
};

use serde_json::{Value, json};
use spectra_core::{LedgerEventKind, LedgerSource, LedgerState, LedgerStore, ledger::redact_text};

const MAX_HOOK_BYTES: usize = 1_048_576;

pub fn run_stdin(agent: &str) {
    let mut input = Vec::new();
    let output = io::stdin()
        .take((MAX_HOOK_BYTES + 1) as u64)
        .read_to_end(&mut input)
        .ok()
        .filter(|_| input.len() <= MAX_HOOK_BYTES)
        .and_then(|_| handle_for(agent, &input).ok())
        .flatten();
    if let Some(output) = output {
        println!("{output}");
    }
}

#[cfg(test)]
fn handle(input: &[u8]) -> Result<Option<Value>, Box<dyn std::error::Error>> {
    handle_for("codex", input)
}

fn handle_for(agent: &str, input: &[u8]) -> Result<Option<Value>, Box<dyn std::error::Error>> {
    let agent = agent.to_ascii_lowercase();
    if !matches!(agent.as_str(), "codex" | "claude" | "gemini" | "cursor") {
        return Err(format!("unsupported lifecycle hook agent '{agent}'").into());
    }
    let event: Value = serde_json::from_slice(input)?;
    let event_name = string(&event, "hook_event_name").unwrap_or_default();
    let cwd = event_cwd(&event)?;
    let project = project_root(&cwd);
    let source = event_source(&agent, &event);

    match (agent.as_str(), event_name) {
        ("codex" | "claude", "SessionStart" | "UserPromptSubmit")
        | ("gemini", "SessionStart" | "BeforeAgent") => {
            context_output(&project, &source, event_name, ContextFormat::HookSpecific)
        }
        ("cursor", "sessionStart") => {
            context_output(&project, &source, event_name, ContextFormat::Cursor)
        }
        ("codex" | "claude", "PermissionRequest") => {
            record_permission(&project, &source, &event)?;
            Ok(None)
        }
        ("codex" | "claude", "PostToolUse") | ("gemini", "AfterTool") => {
            record_tool_result(&project, &source, &event)?;
            Ok(None)
        }
        ("cursor", "afterFileEdit") => {
            record_cursor_edit(&project, &source, &event)?;
            Ok(Some(json!({})))
        }
        ("cursor", "afterShellExecution" | "postToolUse") => {
            record_tool_result(&project, &source, &event)?;
            Ok(Some(json!({})))
        }
        ("codex" | "claude", "Stop") | ("gemini", "AfterAgent") | ("cursor", "stop") => {
            record_stop(&project, &source, &event, &agent)?;
            Ok(Some(json!({})))
        }
        _ => Ok(None),
    }
}

#[derive(Clone, Copy)]
enum ContextFormat {
    HookSpecific,
    Cursor,
}

fn context_output(
    project: &Path,
    source: &LedgerSource,
    event_name: &str,
    format: ContextFormat,
) -> Result<Option<Value>, Box<dyn std::error::Error>> {
    let projection = LedgerStore::transaction(project, |ledger| Ok(ledger.projection_for(source)))?;
    if projection.sequence == 0 {
        return Ok(Some(json!({})));
    }
    let context = format!(
        "Spectra state ledger (bounded, replay-derived):\n{}",
        projection.text
    );
    Ok(Some(match format {
        ContextFormat::HookSpecific => json!({
            "hookSpecificOutput": {
                "hookEventName": event_name,
                "additionalContext": context
            }
        }),
        ContextFormat::Cursor => json!({"additional_context": context}),
    }))
}

fn record_permission(
    project: &Path,
    source: &LedgerSource,
    event: &Value,
) -> Result<(), Box<dyn std::error::Error>> {
    let tool = tool_name(event).unwrap_or("tool");
    let action = if tool == "apply_patch" {
        command(event)
            .map(extract_patch_paths)
            .map(|paths| format!("edit {}", paths.join(",")))
            .unwrap_or_else(|| "file edit".into())
    } else {
        command(event)
            .map(|command| redact_text(&command))
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| tool.to_owned())
    };
    let base = correlation(source, event, &format!("permission:{tool}:{action}"));
    LedgerStore::transaction(project, |ledger| {
        if matches!(
            ledger.state_for(source),
            LedgerState::Idle
                | LedgerState::Observing
                | LedgerState::Blocked
                | LedgerState::Complete
        ) {
            ledger.append_once_for(
                source.clone(),
                format!("{base}:requested"),
                LedgerEventKind::AuthorizationRequested { action },
            )?;
        }
        Ok(())
    })?;
    Ok(())
}

fn record_cursor_edit(
    project: &Path,
    source: &LedgerSource,
    event: &Value,
) -> Result<(), Box<dyn std::error::Error>> {
    let paths = edit_paths(event).unwrap_or_else(|| vec!["<edit>".into()]);
    record_edit(project, source, event, paths)
}

fn record_tool_result(
    project: &Path,
    source: &LedgerSource,
    event: &Value,
) -> Result<(), Box<dyn std::error::Error>> {
    let tool = tool_name(event).unwrap_or_default();
    if is_edit_tool(tool) {
        let paths = edit_paths(event).unwrap_or_else(|| vec!["<edit>".into()]);
        return record_edit(project, source, event, paths);
    }
    if is_shell_tool(tool) || string(event, "command").is_some() {
        let Some(command) = command(event).filter(|command| is_verification(command)) else {
            return Ok(());
        };
        let response = event
            .get("tool_response")
            .or_else(|| event.get("result"))
            .unwrap_or(&Value::Null);
        let exit_code = find_i64(response, &["exit_code", "exitCode", "exit_code_value"])
            .or_else(|| find_i64(event, &["exit_code", "exitCode"]))
            .and_then(|value| i32::try_from(value).ok());
        let success = exit_code == Some(0)
            || (exit_code.is_none()
                && !find_bool(response, &["is_error", "isError", "error"]).unwrap_or(false));
        let output_bytes = response_bytes(response);
        let base = correlation(source, event, &format!("verify:{command}"));
        LedgerStore::transaction(project, |ledger| {
            if ledger.state_for(source) != LedgerState::Editing {
                return Ok(());
            }
            ledger.append_once_for(
                source.clone(),
                format!("{base}:started"),
                LedgerEventKind::VerificationStarted {
                    command: command.clone(),
                },
            )?;
            ledger.append_once_for(
                source.clone(),
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

fn record_edit(
    project: &Path,
    source: &LedgerSource,
    event: &Value,
    paths: Vec<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let base = correlation(source, event, "edit");
    LedgerStore::transaction(project, |ledger| {
        if ledger.events().iter().any(|event| {
            event.correlation_id.as_deref() == Some(&format!("{base}:authorized"))
                || event.correlation_id.as_deref() == Some(&format!("{base}:applied"))
                || event.correlation_id.as_deref() == Some(&format!("{base}:observed"))
        }) {
            return Ok(());
        }
        if ledger.state_for(source) == LedgerState::AwaitingAuthorization {
            ledger.append_once_for(
                source.clone(),
                format!("{base}:authorized"),
                LedgerEventKind::EditAuthorized {
                    action: "file edit".into(),
                },
            )?;
            ledger.append_once_for(
                source.clone(),
                format!("{base}:applied"),
                LedgerEventKind::EditApplied { paths },
            )?;
        } else {
            ledger.append_once_for(
                source.clone(),
                format!("{base}:observed"),
                LedgerEventKind::EditObserved { paths },
            )?;
        }
        Ok(())
    })?;
    Ok(())
}

fn record_stop(
    project: &Path,
    source: &LedgerSource,
    event: &Value,
    agent: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let base = correlation(source, event, "stop");
    LedgerStore::transaction(project, |ledger| {
        match ledger.state_for(source) {
            LedgerState::Editing | LedgerState::Idle | LedgerState::Observing => {
                ledger.append_once_for(
                    source.clone(),
                    format!("{base}:complete"),
                    LedgerEventKind::Completed {
                        summary: format!("{agent} turn completed"),
                    },
                )?;
            }
            LedgerState::AwaitingAuthorization | LedgerState::Verifying => {
                ledger.append_once_for(
                    source.clone(),
                    format!("{base}:blocked"),
                    LedgerEventKind::Blocked {
                        reason: format!("{agent} turn stopped with pending work"),
                    },
                )?;
            }
            LedgerState::Blocked | LedgerState::Complete => {}
        }
        Ok(())
    })?;
    Ok(())
}

fn event_cwd(event: &Value) -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(string(event, "cwd")
        .map(PathBuf::from)
        .or_else(|| {
            event
                .get("workspace_roots")
                .and_then(Value::as_array)
                .and_then(|roots| roots.first())
                .and_then(Value::as_str)
                .map(PathBuf::from)
        })
        .unwrap_or(std::env::current_dir()?))
}

fn event_source(agent: &str, event: &Value) -> LedgerSource {
    LedgerSource {
        harness: agent.to_owned(),
        session_id: ["session_id", "conversation_id"]
            .into_iter()
            .find_map(|key| string(event, key))
            .unwrap_or("session")
            .chars()
            .take(96)
            .collect(),
    }
}

fn project_root(cwd: &Path) -> PathBuf {
    cwd.ancestors()
        .find(|path| path.join(".git").exists())
        .unwrap_or(cwd)
        .to_path_buf()
}

fn tool_name(event: &Value) -> Option<&str> {
    string(event, "tool_name")
        .or_else(|| string(event, "tool"))
        .or_else(|| string(event, "name"))
}

fn command(event: &Value) -> Option<String> {
    let input = event.get("tool_input").or_else(|| event.get("input"));
    input
        .into_iter()
        .flat_map(|value| {
            ["command", "cmd"]
                .into_iter()
                .filter_map(|key| string(value, key))
        })
        .next()
        .or_else(|| string(event, "command"))
        .map(|value| value.chars().take(4_096).collect())
}

fn edit_paths(event: &Value) -> Option<Vec<String>> {
    if tool_name(event) == Some("apply_patch") {
        return command(event).map(extract_patch_paths);
    }
    let input = event
        .get("tool_input")
        .or_else(|| event.get("input"))
        .unwrap_or(event);
    let mut paths = ["file_path", "path", "target_file"]
        .into_iter()
        .filter_map(|key| string(input, key).or_else(|| string(event, key)))
        .map(|path| redact_text(path).chars().take(4_096).collect::<String>())
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    (!paths.is_empty()).then_some(paths)
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
        .take(256)
        .collect();
    paths.sort();
    paths.dedup();
    if paths.is_empty() {
        paths.push("<edit>".into());
    }
    paths
}

fn is_edit_tool(tool: &str) -> bool {
    matches!(
        tool.to_ascii_lowercase().as_str(),
        "apply_patch" | "edit" | "write" | "multiedit" | "write_file" | "replace"
    )
}

fn is_shell_tool(tool: &str) -> bool {
    matches!(
        tool.to_ascii_lowercase().as_str(),
        "bash" | "shell" | "run_shell_command" | "terminal"
    )
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

fn correlation(source: &LedgerSource, event: &Value, suffix: &str) -> String {
    let turn = string(event, "turn_id").unwrap_or("turn");
    let explicit = ["tool_use_id", "event_id", "timestamp"]
        .into_iter()
        .find_map(|key| string(event, key));
    let fallback;
    let event_id = if let Some(explicit) = explicit {
        explicit
    } else {
        fallback = format!("{:016x}", stable_event_hash(event));
        &fallback
    };
    format!(
        "{}:{}:{turn}:{event_id}:{suffix}",
        source.harness, source.session_id
    )
}

fn stable_event_hash(event: &Value) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in serde_json::to_vec(event).unwrap_or_default() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
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
        handle(&serde_json::to_vec(&edit).unwrap()).unwrap();
        let check = json!({
            "session_id":"s1", "turn_id":"t1", "hook_event_name":"PostToolUse",
            "cwd":root, "tool_name":"Bash", "tool_use_id":"check1",
            "tool_input":{"command":"cargo test --workspace"},
            "tool_response":{"exit_code":0,"output":"all tests passed"}
        });
        handle(&serde_json::to_vec(&check).unwrap()).unwrap();
        let store = LedgerStore::open(edit["cwd"].as_str().unwrap().as_ref()).unwrap();
        assert_eq!(store.events().len(), 3);
        assert_eq!(store.state(), LedgerState::Complete);
        let prompt = json!({"session_id":"s1","hook_event_name":"UserPromptSubmit","cwd":edit["cwd"],"prompt":"next"});
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
    fn claude_gemini_and_cursor_outputs_match_their_wire_contracts() {
        let root = fixture();
        let claude = json!({"session_id":"c","hook_event_name":"SessionStart","cwd":root});
        assert_eq!(
            handle_for("claude", &serde_json::to_vec(&claude).unwrap()).unwrap(),
            Some(json!({}))
        );
        let cursor = json!({"conversation_id":"x","hook_event_name":"sessionStart","cwd":root});
        assert_eq!(
            handle_for("cursor", &serde_json::to_vec(&cursor).unwrap()).unwrap(),
            Some(json!({}))
        );
        fs::remove_dir_all(root).unwrap();
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
