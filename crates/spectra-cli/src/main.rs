mod hook;
mod install;
mod mcp;

use std::{
    path::{Path, PathBuf},
    process::ExitCode,
};

use clap::{Parser, Subcommand, ValueEnum};
use spectra_core::{CodeIndex, map_project};

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
        #[arg(long, value_enum, default_value_t = Agent::Codex)]
        agent: Agent,
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove only configuration entries owned by Spectra.
    Uninstall {
        #[arg(long, value_enum, default_value_t = Agent::Codex)]
        agent: Agent,
        #[arg(long)]
        dry_run: bool,
    },
    /// Show whether Spectra is configured for a local coding agent.
    Status {
        #[arg(long, value_enum, default_value_t = Agent::Codex)]
        agent: Agent,
    },
    /// Internal Codex lifecycle adapter. Reads one hook event from stdin.
    #[command(hide = true)]
    Hook,
    /// Build or refresh the local Rust topology index.
    Init {
        #[arg(default_value = ".")]
        path: PathBuf,
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
    /// Run Spectra's single-tool MCP server over stdio.
    Serve {
        #[arg(long)]
        mcp: bool,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Agent {
    Codex,
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
        Command::Install { agent, dry_run } => match agent {
            Agent::Codex => println!("{}", install::install_codex(dry_run)?),
        },
        Command::Uninstall { agent, dry_run } => match agent {
            Agent::Codex => println!("{}", install::uninstall_codex(dry_run)?),
        },
        Command::Status { agent } => match agent {
            Agent::Codex => println!("{}", install::codex_status()?),
        },
        Command::Hook => hook::run_stdin(),
        Command::Init { path } => {
            let (_, report) = CodeIndex::refresh(&path)?;
            println!(
                "Indexed {} Rust files ({} changed, {} removed): {} nodes, {} edges",
                report.files, report.changed, report.removed, report.nodes, report.edges
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

fn print_anchors(artifact: &spectra_core::MapArtifact) {
    for (id, anchor) in &artifact.anchors {
        println!(
            "{id}={}:{}-{}",
            anchor.path, anchor.start_line, anchor.end_line
        );
    }
    println!(
        "nodes={} truncated={} index=v{}",
        artifact.node_count, artifact.truncated, artifact.index_version
    );
}

fn display_relative<'a>(path: &'a Path, project: &'a Path) -> String {
    path.strip_prefix(project)
        .unwrap_or(path)
        .display()
        .to_string()
}
