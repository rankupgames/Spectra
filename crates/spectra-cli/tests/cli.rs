use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use spectra_core::{LedgerState, LedgerStore};

#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn fixture() -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "spectra-cli-test-{}-{timestamp}-{sequence}",
        std::process::id()
    ));
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
    assert!(stdout.contains("N1=function entry @ src/lib.rs:"));
    assert!(stdout.contains("flow N1 -calls-> N2"));
    assert!(root.join(".spectra/index-v4.json").is_file());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn concurrent_sync_processes_serialize_index_and_ledger_writes() {
    let root = fixture();
    for index in 0..200 {
        fs::write(
            root.join(format!("src/module_{index}.rs")),
            format!("pub fn function_{index}() {{}}\n"),
        )
        .unwrap();
    }
    let binary = env!("CARGO_BIN_EXE_spectra");
    let mut first = Command::new(binary)
        .args(["sync", "--quiet", root.to_str().unwrap()])
        .spawn()
        .unwrap();
    let mut second = Command::new(binary)
        .args(["sync", "--quiet", root.to_str().unwrap()])
        .spawn()
        .unwrap();
    assert!(first.wait().unwrap().success());
    assert!(second.wait().unwrap().success());

    let index: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join(".spectra/index-v4.json")).unwrap()).unwrap();
    assert_eq!(index["files"].as_object().unwrap().len(), 201);
    let events = fs::read_to_string(root.join(".spectra/ledger-v1.jsonl")).unwrap();
    assert_eq!(events.lines().count(), 1);
    assert!(!root.join(".spectra/index-v4.lock").exists());
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn git_autosync_fallback_executes_a_quiet_background_sync() {
    let root = fixture();
    assert!(
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&root)
            .status()
            .unwrap()
            .success()
    );
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_spectra"));
    let installed = Command::new(&binary)
        .args(["autosync", "install", root.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    let status = Command::new(&binary)
        .args(["autosync", "status", root.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&status.stdout).contains("fallback=current"));

    let mut paths = vec![binary.parent().unwrap().to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let hook = root.join(".git/hooks/post-checkout");
    assert!(
        Command::new(&hook)
            .current_dir(&root)
            .env("PATH", std::env::join_paths(paths).unwrap())
            .status()
            .unwrap()
            .success()
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !root.join(".spectra/index-v4.json").exists()
        || fs::read_to_string(root.join(".spectra/ledger-v1.jsonl"))
            .map(|ledger| !ledger.ends_with('\n'))
            .unwrap_or(true)
    {
        assert!(
            Instant::now() < deadline,
            "Git fallback did not sync in time"
        );
        thread::sleep(Duration::from_millis(25));
    }

    let removed = Command::new(&binary)
        .args(["autosync", "remove", root.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(removed.status.success());
    assert!(!hook.exists());
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

    let preview = run(&["install", "--agent", "codex", "--dry-run"]);
    assert!(preview.status.success());
    assert!(String::from_utf8_lossy(&preview.stdout).contains("Would configure"));
    assert!(!state.exists());
    assert!(!codex_home.join("hooks.json").exists());

    let first = run(&["install", "--agent", "codex"]);
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

    let second = run(&["install", "--agent", "codex"]);
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(String::from_utf8_lossy(&second.stdout).contains("already configured"));

    let status = run(&["status", "--agent", "codex"]);
    assert!(status.status.success());
    assert_eq!(
        String::from_utf8_lossy(&status.stdout).trim(),
        "Codex: MCP=current, Ledger hooks=current"
    );

    let removed = run(&["uninstall", "--agent", "codex"]);
    assert!(
        removed.status.success(),
        "{}",
        String::from_utf8_lossy(&removed.stderr)
    );
    assert!(String::from_utf8_lossy(&removed.stdout).contains("Removed"));
    assert_eq!(
        String::from_utf8_lossy(&run(&["status", "--agent", "codex"]).stdout).trim(),
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
    let conflict = run(&["install", "--agent", "codex"]);
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
fn standard_json_agent_installers_are_owned_idempotent_and_reversible() {
    let root = fixture();
    let binary = env!("CARGO_BIN_EXE_spectra");
    let targets = [
        ("claude", "SPECTRA_CLAUDE_CONFIG"),
        ("cursor", "SPECTRA_CURSOR_CONFIG"),
        ("gemini", "SPECTRA_GEMINI_CONFIG"),
        ("antigravity", "SPECTRA_ANTIGRAVITY_CONFIG"),
        ("kiro", "SPECTRA_KIRO_CONFIG"),
    ];

    for (agent, variable) in targets {
        let path = root.join(format!("{agent}.json"));
        fs::write(
            &path,
            r#"{
  "unrelated": true,
  "mcpServers": {
    "other": {"command": "other", "args": []}
  }
}
"#,
        )
        .unwrap();
        let run = |command: &str| {
            Command::new(binary)
                .args([command, "--agent", agent])
                .env(variable, &path)
                .output()
                .unwrap()
        };

        let installed = run("install");
        assert!(
            installed.status.success(),
            "{agent}: {}",
            String::from_utf8_lossy(&installed.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["unrelated"], true);
        assert_eq!(value["mcpServers"]["other"]["command"], "other");
        assert_eq!(
            value["mcpServers"]["spectra"]["args"],
            serde_json::json!(["serve", "--mcp"])
        );

        let second = run("install");
        assert!(second.status.success());
        assert!(String::from_utf8_lossy(&second.stdout).contains("already configured"));
        let status = run("status");
        assert!(status.status.success());
        assert!(String::from_utf8_lossy(&status.stdout).contains("MCP=current"));

        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value["mcpServers"]["spectra"]["command"] = "/old/location/spectra".into();
        fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        let updated = run("install");
        assert!(updated.status.success());
        assert!(String::from_utf8_lossy(&updated.stdout).contains("updated"));
        assert!(String::from_utf8_lossy(&run("status").stdout).contains("MCP=current"));

        let removed = run("uninstall");
        assert!(removed.status.success());
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(value["mcpServers"].get("spectra").is_none());
        assert_eq!(value["mcpServers"]["other"]["command"], "other");
    }

    let conflict = root.join("foreign.json");
    let original = r#"{"mcpServers":{"spectra":{"command":"other","args":[]}}}"#;
    fs::write(&conflict, original).unwrap();
    let output = Command::new(binary)
        .args(["install", "--agent", "claude"])
        .env("SPECTRA_CLAUDE_CONFIG", &conflict)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("refusing to overwrite"));
    assert_eq!(fs::read_to_string(conflict).unwrap(), original);

    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn installer_preserves_symlinked_configuration_files() {
    let root = fixture();
    let real = root.join("real-claude.json");
    let linked = root.join("linked-claude.json");
    fs::write(&real, "{}\n").unwrap();
    symlink(&real, &linked).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_spectra"))
        .args(["install", "--agent", "claude"])
        .env("SPECTRA_CLAUDE_CONFIG", &linked)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(linked.is_symlink());
    let value: serde_json::Value = serde_json::from_slice(&fs::read(real).unwrap()).unwrap();
    assert!(value["mcpServers"]["spectra"].is_object());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn opencode_jsonc_preserves_comments_and_unrelated_servers() {
    let root = fixture();
    let path = root.join("opencode.json");
    fs::write(
        &path,
        r#"{
  // this comment belongs to the user
  "theme": "system",
  "mcp": {
    "other": { "type": "local", "command": ["other"] },
  },
}
"#,
    )
    .unwrap();
    let run = |command: &str| {
        Command::new(env!("CARGO_BIN_EXE_spectra"))
            .args([command, "--agent", "open-code"])
            .env("SPECTRA_OPENCODE_CONFIG", &path)
            .output()
            .unwrap()
    };

    let before = fs::read_to_string(&path).unwrap();
    let preview = Command::new(env!("CARGO_BIN_EXE_spectra"))
        .args(["install", "--agent", "open-code", "--dry-run"])
        .env("SPECTRA_OPENCODE_CONFIG", &path)
        .output()
        .unwrap();
    assert!(preview.status.success());
    assert_eq!(fs::read_to_string(&path).unwrap(), before);

    let installed = run("install");
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    let configured = fs::read_to_string(&path).unwrap();
    assert!(configured.contains("// this comment belongs to the user"));
    assert!(configured.contains("\"theme\": \"system\""));
    assert!(configured.contains("\"other\""));
    assert!(configured.contains("\"spectra\""));
    assert!(String::from_utf8_lossy(&run("status").stdout).contains("MCP=current"));

    let executable = fs::canonicalize(env!("CARGO_BIN_EXE_spectra")).unwrap();
    let current_command = serde_json::to_string(executable.to_str().unwrap()).unwrap();
    let stale = configured.replace(&current_command, r#""/old/location/spectra""#);
    assert_ne!(
        stale, configured,
        "fixture must contain the current command"
    );
    fs::write(&path, stale).unwrap();
    let updated = run("install");
    assert!(updated.status.success());
    assert!(String::from_utf8_lossy(&updated.stdout).contains("updated"));

    let removed = run("uninstall");
    assert!(removed.status.success());
    let configured = fs::read_to_string(&path).unwrap();
    assert!(configured.contains("// this comment belongs to the user"));
    assert!(configured.contains("\"other\""));
    assert!(!configured.contains("\"spectra\""));

    let foreign = r#"{
  // preserve me
  "mcp": {
    "spectra": { "type": "local", "command": ["other"] }
  }
}
"#;
    fs::write(&path, foreign).unwrap();
    let conflict = run("install");
    assert!(!conflict.status.success());
    assert!(String::from_utf8_lossy(&conflict.stderr).contains("refusing to overwrite"));
    assert_eq!(fs::read_to_string(&path).unwrap(), foreign);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn hermes_yaml_preserves_unrelated_configuration() {
    let root = fixture();
    let path = root.join("config.yaml");
    fs::write(
        &path,
        "# user configuration\nmodel: test\nmcp_servers:\n  other:\n    command: other\n    args: []\n",
    )
    .unwrap();
    let run = |command: &str| {
        Command::new(env!("CARGO_BIN_EXE_spectra"))
            .args([command, "--agent", "hermes"])
            .env("SPECTRA_HERMES_CONFIG", &path)
            .output()
            .unwrap()
    };

    let before = fs::read_to_string(&path).unwrap();
    let preview = Command::new(env!("CARGO_BIN_EXE_spectra"))
        .args(["install", "--agent", "hermes", "--dry-run"])
        .env("SPECTRA_HERMES_CONFIG", &path)
        .output()
        .unwrap();
    assert!(preview.status.success());
    assert_eq!(fs::read_to_string(&path).unwrap(), before);

    let installed = run("install");
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    let configured = fs::read_to_string(&path).unwrap();
    assert!(configured.contains("# user configuration"));
    assert!(configured.contains("  other:"));
    assert!(configured.contains("  spectra:"));
    assert!(String::from_utf8_lossy(&run("status").stdout).contains("MCP=current"));

    let executable = fs::canonicalize(env!("CARGO_BIN_EXE_spectra")).unwrap();
    let current_command = serde_json::to_string(executable.to_str().unwrap()).unwrap();
    let stale = configured.replace(&current_command, r#""/old/location/spectra""#);
    assert_ne!(
        stale, configured,
        "fixture must contain the current command"
    );
    fs::write(&path, stale).unwrap();
    let updated = run("install");
    assert!(updated.status.success());
    assert!(String::from_utf8_lossy(&updated.stdout).contains("updated"));

    let removed = run("uninstall");
    assert!(removed.status.success());
    let configured = fs::read_to_string(&path).unwrap();
    assert!(configured.contains("  other:"));
    assert!(!configured.contains("  spectra:"));

    let foreign = "mcp_servers:\n  spectra:\n    command: other\n    args: []\n";
    fs::write(&path, foreign).unwrap();
    let conflict = run("install");
    assert!(!conflict.status.success());
    assert!(String::from_utf8_lossy(&conflict.stderr).contains("refusing to overwrite"));
    assert_eq!(fs::read_to_string(&path).unwrap(), foreign);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn malformed_agent_configs_are_never_overwritten() {
    let root = fixture();
    let cases = [
        (
            "claude",
            "SPECTRA_CLAUDE_CONFIG",
            root.join("claude.json"),
            r#"{"mcpServers": []}"#,
        ),
        (
            "open-code",
            "SPECTRA_OPENCODE_CONFIG",
            root.join("opencode.json"),
            r#"{"mcp": []}"#,
        ),
        (
            "hermes",
            "SPECTRA_HERMES_CONFIG",
            root.join("hermes.yaml"),
            "mcp_servers: []\n",
        ),
    ];

    for (agent, variable, path, contents) in cases {
        fs::write(&path, contents).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_spectra"))
            .args(["install", "--agent", agent])
            .env(variable, &path)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{agent} unexpectedly succeeded");
        assert_eq!(fs::read_to_string(path).unwrap(), contents);
    }

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn auto_configures_every_detected_agent_without_host_leakage() {
    let root = fixture();
    let claude = root.join("claude.json");
    let cursor = root.join("cursor.json");
    fs::write(&claude, "{}\n").unwrap();
    fs::write(&cursor, "{}\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_spectra"))
        .arg("install")
        .env("PATH", &root)
        .env("SPECTRA_HOME", &root)
        .env("SPECTRA_CLAUDE_CONFIG", &claude)
        .env("SPECTRA_CURSOR_CONFIG", &cursor)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Claude Code:"));
    assert!(stdout.contains("Cursor:"));
    assert!(!stdout.contains("Codex:"));
    assert!(serde_json::from_slice::<serde_json::Value>(&fs::read(claude).unwrap())
        .unwrap()["mcpServers"]["spectra"]
        .is_object());
    assert!(serde_json::from_slice::<serde_json::Value>(&fs::read(cursor).unwrap())
        .unwrap()["mcpServers"]["spectra"]
        .is_object());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn auto_reports_a_conflict_without_skipping_other_detected_agents() {
    let root = fixture();
    let claude = root.join("claude.json");
    let cursor = root.join("cursor.json");
    fs::write(
        &claude,
        r#"{"mcpServers":{"spectra":{"command":"other","args":[]}}}"#,
    )
    .unwrap();
    fs::write(&cursor, "{}\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_spectra"))
        .arg("install")
        .env("PATH", &root)
        .env("SPECTRA_HOME", &root)
        .env("SPECTRA_CLAUDE_CONFIG", &claude)
        .env("SPECTRA_CURSOR_CONFIG", &cursor)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Cursor:"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Claude Code:"));
    assert!(serde_json::from_slice::<serde_json::Value>(&fs::read(cursor).unwrap())
        .unwrap()["mcpServers"]["spectra"]
        .is_object());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn backtests_a_recorded_codex_hook_session_without_fact_loss() {
    let root = fixture();
    fs::create_dir_all(root.join(".git")).unwrap();
    let fixture = include_str!("../../../benchmarks/fixtures/codex-hook-session.jsonl");
    let mut final_output = Vec::new();
    for event in fixture.lines() {
        let mut event: serde_json::Value = serde_json::from_str(event).unwrap();
        event["cwd"] = root.to_str().unwrap().into();
        let event = serde_json::to_vec(&event).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_spectra"))
            .arg("hook")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(&event).unwrap();
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
