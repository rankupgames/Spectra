use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use spectra_core::{LedgerState, LedgerStore};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn fixture() -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root =
        std::env::temp_dir().join(format!("spectra-cli-test-{}-{unique}", std::process::id()));
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/lib.rs"),
        "pub fn entry() { worker(); }\nfn worker() {}\n",
    )
    .unwrap();
    root
}

#[test]
fn init_and_map_complete_end_to_end() {
    let root = fixture();
    let binary = env!("CARGO_BIN_EXE_spectra");
    let init = Command::new(binary)
        .args(["init", root.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let map = Command::new(binary)
        .args([
            "map",
            "entry worker",
            "--path",
            root.to_str().unwrap(),
            "--max-nodes",
            "8",
        ])
        .output()
        .unwrap();
    assert!(
        map.status.success(),
        "{}",
        String::from_utf8_lossy(&map.stderr)
    );
    let stdout = String::from_utf8(map.stdout).unwrap();
    assert!(stdout.contains("PNG "));
    assert!(stdout.contains("N1=src/lib.rs:"));
    assert!(root.join(".spectra/index-v1.json").is_file());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rejects_node_budgets_outside_the_public_contract() {
    let output = Command::new(env!("CARGO_BIN_EXE_spectra"))
        .args(["map", "query", "--max-nodes", "97"])
        .output()
        .unwrap();
    assert!(!output.status.success());
}

#[test]
fn map_lazily_initializes_and_resyncs_the_ledger() {
    let root = fixture();
    let binary = env!("CARGO_BIN_EXE_spectra");
    let run_map = || {
        Command::new(binary)
            .args(["map", "entry worker", "--path", root.to_str().unwrap()])
            .output()
            .unwrap()
    };

    let first = run_map();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let ledger_path = root.join(".spectra/ledger-v1.jsonl");
    assert!(ledger_path.is_file());

    fs::write(
        root.join("src/lib.rs"),
        "pub fn entry() { worker(); }\nfn worker() {}\nfn added() {}\n",
    )
    .unwrap();
    let second = run_map();
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );

    let events: Vec<serde_json::Value> = fs::read_to_string(ledger_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events.len(), 4);
    assert_eq!(events[2]["kind"]["type"], "repository_synced");
    assert_eq!(events[2]["kind"]["changed"], 1);
    assert_eq!(events[3]["kind"]["type"], "map_rendered");
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn codex_install_is_owned_idempotent_and_reversible() {
    let root = fixture();
    let fake_codex = root.join("codex");
    let state = root.join("mcp-state.json");
    let codex_home = root.join("codex-home");
    let script = r#"#!/bin/sh
set -eu
case "$1 $2" in
  "mcp get")
    if [ -s "$FAKE_CODEX_STATE" ]; then
      /bin/cat "$FAKE_CODEX_STATE"
    else
      echo "Error: No MCP server named 'spectra' found." >&2
      exit 1
    fi
    ;;
  "mcp add")
    printf '{"name":"spectra","enabled":true,"transport":{"type":"stdio","command":"%s","args":["serve","--mcp"]}}\n' "$5" > "$FAKE_CODEX_STATE"
    ;;
  "mcp remove")
    : > "$FAKE_CODEX_STATE"
    ;;
  *)
    echo "unexpected fake Codex invocation: $*" >&2
    exit 2
    ;;
esac
"#;
    fs::write(&fake_codex, script).unwrap();
    let mut permissions = fs::metadata(&fake_codex).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_codex, permissions).unwrap();

    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_spectra"))
            .args(args)
            .env("SPECTRA_CODEX_BIN", &fake_codex)
            .env("FAKE_CODEX_STATE", &state)
            .env("SPECTRA_CODEX_HOME", &codex_home)
            .output()
            .unwrap()
    };

    let preview = run(&["install", "--dry-run"]);
    assert!(preview.status.success());
    assert!(String::from_utf8_lossy(&preview.stdout).contains("Would configure"));
    assert!(!state.exists());
    assert!(!codex_home.join("hooks.json").exists());

    let first = run(&["install"]);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(String::from_utf8_lossy(&first.stdout).contains("Codex configured"));
    let hooks_path = codex_home.join("hooks.json");
    let mut hooks: serde_json::Value =
        serde_json::from_slice(&fs::read(&hooks_path).unwrap()).unwrap();
    hooks["hooks"]["Stop"].as_array_mut().unwrap().push(serde_json::json!({
        "hooks": [{"type":"command","command":"/usr/bin/foreign-hook","statusMessage":"foreign"}]
    }));
    fs::write(&hooks_path, serde_json::to_vec_pretty(&hooks).unwrap()).unwrap();

    let second = run(&["install"]);
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(String::from_utf8_lossy(&second.stdout).contains("already configured"));

    let status = run(&["status"]);
    assert!(status.status.success());
    assert_eq!(
        String::from_utf8_lossy(&status.stdout).trim(),
        "Codex: MCP=current, Ledger hooks=current"
    );

    let removed = run(&["uninstall"]);
    assert!(
        removed.status.success(),
        "{}",
        String::from_utf8_lossy(&removed.stderr)
    );
    assert!(String::from_utf8_lossy(&removed.stdout).contains("Removed"));
    assert_eq!(
        String::from_utf8_lossy(&run(&["status"]).stdout).trim(),
        "Codex: MCP=missing, Ledger hooks=missing"
    );
    let hooks: serde_json::Value = serde_json::from_slice(&fs::read(&hooks_path).unwrap()).unwrap();
    assert_eq!(hooks["hooks"]["Stop"].as_array().unwrap().len(), 1);
    assert_eq!(
        hooks["hooks"]["Stop"][0]["hooks"][0]["statusMessage"],
        "foreign"
    );

    fs::write(
        &state,
        r#"{"name":"spectra","transport":{"type":"stdio","command":"/usr/bin/other","args":["serve","--mcp"]}}"#,
    )
    .unwrap();
    let conflict = run(&["install"]);
    assert!(!conflict.status.success());
    assert!(String::from_utf8_lossy(&conflict.stderr).contains("refusing to overwrite"));
    assert!(
        fs::read_to_string(&state)
            .unwrap()
            .contains("/usr/bin/other")
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn backtests_a_recorded_codex_hook_session_without_fact_loss() {
    let root = fixture();
    fs::create_dir_all(root.join(".git")).unwrap();
    let fixture = include_str!("../../../benchmarks/fixtures/codex-hook-session.jsonl")
        .replace("$PROJECT", root.to_str().unwrap());
    let mut final_output = Vec::new();
    for event in fixture.lines() {
        let mut child = Command::new(env!("CARGO_BIN_EXE_spectra"))
            .arg("hook")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(event.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if !output.stdout.is_empty() {
            final_output = output.stdout;
        }
    }

    let ledger = LedgerStore::open(&root).unwrap();
    assert_eq!(ledger.state(), LedgerState::Complete);
    assert_eq!(ledger.events().len(), 10, "hook retry must be idempotent");
    let projection = ledger.projection();
    assert!(projection.text.contains("edit src/lib.rs,src/recovery.rs"));
    assert!(
        projection
            .text
            .contains("cargo test --workspace success=true")
    );
    assert!(projection.estimated_tokens < 150);

    let output: serde_json::Value = serde_json::from_slice(&final_output).unwrap();
    let context = output["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(context.contains("src/recovery.rs"));
    assert!(context.contains("success=true"));
    assert!(context.chars().count().div_ceil(4) < 200);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn malformed_hook_payloads_fail_open_without_output() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_spectra"))
        .arg("hook")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"not json").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}
