mod hermes;
mod hooks;
mod json;
mod jsonc;
mod local;

use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
};

use atomic_write_file::AtomicWriteFile;
use clap::ValueEnum;

pub(crate) type BoxError = Box<dyn std::error::Error>;

pub(crate) struct Report {
    pub messages: Vec<String>,
    pub errors: Vec<String>,
}

const TARGETS: [Agent; 9] = [
    Agent::Claude,
    Agent::ClaudeDesktop,
    Agent::Cursor,
    Agent::Codex,
    Agent::OpenCode,
    Agent::Hermes,
    Agent::Gemini,
    Agent::Antigravity,
    Agent::Kiro,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum Agent {
    /// Detect and configure every installed supported agent.
    Auto,
    /// Configure every supported agent, even when it is not detected.
    All,
    Claude,
    /// Configure the standalone Claude desktop application.
    ClaudeDesktop,
    Cursor,
    #[value(alias = "codex-desktop")]
    Codex,
    OpenCode,
    Hermes,
    #[value(alias = "gemini-desktop", alias = "gemini-code-assist")]
    Gemini,
    Antigravity,
    Kiro,
}

#[derive(Clone)]
pub(super) struct JsonTarget {
    pub label: &'static str,
    pub root_key: &'static str,
    pub path: PathBuf,
    pub args: &'static [&'static str],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum Location {
    Global,
    Local,
}

const STDIO_ARGS: &[&str] = &["serve", "--mcp"];
const CURSOR_STDIO_ARGS: &[&str] = &["serve", "--mcp", "--path", "${workspaceFolder}"];

const CLAUDE_HOOK_EVENTS: &[(&str, Option<&str>)] = &[
    ("SessionStart", Some("startup|resume|clear|compact")),
    ("UserPromptSubmit", None),
    (
        "PermissionRequest",
        Some("Bash|apply_patch|Edit|Write|MultiEdit"),
    ),
    ("PostToolUse", Some("Bash|apply_patch|Edit|Write|MultiEdit")),
    ("Stop", None),
];
const GEMINI_HOOK_EVENTS: &[(&str, Option<&str>)] = &[
    ("SessionStart", None),
    ("BeforeAgent", None),
    ("AfterTool", Some("write_file|replace|run_shell_command")),
    ("AfterAgent", None),
];
const CURSOR_HOOK_EVENTS: &[(&str, Option<&str>)] = &[
    ("sessionStart", None),
    ("afterFileEdit", None),
    ("afterShellExecution", None),
    ("postToolUse", None),
    ("stop", None),
];

pub(crate) fn install(
    selection: Agent,
    dry_run: bool,
    topology_only: bool,
    location: Location,
    project: &Path,
) -> Result<Report, BoxError> {
    let mut report = Report {
        messages: Vec::new(),
        errors: Vec::new(),
    };
    for agent in selected(selection)? {
        match install_one(agent, dry_run, topology_only, location, project) {
            Ok(message) => report.messages.push(format!(
                "{message}\n{}: Capability={}, Tools=brief+map",
                label(agent),
                if topology_only {
                    "topology"
                } else {
                    capability(agent)
                }
            )),
            Err(error) => report.errors.push(format!("{}: {error}", label(agent))),
        }
    }
    Ok(report)
}

pub(crate) fn uninstall(
    selection: Agent,
    dry_run: bool,
    location: Location,
    project: &Path,
) -> Result<Report, BoxError> {
    run_selected(selection, |agent| {
        uninstall_one(agent, dry_run, location, project)
    })
}

pub(crate) fn status(selection: Agent) -> Result<Report, BoxError> {
    run_selected(selection, |agent| {
        status_one(agent).map(|message| format!("{message}, Tools=brief+map"))
    })
}

pub(crate) fn status_detected() -> Report {
    let mut report = Report {
        messages: Vec::new(),
        errors: Vec::new(),
    };
    for agent in TARGETS.into_iter().filter(|agent| detected(*agent)) {
        match status_one(agent) {
            Ok(message) => report.messages.push(format!("{message}, Tools=brief+map")),
            Err(error) => report.errors.push(format!("{}: {error}", label(agent))),
        }
    }
    if report.messages.is_empty() && report.errors.is_empty() {
        report.messages.push("No supported agents detected; topology remains available through manual MCP configuration.".into());
    }
    report
}

fn run_selected(
    selection: Agent,
    operation: impl Fn(Agent) -> Result<String, BoxError>,
) -> Result<Report, BoxError> {
    let mut report = Report {
        messages: Vec::new(),
        errors: Vec::new(),
    };
    for agent in selected(selection)? {
        match operation(agent) {
            Ok(message) => report.messages.push(message),
            Err(error) => report.errors.push(format!("{}: {error}", label(agent))),
        }
    }
    Ok(report)
}

fn selected(selection: Agent) -> Result<Vec<Agent>, BoxError> {
    match selection {
        Agent::Auto => {
            let detected: Vec<_> = TARGETS
                .into_iter()
                .filter(|agent| detected(*agent))
                .collect();
            if detected.is_empty() {
                Err("no supported local coding agents were detected; use --agent <name> for one target or --agent all".into())
            } else {
                Ok(detected)
            }
        }
        Agent::All => Ok(TARGETS.to_vec()),
        agent => Ok(vec![agent]),
    }
}

fn install_one(
    agent: Agent,
    dry_run: bool,
    topology_only: bool,
    location: Location,
    project: &Path,
) -> Result<String, BoxError> {
    if location == Location::Local
        && !matches!(
            agent,
            Agent::Codex | Agent::Claude | Agent::Cursor | Agent::Gemini
        )
    {
        return Err(format!(
            "{} does not have a verified project-local installer",
            label(agent)
        )
        .into());
    }
    match agent {
        Agent::Codex if location == Location::Local => {
            local::install_codex(project, dry_run, topology_only)
        }
        Agent::Codex if topology_only && codex_cli_available() => {
            Ok(crate::install::install_codex_topology_only(dry_run)?.to_string())
        }
        Agent::Codex if codex_cli_available() => {
            Ok(crate::install::install_codex(dry_run)?.to_string())
        }
        Agent::Codex => local::install_codex_global(dry_run, topology_only),
        Agent::OpenCode => jsonc::install(dry_run),
        Agent::Hermes => hermes::install(dry_run),
        agent => {
            let hook = (!topology_only && has_ledger(agent))
                .then(|| hook_target(agent, location, project))
                .transpose()?;
            if let Some(hook) = &hook {
                hooks::install(hook, true)?;
            }
            let target = json_target(agent, location, project)?;
            let snapshot = (!dry_run)
                .then(|| ConfigSnapshot::capture(&target.path))
                .transpose()?;
            let mut message = json::install(target, dry_run)?;
            if let Some(hook) = &hook {
                message.push('\n');
                match hooks::install(hook, dry_run) {
                    Ok(hook_message) => message.push_str(&hook_message),
                    Err(error) => {
                        if let Some(snapshot) = snapshot {
                            snapshot.restore().map_err(|rollback| {
                                format!("{error}; additionally failed to roll back MCP configuration: {rollback}")
                            })?;
                        }
                        return Err(error);
                    }
                }
            }
            Ok(message)
        }
    }
}

struct ConfigSnapshot {
    path: PathBuf,
    contents: Option<Vec<u8>>,
}

impl ConfigSnapshot {
    fn capture(path: &Path) -> Result<Self, BoxError> {
        Ok(Self {
            path: path.to_path_buf(),
            contents: path.exists().then(|| fs::read(path)).transpose()?,
        })
    }

    fn restore(self) -> Result<(), BoxError> {
        if let Some(contents) = self.contents {
            write_atomic(&self.path, &contents)
        } else if self.path.exists() {
            fs::remove_file(&self.path)?;
            Ok(())
        } else {
            Ok(())
        }
    }
}

fn uninstall_one(
    agent: Agent,
    dry_run: bool,
    location: Location,
    project: &Path,
) -> Result<String, BoxError> {
    if location == Location::Local
        && !matches!(
            agent,
            Agent::Codex | Agent::Claude | Agent::Cursor | Agent::Gemini
        )
    {
        return Err(format!(
            "{} does not have a verified project-local installer",
            label(agent)
        )
        .into());
    }
    match agent {
        Agent::Codex if location == Location::Local => local::uninstall_codex(project, dry_run),
        Agent::Codex if codex_cli_available() => {
            Ok(crate::install::uninstall_codex(dry_run)?.to_string())
        }
        Agent::Codex => local::uninstall_codex_global(dry_run),
        Agent::OpenCode => jsonc::uninstall(dry_run),
        Agent::Hermes => hermes::uninstall(dry_run),
        agent => {
            let hook = has_ledger(agent)
                .then(|| hook_target(agent, location, project))
                .transpose()?;
            if let Some(hook) = &hook {
                hooks::uninstall(hook, true)?;
            }
            let target = json_target(agent, location, project)?;
            let snapshot = (!dry_run)
                .then(|| ConfigSnapshot::capture(&target.path))
                .transpose()?;
            let mut message = json::uninstall(target, dry_run)?;
            if let Some(hook) = &hook {
                message.push('\n');
                match hooks::uninstall(hook, dry_run) {
                    Ok(hook_message) => message.push_str(&hook_message),
                    Err(error) => {
                        if let Some(snapshot) = snapshot {
                            snapshot.restore().map_err(|rollback| {
                                format!("{error}; additionally failed to roll back MCP configuration: {rollback}")
                            })?;
                        }
                        return Err(error);
                    }
                }
            }
            Ok(message)
        }
    }
}

fn status_one(agent: Agent) -> Result<String, BoxError> {
    match agent {
        Agent::Codex if codex_cli_available() => crate::install::codex_status(),
        Agent::Codex => local::codex_global_status(),
        Agent::OpenCode => jsonc::status(),
        Agent::Hermes => hermes::status(),
        agent => {
            let mut message = json::status(json_target(agent, Location::Global, Path::new("."))?)?;
            if has_ledger(agent) {
                let status = hooks::status(&hook_target(agent, Location::Global, Path::new("."))?)?;
                message = format!(
                    "{}: MCP={}, Ledger hooks={}, Capability={}",
                    label(agent),
                    message
                        .split("MCP=")
                        .nth(1)
                        .and_then(|v| v.split(',').next())
                        .unwrap_or("unknown"),
                    status,
                    capability(agent)
                );
            }
            Ok(message)
        }
    }
}

fn has_ledger(agent: Agent) -> bool {
    matches!(
        agent,
        Agent::Codex | Agent::Claude | Agent::Gemini | Agent::Cursor
    )
}

pub(crate) fn capability(agent: Agent) -> &'static str {
    match agent {
        Agent::Cursor => "topology+ledger-partial",
        Agent::Codex | Agent::Claude | Agent::Gemini => "topology+ledger",
        _ => "topology",
    }
}

pub(crate) fn detected_summaries() -> Vec<String> {
    TARGETS
        .into_iter()
        .filter(|agent| detected(*agent))
        .map(|agent| format!("{} — {}", label(agent), capability(agent)))
        .collect()
}

fn hook_target(
    agent: Agent,
    location: Location,
    project: &Path,
) -> Result<hooks::HookTarget, BoxError> {
    let target = match agent {
        Agent::Claude => hooks::HookTarget {
            label: "Claude Code",
            agent: "claude",
            path: if location == Location::Local {
                project.join(".claude/settings.json")
            } else {
                companion_path(
                    "SPECTRA_CLAUDE_SETTINGS",
                    "SPECTRA_CLAUDE_CONFIG",
                    ".claude/settings.json",
                    "claude-hooks.json",
                )?
            },
            schema: hooks::HookSchema::Grouped,
            events: CLAUDE_HOOK_EVENTS,
        },
        Agent::Gemini => hooks::HookTarget {
            label: "Gemini CLI/Code Assist (VS Code)",
            agent: "gemini",
            path: if location == Location::Local {
                project.join(".gemini/settings.json")
            } else {
                gemini_path()?
            },
            schema: hooks::HookSchema::Grouped,
            events: GEMINI_HOOK_EVENTS,
        },
        Agent::Cursor => hooks::HookTarget {
            label: "Cursor",
            agent: "cursor",
            path: if location == Location::Local {
                project.join(".cursor/hooks.json")
            } else {
                companion_path(
                    "SPECTRA_CURSOR_HOOKS",
                    "SPECTRA_CURSOR_CONFIG",
                    ".cursor/hooks.json",
                    "cursor-hooks.json",
                )?
            },
            schema: hooks::HookSchema::Cursor,
            events: CURSOR_HOOK_EVENTS,
        },
        _ => return Err("agent does not provide external Ledger hook configuration".into()),
    };
    Ok(target)
}

fn json_target(agent: Agent, location: Location, project: &Path) -> Result<JsonTarget, BoxError> {
    let target = match agent {
        Agent::Claude => JsonTarget {
            label: "Claude Code",
            root_key: "mcpServers",
            path: if location == Location::Local {
                project.join(".mcp.json")
            } else {
                claude_path()?
            },
            args: STDIO_ARGS,
        },
        Agent::ClaudeDesktop => JsonTarget {
            label: "Claude Desktop",
            root_key: "mcpServers",
            path: claude_desktop_path()?,
            args: STDIO_ARGS,
        },
        Agent::Cursor => JsonTarget {
            label: "Cursor",
            root_key: "mcpServers",
            path: if location == Location::Local {
                project.join(".cursor/mcp.json")
            } else {
                cursor_path()?
            },
            args: if location == Location::Local {
                STDIO_ARGS
            } else {
                CURSOR_STDIO_ARGS
            },
        },
        Agent::Gemini => JsonTarget {
            label: "Gemini CLI/Code Assist (VS Code)",
            root_key: "mcpServers",
            path: if location == Location::Local {
                project.join(".gemini/settings.json")
            } else {
                gemini_path()?
            },
            args: STDIO_ARGS,
        },
        Agent::Antigravity => JsonTarget {
            label: "Antigravity",
            root_key: "mcpServers",
            path: antigravity_path()?,
            args: STDIO_ARGS,
        },
        Agent::Kiro => JsonTarget {
            label: "Kiro",
            root_key: "mcpServers",
            path: kiro_path()?,
            args: STDIO_ARGS,
        },
        _ => return Err("agent does not use the standard JSON adapter".into()),
    };
    Ok(target)
}

fn label(agent: Agent) -> &'static str {
    match agent {
        Agent::Auto => "Automatic detection",
        Agent::All => "All agents",
        Agent::Claude => "Claude Code",
        Agent::ClaudeDesktop => "Claude Desktop",
        Agent::Cursor => "Cursor",
        Agent::Codex => "Codex CLI/Desktop",
        Agent::OpenCode => "OpenCode",
        Agent::Hermes => "Hermes Agent",
        Agent::Gemini => "Gemini CLI/Code Assist (VS Code)",
        Agent::Antigravity => "Antigravity",
        Agent::Kiro => "Kiro",
    }
}

fn detected(agent: Agent) -> bool {
    match agent {
        Agent::Codex => {
            env::var_os("SPECTRA_CODEX_BIN").is_some()
                || command_exists("codex")
                || home_path(".codex").is_some_and(|path| path.exists())
                || desktop_app_exists("Codex")
        }
        Agent::Claude => {
            command_exists("claude")
                || claude_path().is_ok_and(|path| path.exists())
                || home_path(".claude").is_some_and(|path| path.exists())
        }
        Agent::ClaudeDesktop => {
            env::var_os("SPECTRA_CLAUDE_DESKTOP_CONFIG").is_some()
                || claude_desktop_path().is_ok_and(|path| path.exists())
                || desktop_app_exists("Claude")
        }
        Agent::Cursor => {
            command_exists("cursor")
                || command_exists("cursor-agent")
                || cursor_path().is_ok_and(|path| path.exists())
                || home_path(".cursor").is_some_and(|path| path.exists())
        }
        Agent::OpenCode => {
            command_exists("opencode") || jsonc::path().is_ok_and(|path| path.exists())
        }
        Agent::Hermes => command_exists("hermes") || hermes::path().is_ok_and(|path| path.exists()),
        Agent::Gemini => {
            command_exists("gemini")
                || gemini_path().is_ok_and(|path| path.exists())
                || gemini_desktop_detected()
        }
        Agent::Antigravity => {
            command_exists("agy")
                || command_exists("antigravity")
                || antigravity_path().is_ok_and(|path| path.exists())
        }
        Agent::Kiro => {
            command_exists("kiro-cli")
                || command_exists("kiro")
                || kiro_path().is_ok_and(|path| path.exists())
                || home_path(".kiro").is_some_and(|path| path.exists())
        }
        Agent::Auto | Agent::All => false,
    }
}

pub(super) fn spectra_executable() -> Result<PathBuf, BoxError> {
    Ok(env::current_exe()?.canonicalize()?)
}

pub(super) fn configured_command_is_spectra(command: &Path) -> bool {
    command
        .file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem.eq_ignore_ascii_case("spectra"))
}

pub(super) fn resolved(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

pub(super) fn write_atomic(path: &Path, contents: &[u8]) -> Result<(), BoxError> {
    let parent = path
        .parent()
        .ok_or("configuration path has no parent directory")?;
    std::fs::create_dir_all(parent)?;
    let destination = if path.is_symlink() {
        path.canonicalize()?
    } else {
        path.to_path_buf()
    };
    let mut file = AtomicWriteFile::open(destination)?;
    file.write_all(contents)?;
    file.commit()?;
    Ok(())
}

fn configured_path(variable: &str, relative: &str) -> Result<PathBuf, BoxError> {
    if let Some(path) = env::var_os(variable) {
        Ok(PathBuf::from(path))
    } else {
        home_path(relative).ok_or_else(|| "unable to locate the user home directory".into())
    }
}

fn companion_path(
    direct_variable: &str,
    config_variable: &str,
    fallback: &str,
    sibling: &str,
) -> Result<PathBuf, BoxError> {
    if let Some(path) = env::var_os(direct_variable) {
        return Ok(PathBuf::from(path));
    }
    if let Some(config) = env::var_os(config_variable) {
        let config = PathBuf::from(config);
        return Ok(config.parent().unwrap_or(Path::new(".")).join(sibling));
    }
    configured_path(direct_variable, fallback)
}

fn home_path(relative: &str) -> Option<PathBuf> {
    env::var_os("SPECTRA_HOME")
        .or_else(|| env::var_os("HOME"))
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .map(|home| home.join(relative))
}

fn claude_path() -> Result<PathBuf, BoxError> {
    configured_path("SPECTRA_CLAUDE_CONFIG", ".claude.json")
}

fn claude_desktop_path() -> Result<PathBuf, BoxError> {
    if let Some(path) = env::var_os("SPECTRA_CLAUDE_DESKTOP_CONFIG") {
        return Ok(PathBuf::from(path));
    }
    #[cfg(target_os = "macos")]
    return home_path("Library/Application Support/Claude/claude_desktop_config.json")
        .ok_or_else(|| "unable to locate the user home directory".into());
    #[cfg(windows)]
    return env::var_os("APPDATA")
        .map(PathBuf::from)
        .map(|path| path.join("Claude/claude_desktop_config.json"))
        .ok_or_else(|| "unable to locate the roaming application data directory".into());
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let base = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| home_path(".config"))
            .ok_or("unable to locate the user configuration directory")?;
        Ok(base.join("Claude/claude_desktop_config.json"))
    }
}

fn cursor_path() -> Result<PathBuf, BoxError> {
    configured_path("SPECTRA_CURSOR_CONFIG", ".cursor/mcp.json")
}

fn gemini_path() -> Result<PathBuf, BoxError> {
    configured_path("SPECTRA_GEMINI_CONFIG", ".gemini/settings.json")
}

fn antigravity_path() -> Result<PathBuf, BoxError> {
    configured_path(
        "SPECTRA_ANTIGRAVITY_CONFIG",
        ".gemini/config/mcp_config.json",
    )
}

fn kiro_path() -> Result<PathBuf, BoxError> {
    configured_path("SPECTRA_KIRO_CONFIG", ".kiro/settings/mcp.json")
}

fn command_exists(command: &str) -> bool {
    let Some(path) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&path).any(|directory| {
        let candidate = directory.join(command);
        if candidate.is_file() {
            return true;
        }
        #[cfg(windows)]
        {
            ["exe", "cmd", "bat"]
                .into_iter()
                .any(|extension| directory.join(format!("{command}.{extension}")).is_file())
        }
        #[cfg(not(windows))]
        false
    })
}

fn codex_cli_available() -> bool {
    env::var_os("SPECTRA_CODEX_BIN").is_some() || command_exists("codex")
}

fn desktop_app_exists(name: &str) -> bool {
    if env::var_os(format!("SPECTRA_{}_DESKTOP", name.to_ascii_uppercase())).is_some() {
        return true;
    }
    #[cfg(target_os = "macos")]
    {
        Path::new("/Applications")
            .join(format!("{name}.app"))
            .exists()
            || home_path("Applications")
                .is_some_and(|path| path.join(format!("{name}.app")).exists())
    }
    #[cfg(not(target_os = "macos"))]
    {
        #[cfg(windows)]
        {
            let executable = format!("{name}.exe");
            return env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .is_some_and(|path| {
                    path.join("Programs").join(name).join(&executable).exists()
                        || path.join(name).join(&executable).exists()
                });
        }
        #[cfg(not(windows))]
        {
            let desktop_file = format!("{}.desktop", name.to_ascii_lowercase());
            Path::new("/usr/share/applications")
                .join(&desktop_file)
                .exists()
                || home_path(".local/share/applications")
                    .is_some_and(|path| path.join(desktop_file).exists())
        }
    }
}

fn gemini_desktop_detected() -> bool {
    if env::var_os("SPECTRA_GEMINI_DESKTOP").is_some() {
        return true;
    }
    home_path(".vscode/extensions").is_some_and(|path| {
        fs::read_dir(path).is_ok_and(|entries| {
            entries.filter_map(Result::ok).any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("google.geminicodeassist")
            })
        })
    })
}
