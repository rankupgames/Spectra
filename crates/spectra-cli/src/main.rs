mod agents;
mod autosync;
mod git_sync;
mod hook;
mod install;
mod mcp;
mod mcp_query;

use std::{
    path::{Path, PathBuf},
    process::ExitCode,
};

use agents::Agent;
use clap::{Parser, Subcommand};
use spectra_core::{IndexReport, map_project, sync_project};

#[derive(Debug, Parser)]
#[command(
    name = "spectra",
    version,
    about = "Multimodal code topology for local AI agents"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Configure Spectra for detected local coding agents.
    Install {
        #[arg(long, value_enum, default_value_t = Agent::Auto)]
        agent: Agent,
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove only configuration entries owned by Spectra.
    Uninstall {
        #[arg(long, value_enum, default_value_t = Agent::Auto)]
        agent: Agent,
        #[arg(long)]
        dry_run: bool,
    },
    /// Show whether Spectra is configured for a local coding agent.
    Status {
        #[arg(long, value_enum, default_value_t = Agent::Auto)]
        agent: Agent,
    },
    /// Internal Codex lifecycle adapter. Reads one hook event from stdin.
    #[command(hide = true)]
    Hook,
    /// Build or refresh the local polyglot topology index.
    Init {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Reconcile the project index, including for Git hook fallback use.
    Sync {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(short, long)]
        quiet: bool,
    },
    /// Manage Git-based synchronization fallback for a project.
    Autosync {
        #[command(subcommand)]
        command: AutosyncCommand,
    },
    /// Render a query-focused PNG and SVG topology map.
    Map {
        query: String,
        #[arg(long, default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value_t = 48, value_parser = clap::value_parser!(u16).range(1..=96))]
        max_nodes: u16,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Run Spectra's MCP server over stdio.
    Serve {
        #[arg(long)]
        mcp: bool,
    },
}

#[derive(Debug, Subcommand)]
enum AutosyncCommand {
    /// Install ownership-safe post-commit, post-merge, and post-checkout hooks.
    Install {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Remove only the Git hook blocks owned by Spectra.
    Remove {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Show whether all Git fallback hooks are installed.
    Status {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("spectra: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    match cli.command {
        Command::Install { agent, dry_run } => {
            print_agent_report(agents::install(agent, dry_run)?)?;
        }
        Command::Uninstall { agent, dry_run } => {
            print_agent_report(agents::uninstall(agent, dry_run)?)?;
        }
        Command::Status { agent } => {
            print_agent_report(agents::status(agent)?)?;
        }
        Command::Hook => hook::run_stdin(),
        Command::Init { path } => {
            let (_, report) = sync_project(&path)?;
            print_index_report(&report);
        }
        Command::Sync { path, quiet } => {
            let (_, report) = sync_project(&path)?;
            if !quiet {
                print_index_report(&report);
            }
        }
        Command::Autosync {
            command: AutosyncCommand::Install { path },
        } => {
            let report = git_sync::install(&path)?;
            println!(
                "Installed Spectra autosync fallback in {} ({})",
                report.hooks_dir.display(),
                report.hooks.join(", ")
            );
        }
        Command::Autosync {
            command: AutosyncCommand::Remove { path },
        } => {
            let report = git_sync::remove(&path)?;
            println!(
                "Removed {} Spectra autosync hook(s) from {}",
                report.hooks.len(),
                report.hooks_dir.display()
            );
        }
        Command::Autosync {
            command: AutosyncCommand::Status { path },
        } => {
            let (status, hooks_dir) = git_sync::status(&path)?;
            println!(
                "Git autosync fallback={} ({})",
                status.label(),
                hooks_dir.display()
            );
        }
        Command::Map {
            query,
            path,
            max_nodes,
            out,
        } => {
            let output = out.unwrap_or_else(|| path.join(".spectra/artifacts"));
            let artifact = map_project(&path, &query, usize::from(max_nodes), &output)?;
            println!("PNG {}", display_relative(&artifact.png_path, &path));
            println!("SVG {}", display_relative(&artifact.svg_path, &path));
            print_anchors(&artifact);
        }
        Command::Serve { mcp: true } => mcp::serve().await?,
        Command::Serve { mcp: false } => return Err("serve currently requires --mcp".into()),
    }
    Ok(())
}

fn print_index_report(report: &IndexReport) {
    println!(
        "Indexed {} source files ({} changed, {} removed): {} nodes, {} edges",
        report.files, report.changed, report.removed, report.nodes, report.edges
    );
}

fn print_agent_report(report: agents::Report) -> Result<(), Box<dyn std::error::Error>> {
    for message in report.messages {
        println!("{message}");
    }
    if report.errors.is_empty() {
        Ok(())
    } else {
        Err(report.errors.join("\n").into())
    }
}

fn print_anchors(artifact: &spectra_core::MapArtifact) {
    println!("{}", artifact.compact_metadata());
}

fn display_relative<'a>(path: &'a Path, project: &'a Path) -> String {
    path.strip_prefix(project)
        .unwrap_or(path)
        .display()
        .to_string()
}
