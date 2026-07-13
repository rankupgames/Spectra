use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use spectra_core::{CodeIndex, INDEX_VERSION};

#[derive(Debug, Parser)]
#[command(about = "Measure Spectra framework-route parity against pinned CodeGraph repositories")]
struct Args {
    #[arg(long, default_value = "benchmarks/adapter-repositories.json")]
    manifest: PathBuf,
    #[arg(long)]
    corpus_root: PathBuf,
    #[arg(long, default_value = "benchmarks/results/adapter-parity")]
    output: PathBuf,
    #[arg(long, default_value = "codegraph")]
    codegraph_bin: PathBuf,
    #[arg(long)]
    reindex: bool,
}

#[derive(Debug, Deserialize)]
struct Manifest {
    schema_version: u32,
    repositories: Vec<Repository>,
}

#[derive(Debug, Deserialize)]
struct Repository {
    name: String,
    framework: String,
    url: String,
    commit: String,
    minimum_spectra_routes: usize,
}

#[derive(Debug, Serialize)]
struct Evaluation {
    schema_version: u32,
    generated_at_unix_ms: u128,
    manifest_schema_version: u32,
    codegraph_version: String,
    spectra_index_version: u32,
    passed: bool,
    reference_routes: usize,
    shared_routes: usize,
    codegraph_route_recall: f64,
    repositories: Vec<RepositoryEvaluation>,
}

#[derive(Debug, Serialize)]
struct RepositoryEvaluation {
    name: String,
    framework: String,
    url: String,
    commit: String,
    source_files: usize,
    nodes: usize,
    edges: usize,
    spectra_index_ms: u128,
    codegraph_index_ms: Option<u128>,
    minimum_spectra_routes: usize,
    codegraph_routes: Vec<String>,
    spectra_routes: Vec<String>,
    shared_routes: Vec<String>,
    missing_codegraph_routes: Vec<String>,
    additional_spectra_routes: Vec<String>,
    resolved_spectra_routes: usize,
    passed: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let manifest: Manifest = serde_json::from_slice(&fs::read(&args.manifest)?)?;
    fs::create_dir_all(&args.output)?;
    let codegraph_version = checked_stdout(
        Command::new(&args.codegraph_bin)
            .arg("--version")
            .output()?,
        "CodeGraph version",
    )?
    .trim()
    .to_owned();

    let mut repositories = Vec::new();
    for repository in manifest.repositories {
        let root = args.corpus_root.join(&repository.name);
        verify_checkout(&root, &repository.commit)?;
        let codegraph_index_ms = args
            .reindex
            .then(|| {
                timed_command(
                    &args.codegraph_bin,
                    &["index", root.to_string_lossy().as_ref()],
                )
            })
            .transpose()?;
        if args.reindex {
            let cache = root.join(format!(".spectra/index-v{INDEX_VERSION}.json"));
            if cache.exists() {
                fs::remove_file(cache)?;
            }
        }

        let started = Instant::now();
        let (index, report) = CodeIndex::refresh(&root)?;
        let spectra_index_ms = started.elapsed().as_millis();
        let spectra_routes = route_labels(&index);
        let resolved_spectra_routes = resolved_route_count(&index);
        let codegraph_routes = codegraph_routes(&args.codegraph_bin, &root)?;
        let shared_routes = codegraph_routes
            .intersection(&spectra_routes)
            .cloned()
            .collect::<BTreeSet<_>>();
        let missing_codegraph_routes = codegraph_routes
            .difference(&spectra_routes)
            .cloned()
            .collect::<Vec<_>>();
        let additional_spectra_routes = spectra_routes
            .difference(&codegraph_routes)
            .cloned()
            .collect::<Vec<_>>();
        let passed = missing_codegraph_routes.is_empty()
            && spectra_routes.len() >= repository.minimum_spectra_routes;
        repositories.push(RepositoryEvaluation {
            name: repository.name,
            framework: repository.framework,
            url: repository.url,
            commit: repository.commit,
            source_files: report.files,
            nodes: report.nodes,
            edges: report.edges,
            spectra_index_ms,
            codegraph_index_ms,
            minimum_spectra_routes: repository.minimum_spectra_routes,
            codegraph_routes: codegraph_routes.into_iter().collect(),
            spectra_routes: spectra_routes.into_iter().collect(),
            shared_routes: shared_routes.into_iter().collect(),
            missing_codegraph_routes,
            additional_spectra_routes,
            resolved_spectra_routes,
            passed,
        });
    }

    let reference_routes = repositories
        .iter()
        .map(|repository| repository.codegraph_routes.len())
        .sum();
    let shared_routes = repositories
        .iter()
        .map(|repository| repository.shared_routes.len())
        .sum();
    let codegraph_route_recall = if reference_routes == 0 {
        0.0
    } else {
        shared_routes as f64 / reference_routes as f64
    };
    let passed = reference_routes > 0 && repositories.iter().all(|repository| repository.passed);
    let evaluation = Evaluation {
        schema_version: 1,
        generated_at_unix_ms: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
        manifest_schema_version: manifest.schema_version,
        codegraph_version,
        spectra_index_version: INDEX_VERSION,
        passed,
        reference_routes,
        shared_routes,
        codegraph_route_recall,
        repositories,
    };
    let output = args.output.join("adapter-evaluation.json");
    fs::write(&output, serde_json::to_vec_pretty(&evaluation)?)?;
    println!(
        "passed={} reference_routes={} shared_routes={} recall={:.3}",
        evaluation.passed,
        evaluation.reference_routes,
        evaluation.shared_routes,
        evaluation.codegraph_route_recall
    );
    for repository in &evaluation.repositories {
        println!(
            "{} framework={} codegraph={} spectra={} shared={} resolved={} passed={}",
            repository.name,
            repository.framework,
            repository.codegraph_routes.len(),
            repository.spectra_routes.len(),
            repository.shared_routes.len(),
            repository.resolved_spectra_routes,
            repository.passed
        );
    }
    println!("Wrote {}", output.display());
    if evaluation.passed {
        Ok(())
    } else {
        Err("adapter parity gate failed".into())
    }
}

fn route_labels(index: &CodeIndex) -> BTreeSet<String> {
    index
        .graph
        .nodes
        .iter()
        .filter(|node| index.graph.kind(node.id) == "route")
        .map(|node| index.graph.label(node.id).to_owned())
        .collect()
}

fn resolved_route_count(index: &CodeIndex) -> usize {
    index
        .graph
        .nodes
        .iter()
        .filter(|node| index.graph.kind(node.id) == "route")
        .filter(|route| {
            index.graph.edges.iter().any(|edge| {
                edge.source == route.id
                    && matches!(
                        index.graph.atom(edge.kind),
                        "routes_to" | "calls" | "references" | "binds"
                    )
            })
        })
        .count()
}

fn codegraph_routes(
    codegraph: &Path,
    root: &Path,
) -> Result<BTreeSet<String>, Box<dyn std::error::Error>> {
    let output = Command::new(codegraph)
        .args(["query", "", "--path"])
        .arg(root)
        .args(["--kind", "route", "--limit", "10000", "--json"])
        .output()?;
    let value: Value = serde_json::from_str(&checked_stdout(output, "CodeGraph route query")?)?;
    Ok(value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|result| result["node"]["name"].as_str())
        .map(str::to_owned)
        .collect())
}

fn verify_checkout(path: &Path, expected: &str) -> Result<(), Box<dyn std::error::Error>> {
    let output = Command::new("git")
        .args(["-C"])
        .arg(path)
        .args(["rev-parse", "HEAD"])
        .output()?;
    let actual = checked_stdout(output, "git rev-parse")?;
    if actual.trim() == expected {
        Ok(())
    } else {
        Err(format!(
            "{} is at {}, expected {expected}",
            path.display(),
            actual.trim()
        )
        .into())
    }
}

fn timed_command(binary: &Path, args: &[&str]) -> Result<u128, Box<dyn std::error::Error>> {
    let started = Instant::now();
    let output = Command::new(binary).args(args).output()?;
    checked_stdout(output, "index command")?;
    Ok(started.elapsed().as_millis())
}

fn checked_stdout(
    output: std::process::Output,
    action: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    if output.status.success() {
        Ok(String::from_utf8(output.stdout)?)
    } else {
        Err(format!(
            "{action} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into())
    }
}
