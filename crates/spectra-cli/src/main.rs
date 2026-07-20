mod agents;
mod autosync;
mod context_state;
mod git_sync;
mod hook;
mod install;
mod lifecycle;
mod mcp;
mod mcp_query;

use std::{
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

use agents::{Agent, Location};
use clap::{Parser, Subcommand, ValueEnum};
use spectra_core::{CodeIndex, INDEX_VERSION, IndexReport, LedgerSource, sync_project};
use spectra_render::{MapArtifact, map_project};

use crate::{
    context_state::Delivery,
    mcp_query::{ContextIntent, ContextOptions},
};

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
        /// Configure only topology MCP, without lifecycle hooks.
        #[arg(long)]
        topology_only: bool,
        /// Install globally or in one trusted project.
        #[arg(long, value_enum)]
        location: Option<Location>,
        /// Project used for a local installation.
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Accept detected targets and defaults without prompting.
        #[arg(short = 'y', long)]
        yes: bool,
        /// Disable ANSI styling.
        #[arg(long)]
        no_color: bool,
    },
    /// Remove only configuration entries owned by Spectra.
    Uninstall {
        #[arg(long, value_enum, default_value_t = Agent::Auto)]
        agent: Agent,
        #[arg(long)]
        dry_run: bool,
        /// Remove global or project-local configuration.
        #[arg(long, value_enum, default_value_t = Location::Global)]
        location: Location,
        /// Project containing a local installation.
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },
    /// Show whether Spectra is configured for a local coding agent.
    Status {
        #[arg(long, value_enum)]
        agent: Option<Agent>,
        #[arg(long, default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Internal Codex lifecycle adapter. Reads one hook event from stdin.
    #[command(hide = true)]
    Hook {
        #[arg(long, default_value = "codex")]
        agent: String,
    },
    /// Ingest harness-neutral lifecycle events over JSON stdin/stdout.
    Lifecycle {
        #[command(subcommand)]
        command: LifecycleCommand,
    },
    /// Build or refresh the local polyglot topology index.
    Init {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        no_color: bool,
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
    /// Produce one budgeted adaptive Context Packet v1.
    Context {
        query: String,
        #[arg(long, default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value_t = 600, value_parser = clap::value_parser!(u16).range(128..=2000))]
        token_budget: u16,
        #[arg(long, value_enum, default_value_t = CliContextIntent::Auto)]
        intent: CliContextIntent,
        #[arg(long, value_enum, default_value_t = CliRepresentation::Text)]
        representation: CliRepresentation,
        #[arg(long, value_enum, default_value_t = CliDelivery::Delta)]
        delivery: CliDelivery,
        #[arg(long, requires = "session_id")]
        source_harness: Option<String>,
        #[arg(long, requires = "source_harness")]
        session_id: Option<String>,
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Show or reset privacy-safe local context efficiency counters.
    Stats {
        #[arg(long, default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        reset: bool,
    },
    /// Run Spectra's MCP server over stdio.
    Serve {
        #[arg(long)]
        mcp: bool,
        /// Default project directory for MCP tool calls.
        #[arg(long)]
        path: Option<PathBuf>,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum CliContextIntent {
    #[default]
    Auto,
    Resume,
    Locate,
    Flow,
    Change,
    Inspect,
}

impl From<CliContextIntent> for ContextIntent {
    fn from(value: CliContextIntent) -> Self {
        match value {
            CliContextIntent::Auto => Self::Auto,
            CliContextIntent::Resume => Self::Resume,
            CliContextIntent::Locate => Self::Locate,
            CliContextIntent::Flow => Self::Flow,
            CliContextIntent::Change => Self::Change,
            CliContextIntent::Inspect => Self::Inspect,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum CliRepresentation {
    #[default]
    Text,
    Map,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum CliDelivery {
    #[default]
    Delta,
    Full,
}

impl From<CliDelivery> for Delivery {
    fn from(value: CliDelivery) -> Self {
        match value {
            CliDelivery::Delta => Self::Delta,
            CliDelivery::Full => Self::Full,
        }
    }
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

#[derive(Debug, Subcommand)]
enum LifecycleCommand {
    /// Ingest one canonical lifecycle JSON v1 envelope from stdin.
    Ingest,
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
        Command::Install {
            mut agent,
            dry_run,
            topology_only,
            location,
            path,
            yes,
            no_color,
        } => {
            let interactive = io::stdin().is_terminal() && io::stdout().is_terminal() && !yes;
            if !interactive && agent == Agent::Auto && !yes {
                return Err(
                    "non-interactive installation requires --yes or an explicit --agent".into(),
                );
            }
            if interactive && agent == Agent::Auto {
                agent = prompt_agent(!no_color)?;
            }
            let location = location.unwrap_or_else(|| {
                if interactive {
                    prompt_location(!no_color).unwrap_or(Location::Global)
                } else {
                    Location::Global
                }
            });
            if interactive && !confirm_install(agent, location, topology_only, !no_color)? {
                println!("Installation cancelled.");
                return Ok(());
            }
            print_agent_report(
                agents::install(agent, dry_run, topology_only, location, &path)?,
                !no_color,
            )?;
        }
        Command::Uninstall {
            agent,
            dry_run,
            location,
            path,
        } => {
            print_agent_report(agents::uninstall(agent, dry_run, location, &path)?, true)?;
        }
        Command::Status { agent, path, json } => {
            if let Some(agent) = agent {
                print_agent_report(agents::status(agent)?, true)?;
            } else {
                print_project_status(&path, json)?;
                if !json {
                    print_agent_report(agents::status_detected(), true)?;
                }
            }
        }
        Command::Hook { agent } => hook::run_stdin(&agent),
        Command::Lifecycle {
            command: LifecycleCommand::Ingest,
        } => lifecycle::run_stdin()?,
        Command::Init {
            path,
            force,
            json,
            no_color,
        } => {
            let path = guarded_project(&path, force)?;
            let started = std::time::Instant::now();
            let (index, report) = sync_project(&path)?;
            print_detailed_index_report(
                &path,
                &index,
                &report,
                started.elapsed().as_millis(),
                json,
                !no_color,
                true,
            )?;
        }
        Command::Sync { path, quiet } => {
            let (_, report) = sync_project(&path)?;
            if !quiet {
                print_index_report(&report, true);
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
        Command::Context {
            query,
            path,
            token_budget,
            intent,
            representation,
            delivery,
            source_harness,
            session_id,
            cursor,
        } => {
            let source = source_harness
                .zip(session_id)
                .map(|(harness, session_id)| LedgerSource {
                    harness,
                    session_id,
                });
            let autosync = autosync::AutoSync::default();
            let view = mcp_query::open_project(&autosync, path.to_str())?;
            let map_requested = representation == CliRepresentation::Map;
            let packet = mcp_query::context_packet(
                &view,
                ContextOptions {
                    query: &query,
                    token_budget: usize::from(token_budget),
                    intent: intent.into(),
                    delivery: delivery.into(),
                    source,
                    cursor: cursor.as_deref(),
                    map_requested,
                },
            )?;
            println!("{}", packet.text);
            if map_requested {
                let output = view.root.join(".spectra/artifacts");
                let artifact = map_project(&view.root, &query, 48, &output)?;
                println!("PNG {}", display_relative(&artifact.png_path, &view.root));
                println!("SVG {}", display_relative(&artifact.svg_path, &view.root));
            }
        }
        Command::Stats { path, json, reset } => {
            if reset {
                context_state::reset_metrics(&path)?;
            }
            let metrics = context_state::read_metrics(&path)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&metrics)?);
            } else if reset {
                println!("Spectra context metrics reset.");
            } else {
                println!(
                    "Spectra Context Stats\ncalls={} emitted_estimated_tokens={} duplicate_evidence_avoided={} maps_requested={} errors={} full={} delta={}",
                    metrics.calls,
                    metrics.estimated_tokens_emitted,
                    metrics.duplicate_evidence_avoided,
                    metrics.maps_requested,
                    metrics.errors,
                    metrics.full_deliveries,
                    metrics.delta_deliveries
                );
            }
        }
        Command::Serve {
            mcp: true,
            path: Some(path),
        } => {
            std::env::set_current_dir(path)?;
            mcp::serve().await?;
        }
        Command::Serve {
            mcp: true,
            path: None,
        } => mcp::serve().await?,
        Command::Serve { mcp: false, .. } => {
            return Err("serve currently requires --mcp".into());
        }
    }
    Ok(())
}

fn print_index_report(report: &IndexReport, color: bool) {
    let (bold, green, reset) = if color_enabled(color) {
        ("\x1b[1m", "\x1b[32m", "\x1b[0m")
    } else {
        ("", "", "")
    };
    println!(
        "{bold}Spectra Sync{reset}\n{green}✓{reset} Indexed {} source files ({} changed, {} removed): {} nodes, {} edges",
        report.files, report.changed, report.removed, report.nodes, report.edges
    );
}

fn guarded_project(path: &Path, force: bool) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let path = path.canonicalize()?;
    if !force {
        let is_root = path.parent().is_none();
        let is_home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .and_then(|home| PathBuf::from(home).canonicalize().ok())
            .is_some_and(|home| home == path);
        if is_root || is_home {
            return Err(format!(
                "refusing to initialize {}; pass --force if this is intentional",
                path.display()
            )
            .into());
        }
    }
    Ok(path)
}

fn print_detailed_index_report(
    project: &Path,
    index: &CodeIndex,
    report: &IndexReport,
    elapsed_ms: u128,
    json_output: bool,
    color: bool,
    synchronized: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let kinds = index.node_kind_counts();
    let languages = CodeIndex::language_counts(project)?;
    let bytes = CodeIndex::persisted_size(project)?;
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "version": 1,
                "project": project,
                "index_version": INDEX_VERSION,
                "files": report.files,
                "changed": report.changed,
                "removed": report.removed,
                "nodes": report.nodes,
                "edges": report.edges,
                "database_bytes": bytes,
                "elapsed_ms": elapsed_ms,
                "nodes_by_kind": kinds,
                "files_by_language": languages,
                "lazy_sync": true,
                "synchronization_state": if synchronized { "current" } else { "persisted" }
            }))?
        );
        return Ok(());
    }
    let color = color_enabled(color);
    let (bold, cyan, green, reset) = if color {
        ("\x1b[1m", "\x1b[36m", "\x1b[32m", "\x1b[0m")
    } else {
        ("", "", "", "")
    };
    println!("{bold}\nSpectra Index{reset}");
    println!("{cyan}Project:{reset} {}", project.display());
    println!("\n{bold}Index Statistics{reset}");
    println!("  Files:     {}", report.files);
    println!("  Nodes:     {}", report.nodes);
    println!("  Edges:     {}", report.edges);
    println!("  DB Size:   {bytes} bytes");
    println!("  Version:   {INDEX_VERSION}");
    println!("  Elapsed:   {elapsed_ms} ms");
    println!("\n{bold}Nodes by Kind{reset}");
    for (kind, count) in kinds {
        println!("  {kind:<18} {count}");
    }
    println!("\n{bold}Files by Language{reset}");
    for (language, count) in languages {
        println!("  {language:<18} {count}");
    }
    if synchronized {
        println!(
            "\n{green}✓{reset} Index is up to date; MCP lazy synchronization remains enabled."
        );
    } else {
        println!(
            "\nPersisted index loaded without scanning; the next MCP request will synchronize lazily."
        );
    }
    Ok(())
}

fn print_project_status(path: &Path, json_output: bool) -> Result<(), Box<dyn std::error::Error>> {
    let path = path.canonicalize()?;
    let index = CodeIndex::load(&path)?;
    if json_output {
        let value = if let Some(index) = index {
            serde_json::json!({
                "version": 1,
                "project": path,
                "initialized": true,
                "index_version": index.version,
                "files": index.graph.nodes.iter().filter(|node| index.graph.kind(node.id) == "file").count(),
                "nodes": index.graph.nodes.len(),
                "edges": index.graph.edges.len(),
                "database_bytes": CodeIndex::persisted_size(&path)?,
                "nodes_by_kind": index.node_kind_counts(),
                "files_by_language": CodeIndex::language_counts(&path)?,
                "synchronization_state": "persisted"
            })
        } else {
            serde_json::json!({"version":1,"project":path,"initialized":false,"lazy_sync":true,"synchronization_state":"uninitialized"})
        };
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else if let Some(index) = index {
        let report = IndexReport {
            files: index
                .graph
                .nodes
                .iter()
                .filter(|node| index.graph.kind(node.id) == "file")
                .count(),
            changed: 0,
            removed: 0,
            nodes: index.graph.nodes.len(),
            edges: index.graph.edges.len(),
        };
        print_detailed_index_report(&path, &index, &report, 0, false, true, false)?;
    } else {
        println!(
            "Spectra Index\nProject: {}\nStatus: not initialized (lazy MCP sync is enabled)",
            path.display()
        );
    }
    Ok(())
}

fn print_agent_report(
    report: agents::Report,
    color: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let color = color_enabled(color);
    if color {
        println!("\x1b[1m\nSpectra Agent Setup\x1b[0m");
    }
    for message in report.messages {
        if color {
            println!("\x1b[32m✓\x1b[0m {message}");
        } else {
            println!("{message}");
        }
    }
    if report.errors.is_empty() {
        Ok(())
    } else {
        Err(report.errors.join("\n").into())
    }
}

fn color_enabled(requested: bool) -> bool {
    requested
        && io::stdout().is_terminal()
        && std::env::var_os("NO_COLOR").is_none()
        && std::env::var("TERM").map_or(true, |term| term != "dumb")
}

fn prompt_location(color: bool) -> Result<Location, Box<dyn std::error::Error>> {
    let cyan = if color_enabled(color) { "\x1b[36m" } else { "" };
    let reset = if cyan.is_empty() { "" } else { "\x1b[0m" };
    print!("{cyan}Install location{reset} [G]lobal/[l]ocal: ");
    io::stdout().flush()?;
    let mut value = String::new();
    io::stdin().read_line(&mut value)?;
    Ok(
        if value.trim().eq_ignore_ascii_case("l") || value.trim().eq_ignore_ascii_case("local") {
            Location::Local
        } else {
            Location::Global
        },
    )
}

fn prompt_agent(color: bool) -> Result<Agent, Box<dyn std::error::Error>> {
    let (bold, cyan, reset) = if color_enabled(color) {
        ("\x1b[1m", "\x1b[36m", "\x1b[0m")
    } else {
        ("", "", "")
    };
    println!("{cyan}⠋{reset} Scanning for supported agents…");
    println!("{bold}Detected agents{reset}");
    for target in agents::detected_summaries() {
        println!("  • {target}");
    }
    print!("Targets [auto/all/agent name] (auto): ");
    io::stdout().flush()?;
    let mut value = String::new();
    io::stdin().read_line(&mut value)?;
    let value = value.trim();
    if value.is_empty() {
        return Ok(Agent::Auto);
    }
    Agent::from_str(value, true).map_err(|_| format!("unknown agent target '{value}'").into())
}

fn confirm_install(
    agent: Agent,
    location: Location,
    topology_only: bool,
    color: bool,
) -> Result<bool, Box<dyn std::error::Error>> {
    let (bold, reset) = if color_enabled(color) {
        ("\x1b[1m", "\x1b[0m")
    } else {
        ("", "")
    };
    println!("{bold}Spectra installation{reset}");
    println!("  Targets: {agent:?}");
    println!("  Location: {location:?}");
    println!(
        "  Capability: {}",
        if topology_only {
            "topology"
        } else {
            "best verified Ledger tier"
        }
    );
    print!("Continue? [Y/n]: ");
    io::stdout().flush()?;
    let mut value = String::new();
    io::stdin().read_line(&mut value)?;
    Ok(!matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "n" | "no"
    ))
}

fn print_anchors(artifact: &MapArtifact) {
    println!("{}", artifact.compact_metadata());
}

fn display_relative<'a>(path: &'a Path, project: &'a Path) -> String {
    path.strip_prefix(project)
        .unwrap_or(path)
        .display()
        .to_string()
}
