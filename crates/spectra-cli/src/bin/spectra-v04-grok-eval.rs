use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use clap::{Parser, ValueEnum};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use spectra_core::estimate_tokens;

const SYSTEM_PROMPT: &str = "You are evaluating a local code repository. Use only the supplied local tools. Do not edit files. Return a concise answer with exact path:start-end anchors for every material claim. For flows, order the anchors. For change or repair work, name focused tests. Stop once the evidence is sufficient.";
const MAX_TOOL_OUTPUT: usize = 30_000;

#[derive(Debug, Parser)]
#[command(about = "Run the complete Spectra v0.4 holdout with Grok")]
struct Args {
    #[arg(long, default_value = "benchmarks/v0.4-holdout.json")]
    manifest: PathBuf,
    #[arg(long)]
    corpus_root: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long, default_value = "target/release/spectra")]
    spectra_bin: PathBuf,
    #[arg(long, default_value = ".env")]
    env_file: PathBuf,
    #[arg(long, default_value = "grok-4.5")]
    model: String,
    #[arg(long, default_value_t = 600, value_parser = clap::value_parser!(u16).range(128..=2000))]
    token_budget: u16,
    #[arg(long, default_value_t = 8, value_parser = clap::value_parser!(u8).range(1..=16))]
    max_turns: u8,
    /// Run at most this many expanded tasks; zero runs all 100.
    #[arg(long, default_value_t = 0)]
    limit: usize,
    #[arg(long)]
    repository: Vec<String>,
    #[arg(long)]
    category: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct Manifest {
    schema_version: u32,
    repositories: Vec<Repository>,
    task_templates: Vec<TaskTemplate>,
}

#[derive(Clone, Debug, Deserialize)]
struct Repository {
    id: String,
    commit: String,
    public_boundary: String,
    focus: String,
    verification: String,
}

#[derive(Clone, Debug, Deserialize)]
struct TaskTemplate {
    id: String,
    category: String,
    prompt: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum ArmKind {
    Baseline,
    Spectra,
}

impl ArmKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Spectra => "spectra",
        }
    }
}

#[derive(Clone, Debug)]
struct ExpandedTask {
    id: String,
    repository: String,
    category: String,
    prompt: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct ArmResult {
    input_tokens: u64,
    cached_input_tokens: u64,
    output_tokens: u64,
    latency_ms: u64,
    api_calls: u64,
    retrieval_calls: u64,
    spectra_calls: u64,
    repeated_context_bytes: u64,
    packets_within_budget: u64,
    packets_total: u64,
    cost_usd: f64,
    valid_anchors: usize,
    solved: bool,
    answer: String,
}

#[derive(Clone, Debug, Serialize)]
struct GateReport {
    schema_version: u32,
    environments: Vec<Environment>,
    tasks: Vec<GateTask>,
    forbidden_findings: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
struct Environment {
    id: String,
    harness: String,
    model: String,
}

#[derive(Clone, Debug, Serialize)]
struct GateTask {
    id: String,
    repository: String,
    environment_id: String,
    category: String,
    baseline: GateArm,
    spectra: GateArm,
    packets_within_budget: u64,
    packets_total: u64,
}

#[derive(Clone, Debug, Serialize)]
struct GateArm {
    input_tokens: u64,
    input_schema_tokens: u64,
    input_text_tokens: u64,
    image_tokens: u64,
    cached_input_tokens: u64,
    output_tokens: u64,
    solved: bool,
    tool_calls: u64,
    latency_ms: u64,
    repeated_context_bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
struct DetailReport {
    schema_version: u32,
    label: &'static str,
    generated_at_unix_ms: u128,
    model: String,
    input_schema_accounting: &'static str,
    tasks: Vec<DetailTask>,
}

#[derive(Clone, Debug, Serialize)]
struct DetailTask {
    id: String,
    repository: String,
    category: String,
    baseline: ArmResult,
    spectra: ArmResult,
}

#[derive(Default)]
struct DeliveryAccounting {
    seen: BTreeSet<String>,
    repeated_bytes: u64,
    packets_within_budget: u64,
    packets_total: u64,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let manifest: Manifest = serde_json::from_slice(&fs::read(&args.manifest)?)?;
    if manifest.schema_version != 1 {
        return Err(format!("unsupported holdout schema {}", manifest.schema_version).into());
    }
    let key = api_key(&args.env_file)?;
    let tasks = expand_tasks(&manifest, &args);
    if tasks.is_empty() {
        return Err("no holdout tasks matched the filters".into());
    }
    fs::create_dir_all(&args.output)?;
    let mut details = Vec::new();
    for (index, task) in tasks.iter().enumerate() {
        let repository = args.corpus_root.join(&task.repository);
        verify_checkout(&repository, repository_commit(&manifest, &task.repository)?)?;
        eprintln!(
            "[{}/{}] {}/{} baseline",
            index + 1,
            tasks.len(),
            task.repository,
            task.category
        );
        let baseline = run_arm(&args, &key, task, &repository, ArmKind::Baseline)?;
        eprintln!(
            "[{}/{}] {}/{} spectra",
            index + 1,
            tasks.len(),
            task.repository,
            task.category
        );
        let spectra = run_arm(&args, &key, task, &repository, ArmKind::Spectra)?;
        details.push(DetailTask {
            id: task.id.clone(),
            repository: task.repository.clone(),
            category: task.category.clone(),
            baseline,
            spectra,
        });
        write_reports(&args, &manifest, &details)?;
    }
    let (input_reduction, cost) = summary(&details);
    println!(
        "Completed {} paired tasks with {}: aggregate input reduction={:.1}% cost=${cost:.4}",
        details.len(),
        args.model,
        input_reduction * 100.0
    );
    println!(
        "Wrote {}",
        args.output.join("grok-pilot-details.json").display()
    );
    println!("Wrote {}", args.output.join("reviewed-v0.4.json").display());
    Ok(())
}

fn expand_tasks(manifest: &Manifest, args: &Args) -> Vec<ExpandedTask> {
    let mut tasks = Vec::new();
    for repository in &manifest.repositories {
        if !args.repository.is_empty() && !args.repository.contains(&repository.id) {
            continue;
        }
        for template in &manifest.task_templates {
            if !args.category.is_empty() && !args.category.contains(&template.category) {
                continue;
            }
            let prompt = template
                .prompt
                .replace("{focus}", &repository.focus)
                .replace("{public_boundary}", &repository.public_boundary)
                .replace("{verification}", &repository.verification);
            tasks.push(ExpandedTask {
                id: format!("{}-{}", repository.id, template.id),
                repository: repository.id.clone(),
                category: template.category.clone(),
                prompt,
            });
            if args.limit != 0 && tasks.len() >= args.limit {
                return tasks;
            }
        }
    }
    tasks
}

fn run_arm(
    args: &Args,
    key: &str,
    task: &ExpandedTask,
    repository: &Path,
    arm: ArmKind,
) -> Result<ArmResult, Box<dyn std::error::Error>> {
    let arm_dir = args
        .output
        .join("artifacts")
        .join(&task.repository)
        .join(&task.category)
        .join(arm.as_str());
    fs::create_dir_all(&arm_dir)?;
    let summary_path = arm_dir.join("summary.json");
    if summary_path.exists() {
        return Ok(serde_json::from_slice(&fs::read(summary_path)?)?);
    }
    let tools = tool_schemas(arm);
    let mut delivery = DeliveryAccounting::default();
    let mut result = ArmResult::default();
    let initial_context = if arm == ArmKind::Spectra {
        result.spectra_calls += 1;
        let output = spectra_context(args, repository, task, &task.prompt, &task.category, "full")?;
        account_output(&mut delivery, &output);
        account_packet(&mut delivery, &output);
        Some(output)
    } else {
        None
    };
    let mut history = vec![
        json!({"role":"system", "content":SYSTEM_PROMPT}),
        json!({
            "role":"user",
            "content": match initial_context {
                Some(context) => format!("Task: {}\n\nInitial Spectra Context Packet:\n{}", task.prompt, context),
                None => format!("Task: {}", task.prompt),
            }
        }),
    ];
    for turn in 0..args.max_turns {
        let raw_path = arm_dir.join(format!("turn-{turn}.json"));
        let (response, elapsed) = api_response(key, &args.model, &history, &tools, &raw_path)?;
        result.latency_ms = result.latency_ms.saturating_add(elapsed as u64);
        result.api_calls += 1;
        add_usage(&mut result, &response);
        let outputs = response["output"].as_array().cloned().unwrap_or_default();
        history.extend(outputs.iter().cloned());
        let calls = outputs
            .iter()
            .filter(|item| item["type"] == "function_call")
            .cloned()
            .collect::<Vec<_>>();
        if calls.is_empty() {
            result.answer = extract_answer(&response);
            break;
        }
        for call in calls {
            let name = call["name"].as_str().unwrap_or_default();
            let arguments: Value = serde_json::from_str(call["arguments"].as_str().unwrap_or("{}"))
                .unwrap_or_else(|_| json!({}));
            let output = execute_tool(args, repository, task, arm, name, &arguments)?;
            result.retrieval_calls += 1;
            if name == "spectra_context" {
                result.spectra_calls += 1;
                account_packet(&mut delivery, &output);
            }
            account_output(&mut delivery, &output);
            history.push(json!({
                "type":"function_call_output",
                "call_id":call["call_id"].as_str().unwrap_or_default(),
                "output":output,
            }));
        }
    }
    result.repeated_context_bytes = delivery.repeated_bytes;
    result.packets_within_budget = delivery.packets_within_budget;
    result.packets_total = delivery.packets_total;
    result.valid_anchors = valid_anchor_count(repository, &result.answer);
    result.solved = deterministic_success(task, &result);
    write_json(&summary_path, &result)?;
    Ok(result)
}

fn tool_schemas(arm: ArmKind) -> Value {
    let read = json!({
        "type":"function", "name":"read_source",
        "description":"Read a bounded, line-numbered source range after locating an exact file.",
        "parameters":{"type":"object","properties":{
            "path":{"type":"string"}, "start":{"type":"integer"}, "end":{"type":"integer"}
        },"required":["path","start","end"],"additionalProperties":false},
        "strict":true
    });
    if arm == ArmKind::Baseline {
        json!([{
            "type":"function", "name":"search_code",
            "description":"Literal search across repository source. Use a precise symbol or phrase.",
            "parameters":{"type":"object","properties":{"query":{"type":"string"}},
            "required":["query"],"additionalProperties":false}, "strict":true
        }, read])
    } else {
        json!([{
            "type":"function", "name":"spectra_context",
            "description":"Get another budgeted adaptive context packet. Prefer the initial packet and call again only when evidence is insufficient.",
            "parameters":{"type":"object","properties":{
                "query":{"type":"string"},
                "intent":{"type":"string","enum":["auto","resume","locate","flow","change","inspect"]}
            },"required":["query","intent"],"additionalProperties":false}, "strict":true
        }, read])
    }
}

fn execute_tool(
    args: &Args,
    repository: &Path,
    task: &ExpandedTask,
    arm: ArmKind,
    name: &str,
    arguments: &Value,
) -> Result<String, Box<dyn std::error::Error>> {
    let output = (|| -> Result<String, Box<dyn std::error::Error>> {
        match name {
            "search_code" if arm == ArmKind::Baseline => {
                search_code(repository, string_argument(arguments, "query")?)
            }
            "read_source" => read_source(
                repository,
                string_argument(arguments, "path")?,
                integer_argument(arguments, "start", 1),
                integer_argument(arguments, "end", 120),
            ),
            "spectra_context" if arm == ArmKind::Spectra => spectra_context(
                args,
                repository,
                task,
                string_argument(arguments, "query")?,
                string_argument(arguments, "intent").unwrap_or("auto"),
                "delta",
            ),
            _ => Ok(format!("tool_error unsupported tool {name}")),
        }
    })()
    .unwrap_or_else(|error| format!("tool_error {name} failed: {error}"));
    Ok(bound_text(output, MAX_TOOL_OUTPUT))
}

fn search_code(repository: &Path, query: &str) -> Result<String, Box<dyn std::error::Error>> {
    if query.trim().is_empty() || query.len() > 200 {
        return Ok("tool_error search query must contain 1..200 characters".into());
    }
    let output = Command::new("rg")
        .args([
            "--line-number",
            "--fixed-strings",
            "--color",
            "never",
            "--glob",
            "!.git/**",
            "--glob",
            "!.spectra/**",
            query,
            ".",
        ])
        .current_dir(repository)
        .output()?;
    if !output.status.success() && output.status.code() != Some(1) {
        return Ok(format!(
            "tool_error search failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let lines = text.lines().take(80).collect::<Vec<_>>();
    if lines.is_empty() {
        Ok("No literal matches.".into())
    } else {
        Ok(lines.join("\n"))
    }
}

fn read_source(
    repository: &Path,
    relative: &str,
    start: usize,
    end: usize,
) -> Result<String, Box<dyn std::error::Error>> {
    let relative_path = Path::new(relative.trim_start_matches("./"));
    if relative_path.is_absolute()
        || relative_path.components().any(|component| {
            matches!(component, Component::ParentDir | Component::RootDir | Component::Prefix(_))
        })
        || relative_path.components().any(|component| {
            matches!(component, Component::Normal(name) if name == ".git" || name == ".spectra" || name == ".env")
        })
    {
        return Ok("tool_error path is outside the readable source boundary".into());
    }
    let path = repository.join(relative_path).canonicalize()?;
    let root = repository.canonicalize()?;
    if !path.starts_with(&root) || !path.is_file() {
        return Ok("tool_error path is not a repository file".into());
    }
    let content = fs::read_to_string(path)?;
    let lines = content.lines().collect::<Vec<_>>();
    let start = start.max(1);
    let end = end
        .max(start)
        .min(start.saturating_add(199))
        .min(lines.len());
    if start > lines.len() {
        return Ok(format!(
            "tool_error start {start} is past {} lines",
            lines.len()
        ));
    }
    Ok(lines
        .iter()
        .enumerate()
        .take(end)
        .skip(start - 1)
        .map(|(index, line)| format!("{}\t{line}", index + 1))
        .collect::<Vec<_>>()
        .join("\n"))
}

fn spectra_context(
    args: &Args,
    repository: &Path,
    task: &ExpandedTask,
    query: &str,
    intent: &str,
    delivery: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let intent = match intent {
        "auto" | "resume" | "locate" | "flow" | "change" | "inspect" => intent,
        _ => "auto",
    };
    let output = Command::new(&args.spectra_bin)
        .args([
            "context",
            query,
            "--path",
            repository.to_str().ok_or("non-UTF-8 repository path")?,
            "--token-budget",
            &args.token_budget.to_string(),
            "--intent",
            intent,
            "--delivery",
            delivery,
            "--source-harness",
            "grok-v04-eval",
            "--session-id",
            &task.id,
        ])
        .output()?;
    if !output.status.success() {
        return Ok(format!(
            "tool_error spectra_context failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn api_response(
    key: &str,
    model: &str,
    history: &[Value],
    tools: &Value,
    raw_path: &Path,
) -> Result<(Value, u128), Box<dyn std::error::Error>> {
    if raw_path.exists() {
        let envelope: Value = serde_json::from_slice(&fs::read(raw_path)?)?;
        return Ok((
            envelope["response"].clone(),
            envelope["elapsed_ms"].as_u64().unwrap_or(0) as u128,
        ));
    }
    let request = json!({
        "model":model,
        "store":false,
        "reasoning":{"effort":"low"},
        "max_output_tokens":900,
        "include":["reasoning.encrypted_content"],
        "tools":tools,
        "input":history,
    });
    let request_bytes = serde_json::to_vec(&request)?;
    let started = Instant::now();
    for attempt in 0..=3_u32 {
        let mut child = Command::new("curl")
            .args([
                "-sS",
                "https://api.x.ai/v1/responses",
                "-m",
                "3600",
                "-H",
                "Content-Type: application/json",
                "-H",
                &format!("Authorization: Bearer {key}"),
                "--data-binary",
                "@-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .ok_or("curl stdin unavailable")?
            .write_all(&request_bytes)?;
        let response = child.wait_with_output()?;
        if response.status.success()
            && let Ok(value) = serde_json::from_slice::<Value>(&response.stdout)
        {
            if value["error"].is_null() {
                let elapsed = started.elapsed().as_millis();
                write_json(raw_path, &json!({"elapsed_ms":elapsed,"response":value}))?;
                return Ok((value, elapsed));
            }
            if attempt < 3 && retriable_api_error(&value["error"]) {
                std::thread::sleep(Duration::from_secs(1_u64 << attempt));
                continue;
            }
            return Err(format!("xAI API error: {}", value["error"]).into());
        }
        if attempt < 3 {
            std::thread::sleep(Duration::from_secs(1_u64 << attempt));
            continue;
        }
        return Err(format!(
            "xAI request failed: {}",
            String::from_utf8_lossy(&response.stderr)
        )
        .into());
    }
    unreachable!()
}

fn add_usage(result: &mut ArmResult, response: &Value) {
    let usage = &response["usage"];
    result.input_tokens = result
        .input_tokens
        .saturating_add(number(usage, "input_tokens"));
    result.cached_input_tokens = result
        .cached_input_tokens
        .saturating_add(number(&usage["input_tokens_details"], "cached_tokens"));
    result.output_tokens = result
        .output_tokens
        .saturating_add(number(usage, "output_tokens"));
    result.cost_usd += usage["cost_in_usd_ticks"].as_u64().unwrap_or(0) as f64 / 10_000_000_000.0;
}

fn account_output(accounting: &mut DeliveryAccounting, output: &str) {
    if !accounting.seen.insert(output.to_owned()) {
        accounting.repeated_bytes = accounting
            .repeated_bytes
            .saturating_add(output.len() as u64);
    }
}

fn account_packet(accounting: &mut DeliveryAccounting, output: &str) {
    let Some(header) = output.lines().find(|line| line.starts_with("C1 ")) else {
        return;
    };
    accounting.packets_total += 1;
    let budget = header
        .split_whitespace()
        .find_map(|part| part.strip_prefix("budget="))
        .and_then(|value| value.parse::<u64>().ok());
    let used = header
        .split_whitespace()
        .find_map(|part| part.strip_prefix("used≈"))
        .and_then(|value| value.parse::<u64>().ok());
    if matches!((budget, used), (Some(budget), Some(used)) if used <= budget) {
        accounting.packets_within_budget += 1;
    }
}

fn extract_answer(response: &Value) -> String {
    response["output"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item["type"] == "message")
        .flat_map(|item| item["content"].as_array().into_iter().flatten())
        .filter(|content| content["type"] == "output_text")
        .filter_map(|content| content["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn valid_anchor_count(repository: &Path, answer: &str) -> usize {
    answer
        .split_whitespace()
        .filter_map(|token| {
            let token = token
                .trim_matches(|ch: char| matches!(ch, '`' | ',' | '.' | ')' | '(' | '[' | ']'));
            let (path, range) = token.rsplit_once(':')?;
            let line = range.split('-').next()?.parse::<usize>().ok()?;
            let relative = path.trim_start_matches("./");
            if line == 0 || relative.contains("..") || !relative.contains('.') {
                return None;
            }
            let content = fs::read_to_string(repository.join(relative)).ok()?;
            (line <= content.lines().count()).then_some(format!("{relative}:{line}"))
        })
        .collect::<BTreeSet<_>>()
        .len()
}

fn deterministic_success(task: &ExpandedTask, result: &ArmResult) -> bool {
    if result.answer.len() < 80 || result.valid_anchors == 0 {
        return false;
    }
    let answer = result.answer.to_ascii_lowercase();
    match task.category.as_str() {
        "flow" => result.valid_anchors >= 2,
        "change-impact" | "repair" => answer.contains("test") || answer.contains("verify"),
        "resume" => answer.contains("next") || answer.contains("verify"),
        _ => true,
    }
}

fn write_reports(
    args: &Args,
    manifest: &Manifest,
    details: &[DetailTask],
) -> Result<(), Box<dyn std::error::Error>> {
    let environment_id = format!("{}-local", args.model);
    let tasks = details
        .iter()
        .map(|task| {
            let baseline = gate_arm(
                &task.baseline,
                tool_schema_estimate(ArmKind::Baseline, task.baseline.api_calls),
            );
            let spectra = gate_arm(
                &task.spectra,
                tool_schema_estimate(ArmKind::Spectra, task.spectra.api_calls),
            );
            GateTask {
                id: task.id.clone(),
                repository: task.repository.clone(),
                environment_id: environment_id.clone(),
                category: task.category.clone(),
                packets_within_budget: task.spectra.packets_within_budget,
                packets_total: task.spectra.packets_total.max(1),
                baseline,
                spectra,
            }
        })
        .collect();
    let gate = GateReport {
        schema_version: 1,
        environments: vec![Environment {
            id: environment_id,
            harness: "spectra-v04-grok-eval".into(),
            model: args.model.clone(),
        }],
        tasks,
        forbidden_findings: privacy_findings(&args.corpus_root, manifest),
    };
    let detail = DetailReport {
        schema_version: 1,
        label: "single-provider-pilot",
        generated_at_unix_ms: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
        model: args.model.clone(),
        input_schema_accounting: "conservative local estimate; provider reports total and cached input only",
        tasks: details.to_vec(),
    };
    write_json(&args.output.join("reviewed-v0.4.json"), &gate)?;
    write_json(&args.output.join("grok-pilot-details.json"), &detail)?;
    Ok(())
}

fn gate_arm(result: &ArmResult, schema_estimate: u64) -> GateArm {
    let schema = schema_estimate.min(result.input_tokens.saturating_sub(1));
    GateArm {
        input_tokens: result.input_tokens,
        input_schema_tokens: schema,
        input_text_tokens: result.input_tokens.saturating_sub(schema),
        image_tokens: 0,
        cached_input_tokens: result.cached_input_tokens,
        output_tokens: result.output_tokens.max(1),
        solved: result.solved,
        tool_calls: if result.spectra_calls == 0 {
            result.retrieval_calls
        } else {
            result.spectra_calls
        },
        latency_ms: result.latency_ms.max(1),
        repeated_context_bytes: result.repeated_context_bytes,
    }
}

fn tool_schema_estimate(arm: ArmKind, calls: u64) -> u64 {
    estimate_tokens(&serde_json::to_string(&tool_schemas(arm)).unwrap_or_default()) as u64 * calls
}

fn privacy_findings(corpus: &Path, manifest: &Manifest) -> Vec<String> {
    let mut findings = Vec::new();
    for repository in &manifest.repositories {
        for name in ["context-receipts-v1.json", "metrics-v1.json"] {
            let path = corpus.join(&repository.id).join(".spectra").join(name);
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            if text.contains("grok-v04-eval")
                || manifest
                    .task_templates
                    .iter()
                    .any(|task| text.contains(&task.prompt))
            {
                findings.push(format!(
                    "{} contains raw session or prompt text",
                    path.display()
                ));
            }
            if text.to_ascii_lowercase().contains("xai_key") || text.contains("Bearer ") {
                findings.push(format!(
                    "{} contains credential-shaped text",
                    path.display()
                ));
            }
        }
    }
    findings
}

fn summary(details: &[DetailTask]) -> (f64, f64) {
    let baseline = details
        .iter()
        .map(|task| task.baseline.input_tokens)
        .sum::<u64>() as f64;
    let spectra = details
        .iter()
        .map(|task| task.spectra.input_tokens)
        .sum::<u64>() as f64;
    let cost = details
        .iter()
        .map(|task| task.baseline.cost_usd + task.spectra.cost_usd)
        .sum();
    (
        if baseline == 0.0 {
            0.0
        } else {
            1.0 - spectra / baseline
        },
        cost,
    )
}

fn repository_commit<'a>(
    manifest: &'a Manifest,
    id: &str,
) -> Result<&'a str, Box<dyn std::error::Error>> {
    manifest
        .repositories
        .iter()
        .find(|repository| repository.id == id)
        .map(|repository| repository.commit.as_str())
        .ok_or_else(|| format!("missing repository {id}").into())
}

fn verify_checkout(path: &Path, expected: &str) -> Result<(), Box<dyn std::error::Error>> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(path)
        .output()?;
    let actual = String::from_utf8(output.stdout)?;
    if !output.status.success() || actual.trim() != expected {
        return Err(format!(
            "{} is at {}, expected {expected}",
            path.display(),
            actual.trim()
        )
        .into());
    }
    Ok(())
}

fn api_key(path: &Path) -> Result<String, Box<dyn std::error::Error>> {
    if let Ok(value) = std::env::var("XAI_KEY")
        && !value.trim().is_empty()
    {
        return Ok(value);
    }
    for line in fs::read_to_string(path)?.lines() {
        let line = line.trim().strip_prefix("export ").unwrap_or(line.trim());
        if let Some(value) = line.strip_prefix("XAI_KEY=") {
            let value = value.trim().trim_matches(['\'', '"']);
            if !value.is_empty() {
                return Ok(value.into());
            }
        }
    }
    Err(format!(
        "XAI_KEY was not found in the environment or {}",
        path.display()
    )
    .into())
}

fn retriable_api_error(error: &Value) -> bool {
    let code = error["code"].as_str().unwrap_or_default();
    let message = error["message"].as_str().unwrap_or_default();
    matches!(code, "rate_limit_exceeded" | "server_error")
        || message.contains("rate limit")
        || message.contains("temporarily unavailable")
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), Box<dyn std::error::Error>> {
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(value)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn bound_text(mut value: String, max: usize) -> String {
    if value.len() <= max {
        return value;
    }
    while !value.is_char_boundary(max.min(value.len())) {
        value.pop();
    }
    value.truncate(max);
    value.push_str("\n[tool output bounded]");
    value
}

fn string_argument<'a>(value: &'a Value, key: &str) -> Result<&'a str, Box<dyn std::error::Error>> {
    value[key]
        .as_str()
        .ok_or_else(|| format!("missing string argument {key}").into())
}

fn integer_argument(value: &Value, key: &str, default: usize) -> usize {
    value[key]
        .as_u64()
        .and_then(|number| usize::try_from(number).ok())
        .unwrap_or(default)
}

fn number(value: &Value, key: &str) -> u64 {
    value[key].as_u64().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_the_frozen_matrix_and_filters_without_changing_order() {
        let manifest: Manifest =
            serde_json::from_str(include_str!("../../../../benchmarks/v0.4-holdout.json")).unwrap();
        let args = Args::parse_from([
            "test",
            "--corpus-root",
            "/tmp/corpus",
            "--output",
            "/tmp/output",
        ]);
        let tasks = expand_tasks(&manifest, &args);
        assert_eq!(tasks.len(), 100);
        assert_eq!(tasks[0].id, "ripgrep-navigation");
        assert_eq!(tasks[99].id, "fmt-resume");
    }

    #[test]
    fn packet_accounting_reads_context_headers() {
        let mut accounting = DeliveryAccounting::default();
        account_packet(
            &mut accounting,
            "C1 id=p1 intent=flow index=v4 budget=600 used≈438 delivery=full",
        );
        assert_eq!(accounting.packets_total, 1);
        assert_eq!(accounting.packets_within_budget, 1);
    }

    #[test]
    fn source_reader_rejects_parent_traversal() {
        let root = std::env::temp_dir();
        assert!(
            read_source(&root, "../secret", 1, 2)
                .unwrap()
                .contains("outside")
        );
    }

    #[test]
    fn ordinary_missing_source_is_returned_to_the_model_as_a_tool_error() {
        let args = Args::parse_from([
            "test",
            "--corpus-root",
            "/tmp/corpus",
            "--output",
            "/tmp/output",
        ]);
        let task = ExpandedTask {
            id: "task".into(),
            repository: "repo".into(),
            category: "navigation".into(),
            prompt: "locate".into(),
        };
        let output = execute_tool(
            &args,
            &std::env::temp_dir(),
            &task,
            ArmKind::Baseline,
            "read_source",
            &json!({"path":"definitely-missing.rs","start":1,"end":10}),
        )
        .unwrap();
        assert!(output.starts_with("tool_error read_source failed:"));
    }
}
