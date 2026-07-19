use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use clap::{Parser, ValueEnum};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum RasterBackendArg {
    #[default]
    Direct,
    SvgCompat,
}

impl RasterBackendArg {
    const fn label(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::SvgCompat => "svg-compat",
        }
    }
}

#[derive(Debug, Parser)]
#[command(about = "Run the deterministic Spectra versus CodeGraph benchmark arm")]
struct Args {
    #[arg(long)]
    manifest: PathBuf,
    #[arg(long)]
    corpus_root: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long, default_value = "codegraph")]
    codegraph_bin: PathBuf,
    #[arg(long)]
    spectra_bin: Option<PathBuf>,
    /// Rebuild both indexes before measuring warm queries.
    #[arg(long)]
    reindex: bool,
    /// Warm query repetitions; every sample and the median are recorded.
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u8).range(1..=10))]
    repeats: u8,
    /// PNG backend forwarded to `spectra map`.
    #[arg(long, value_enum, default_value_t = RasterBackendArg::Direct)]
    raster_backend: RasterBackendArg,
}

#[derive(Debug, Deserialize)]
struct Manifest {
    schema_version: u32,
    repositories: Vec<Repository>,
}

#[derive(Debug, Deserialize)]
struct Repository {
    name: String,
    url: String,
    commit: String,
    prompts: Vec<Prompt>,
}

#[derive(Debug, Deserialize)]
struct Prompt {
    id: String,
    question: String,
    expected_concepts: Vec<String>,
    expected_anchors: Vec<String>,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    generated_at_unix_ms: u128,
    manifest_schema_version: u32,
    tools: BTreeMap<String, String>,
    model_evaluation: ModelEvaluation,
    repositories: Vec<RepositoryResult>,
}

#[derive(Debug, Serialize)]
struct ModelEvaluation {
    status: &'static str,
    reason: &'static str,
    input_tokens_include_images: Option<bool>,
    task_score: Option<f64>,
}

#[derive(Debug, Serialize)]
struct RepositoryResult {
    name: String,
    url: String,
    commit: String,
    rust_files: usize,
    codegraph_index_ms: Option<u128>,
    spectra_index_ms: Option<u128>,
    prompts: Vec<PromptResult>,
}

#[derive(Debug, Serialize)]
struct PromptResult {
    id: String,
    question: String,
    expected_concepts: Vec<String>,
    expected_anchors: Vec<String>,
    codegraph: TextArm,
    spectra: ImageArm,
}

#[derive(Debug, Serialize)]
struct TextArm {
    elapsed_ms: u128,
    elapsed_samples_ms: Vec<u128>,
    output_bytes: usize,
    estimated_text_tokens: usize,
    expected_anchor_path_recall: f64,
    raw_output: String,
}

#[derive(Debug, Serialize)]
struct ImageArm {
    elapsed_ms: u128,
    elapsed_samples_ms: Vec<u128>,
    metadata_bytes: usize,
    estimated_metadata_tokens: usize,
    expected_anchor_path_recall: f64,
    png_bytes: usize,
    svg_bytes: usize,
    png_path: String,
    svg_path: String,
    raw_output: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = Args::parse();
    if args.output.is_relative() {
        args.output = std::env::current_dir()?.join(&args.output);
    }
    let manifest: Manifest = serde_json::from_slice(&fs::read(&args.manifest)?)?;
    let spectra_bin = args.spectra_bin.unwrap_or_else(|| {
        std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(|parent| parent.join("spectra")))
            .unwrap_or_else(|| PathBuf::from("spectra"))
    });
    fs::create_dir_all(&args.output)?;

    let mut tools = BTreeMap::new();
    tools.insert(
        "codegraph".into(),
        version(&args.codegraph_bin, &["version"])?,
    );
    tools.insert("spectra".into(), version(&spectra_bin, &["--version"])?);
    tools.insert(
        "spectra_raster_backend".into(),
        args.raster_backend.label().into(),
    );

    let mut repositories = Vec::new();
    for repository in manifest.repositories {
        let path = args.corpus_root.join(&repository.name);
        verify_checkout(&path, &repository.commit)?;
        let rust_files = count_rust_files(&path)?;
        let (codegraph_index_ms, spectra_index_ms) = if args.reindex {
            let codegraph = timed_command(&args.codegraph_bin, &["index", "."], &path)?.0;
            let spectra_cache = path.join(".spectra");
            if spectra_cache.exists() {
                fs::remove_dir_all(&spectra_cache)?;
            }
            let spectra = timed_command(&spectra_bin, &["init", "."], &path)?.0;
            (Some(codegraph), Some(spectra))
        } else {
            (None, None)
        };

        let mut prompts = Vec::new();
        for prompt in repository.prompts {
            let prompt_dir = args
                .output
                .join("artifacts")
                .join(&repository.name)
                .join(&prompt.id);
            fs::create_dir_all(&prompt_dir)?;

            let (codegraph_ms, codegraph_samples, codegraph_output) = repeated_command(
                args.repeats,
                &args.codegraph_bin,
                &[
                    "explore",
                    "--path",
                    path.to_str().unwrap(),
                    &prompt.question,
                ],
                &path,
            )?;
            let codegraph_text = checked_stdout(codegraph_output, "codegraph explore")?;
            let raw_codegraph = prompt_dir.join("codegraph.txt");
            fs::write(&raw_codegraph, &codegraph_text)?;

            let out_arg = prompt_dir.to_string_lossy().into_owned();
            let (spectra_ms, spectra_samples, spectra_output) = repeated_command(
                args.repeats,
                &spectra_bin,
                &[
                    "map",
                    &prompt.question,
                    "--path",
                    path.to_str().unwrap(),
                    "--max-nodes",
                    "48",
                    "--out",
                    &out_arg,
                    "--raster-backend",
                    args.raster_backend.label(),
                ],
                &path,
            )?;
            let spectra_text = checked_stdout(spectra_output, "spectra map")?;
            let (png_path, svg_path) = artifact_paths(&spectra_text, &path, &prompt_dir)?;
            let metadata = spectra_text.lines().skip(2).collect::<Vec<_>>().join("\n");

            prompts.push(PromptResult {
                id: prompt.id,
                question: prompt.question,
                expected_concepts: prompt.expected_concepts,
                expected_anchors: prompt.expected_anchors.clone(),
                codegraph: TextArm {
                    elapsed_ms: codegraph_ms,
                    elapsed_samples_ms: codegraph_samples,
                    output_bytes: codegraph_text.len(),
                    estimated_text_tokens: estimate_text_tokens(&codegraph_text),
                    expected_anchor_path_recall: anchor_path_recall(
                        &prompt.expected_anchors,
                        &codegraph_text,
                    ),
                    raw_output: relative(&raw_codegraph, &args.output),
                },
                spectra: ImageArm {
                    elapsed_ms: spectra_ms,
                    elapsed_samples_ms: spectra_samples,
                    metadata_bytes: metadata.len(),
                    estimated_metadata_tokens: estimate_text_tokens(&metadata),
                    expected_anchor_path_recall: anchor_path_recall(
                        &prompt.expected_anchors,
                        &metadata,
                    ),
                    png_bytes: fs::metadata(&png_path)?.len() as usize,
                    svg_bytes: fs::metadata(&svg_path)?.len() as usize,
                    png_path: relative(&png_path, &args.output),
                    svg_path: relative(&svg_path, &args.output),
                    raw_output: spectra_text,
                },
            });
        }
        repositories.push(RepositoryResult {
            name: repository.name,
            url: repository.url,
            commit: repository.commit,
            rust_files,
            codegraph_index_ms,
            spectra_index_ms,
            prompts,
        });
    }

    let report = Report {
        schema_version: 1,
        generated_at_unix_ms: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
        manifest_schema_version: manifest.schema_version,
        tools,
        model_evaluation: ModelEvaluation {
            status: "not_run",
            reason: "No model API credential was available; image-inclusive provider tokens and answer quality remain unset.",
            input_tokens_include_images: None,
            task_score: None,
        },
        repositories,
    };
    let report_path = args.output.join("results.json");
    fs::write(&report_path, serde_json::to_vec_pretty(&report)?)?;
    println!("Wrote {}", report_path.display());
    print_summary(&report);
    Ok(())
}

fn timed_command(
    binary: &Path,
    arguments: &[&str],
    cwd: &Path,
) -> Result<(u128, Output), Box<dyn std::error::Error>> {
    let started = Instant::now();
    let output = Command::new(binary)
        .args(arguments)
        .current_dir(cwd)
        .output()?;
    Ok((started.elapsed().as_millis(), output))
}

fn repeated_command(
    repeats: u8,
    binary: &Path,
    arguments: &[&str],
    cwd: &Path,
) -> Result<(u128, Vec<u128>, Output), Box<dyn std::error::Error>> {
    let mut samples = Vec::with_capacity(usize::from(repeats));
    let mut last = None;
    for _ in 0..repeats {
        let (elapsed, output) = timed_command(binary, arguments, cwd)?;
        if !output.status.success() {
            return Err(format!(
                "{} failed: {}",
                binary.display(),
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        samples.push(elapsed);
        last = Some(output);
    }
    let mut ordered = samples.clone();
    ordered.sort_unstable();
    Ok((
        ordered[ordered.len() / 2],
        samples,
        last.expect("repeats is at least one"),
    ))
}

fn checked_stdout(output: Output, label: &str) -> Result<String, Box<dyn std::error::Error>> {
    if !output.status.success() {
        return Err(format!(
            "{label} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn version(binary: &Path, arguments: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new(binary).args(arguments).output()?;
    Ok(checked_stdout(output, "version command")?.trim().to_owned())
}

fn verify_checkout(path: &Path, expected: &str) -> Result<(), Box<dyn std::error::Error>> {
    let actual = checked_stdout(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(path)
            .output()?,
        "git rev-parse",
    )?;
    if actual.trim() != expected {
        return Err(format!(
            "{} is at {}, expected {expected}",
            path.display(),
            actual.trim()
        )
        .into());
    }
    Ok(())
}

fn count_rust_files(path: &Path) -> Result<usize, Box<dyn std::error::Error>> {
    fn visit(path: &Path, count: &mut usize) -> std::io::Result<()> {
        if path.file_name().is_some_and(|name| {
            name == ".git" || name == ".codegraph" || name == ".spectra" || name == "target"
        }) {
            return Ok(());
        }
        if path.is_dir() {
            for entry in fs::read_dir(path)? {
                visit(&entry?.path(), count)?;
            }
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            *count += 1;
        }
        Ok(())
    }
    let mut count = 0;
    visit(path, &mut count)?;
    Ok(count)
}

fn artifact_paths(
    stdout: &str,
    project: &Path,
    output: &Path,
) -> Result<(PathBuf, PathBuf), Box<dyn std::error::Error>> {
    let mut lines = stdout.lines();
    let png = lines
        .next()
        .and_then(|line| line.strip_prefix("PNG "))
        .ok_or("missing PNG path")?;
    let svg = lines
        .next()
        .and_then(|line| line.strip_prefix("SVG "))
        .ok_or("missing SVG path")?;
    let resolve = |value: &str| {
        let path = PathBuf::from(value);
        if path.is_absolute() {
            path
        } else if output.join(&path).exists() {
            output.join(path)
        } else {
            project.join(path)
        }
    };
    Ok((resolve(png), resolve(svg)))
}

fn estimate_text_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}
fn anchor_path_recall(expected: &[String], output: &str) -> f64 {
    if expected.is_empty() {
        return 1.0;
    }
    let found = expected
        .iter()
        .filter(|anchor| {
            let path = anchor
                .find(".rs:")
                .map(|position| &anchor[..position + 3])
                .unwrap_or(anchor);
            output.contains(path)
        })
        .count();
    found as f64 / expected.len() as f64
}
fn relative(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn print_summary(report: &Report) {
    println!(
        "repository,prompt,codegraph_ms,codegraph_bytes,spectra_ms,spectra_metadata_bytes,png_bytes"
    );
    for repository in &report.repositories {
        for prompt in &repository.prompts {
            println!(
                "{},{},{},{},{},{},{}",
                repository.name,
                prompt.id,
                prompt.codegraph.elapsed_ms,
                prompt.codegraph.output_bytes,
                prompt.spectra.elapsed_ms,
                prompt.spectra.metadata_bytes,
                prompt.spectra.png_bytes
            );
        }
    }
}
