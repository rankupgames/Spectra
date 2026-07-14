mod hermes;
mod json;
mod jsonc;

use std::{
    env,
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

const TARGETS: [Agent; 8] = [
    Agent::Claude,
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
    Cursor,
    Codex,
    OpenCode,
    Hermes,
    Gemini,
    Antigravity,
    Kiro,
}

#[derive(Clone, Copy)]
pub(super) struct JsonTarget {
    pub label: &'static str,
    pub root_key: &'static str,
    pub path: fn() -> Result<PathBuf, BoxError>,
    pub args: &'static [&'static str],
}

const STDIO_ARGS: &[&str] = &["serve", "--mcp"];
const CURSOR_STDIO_ARGS: &[&str] = &["serve", "--mcp", "--path", "${workspaceFolder}"];

pub(crate) fn install(selection: Agent, dry_run: bool) -> Result<Report, BoxError> {
    run_selected(selection, |agent| install_one(agent, dry_run))
}

pub(crate) fn uninstall(selection: Agent, dry_run: bool) -> Result<Report, BoxError> {
    run_selected(selection, |agent| uninstall_one(agent, dry_run))
}

pub(crate) fn status(selection: Agent) -> Result<Report, BoxError> {
    run_selected(selection, status_one)
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

fn install_one(agent: Agent, dry_run: bool) -> Result<String, BoxError> {
    match agent {
        Agent::Codex => Ok(crate::install::install_codex(dry_run)?.to_string()),
        Agent::OpenCode => jsonc::install(dry_run),
        Agent::Hermes => hermes::install(dry_run),
        agent => json::install(json_target(agent)?, dry_run),
    }
}

fn uninstall_one(agent: Agent, dry_run: bool) -> Result<String, BoxError> {
    match agent {
        Agent::Codex => Ok(crate::install::uninstall_codex(dry_run)?.to_string()),
        Agent::OpenCode => jsonc::uninstall(dry_run),
        Agent::Hermes => hermes::uninstall(dry_run),
        agent => json::uninstall(json_target(agent)?, dry_run),
    }
}

fn status_one(agent: Agent) -> Result<String, BoxError> {
    match agent {
        Agent::Codex => crate::install::codex_status(),
        Agent::OpenCode => jsonc::status(),
        Agent::Hermes => hermes::status(),
        agent => json::status(json_target(agent)?),
    }
}

fn json_target(agent: Agent) -> Result<JsonTarget, BoxError> {
    let target = match agent {
        Agent::Claude => JsonTarget {
            label: "Claude Code",
            root_key: "mcpServers",
            path: claude_path,
            args: STDIO_ARGS,
        },
        Agent::Cursor => JsonTarget {
            label: "Cursor",
            root_key: "mcpServers",
            path: cursor_path,
            args: CURSOR_STDIO_ARGS,
        },
        Agent::Gemini => JsonTarget {
            label: "Gemini CLI",
            root_key: "mcpServers",
            path: gemini_path,
            args: STDIO_ARGS,
        },
        Agent::Antigravity => JsonTarget {
            label: "Antigravity",
            root_key: "mcpServers",
            path: antigravity_path,
            args: STDIO_ARGS,
        },
        Agent::Kiro => JsonTarget {
            label: "Kiro",
            root_key: "mcpServers",
            path: kiro_path,
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
        Agent::Cursor => "Cursor",
        Agent::Codex => "Codex",
        Agent::OpenCode => "OpenCode",
        Agent::Hermes => "Hermes Agent",
        Agent::Gemini => "Gemini CLI",
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
        }
        Agent::Claude => {
            command_exists("claude")
                || claude_path().is_ok_and(|path| path.exists())
                || home_path(".claude").is_some_and(|path| path.exists())
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
        Agent::Gemini => command_exists("gemini") || gemini_path().is_ok_and(|path| path.exists()),
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
