use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use clap::Parser;
use serde::Serialize;
use serde_json::{Value, json};

const SYSTEM_PROMPT: &str = "You are evaluating code-navigation context. Answer only from the supplied context. Explain the architecture path concisely, name the most relevant source paths or visual IDs, and explicitly mark uncertainty.";

#[derive(Debug, Parser)]
#[command(about = "Evaluate saved CodeGraph and Spectra payloads with Grok")]
struct Args {
    /// results.json produced by spectra-bench.
    #[arg(long)]
    results: PathBuf,
    /// Directory for API responses and the evaluation report.
    #[arg(long)]
    output: PathBuf,
    /// Environment file containing XAI_KEY. The process environment wins.
    #[arg(long, default_value = ".env")]
    env_file: PathBuf,
    #[arg(long, default_value = "grok-4.5")]
    model: String,
    /// Evaluate at most this many prompts; zero means all prompts.
    #[arg(long, default_value_t = 1)]
    limit: usize,
    /// Evaluate only these prompt IDs. Repeat the flag to select a representative set.
    #[arg(long = "prompt-id")]
    prompt_ids: Vec<String>,
    /// Image detail sent to Grok. Low is the token-efficient default.
    #[arg(long, default_value = "low", value_parser = ["low", "high", "auto"])]
    image_detail: String,
}

#[derive(Debug, Serialize)]
struct EvalReport {
    schema_version: u32,
    generated_at_unix_ms: u128,
    source_results: String,
    model: String,
    reasoning_effort: &'static str,
    image_detail: String,
    prompts: Vec<PromptEval>,
}

#[derive(Debug, Serialize)]
struct PromptEval {
    repository: String,
    id: String,
    question: String,
    codegraph: ArmEval,
    spectra: ArmEval,
}

#[derive(Debug, Serialize)]
struct ArmEval {
    elapsed_ms: u128,
    input_tokens: u64,
    cached_input_tokens: u64,
    output_tokens: u64,
    reasoning_tokens: u64,
    total_tokens: u64,
    cost_usd: Option<f64>,
    concept_recall_proxy: f64,
    anchor_recall_proxy: f64,
    answer: String,
    raw_response: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let key = api_key(&args.env_file)?;
    let results_root = args.results.parent().unwrap_or(Path::new("."));
    let source: Value = serde_json::from_slice(&fs::read(&args.results)?)?;
    fs::create_dir_all(&args.output)?;

    let mut prompts = Vec::new();
    'repositories: for repository in array(&source, "repositories")? {
        let repository_name = string(repository, "name")?.to_owned();
        for prompt in array(repository, "prompts")? {
            if args.limit != 0 && prompts.len() >= args.limit {
                break 'repositories;
            }
            let id = string(prompt, "id")?.to_owned();
            if !args.prompt_ids.is_empty() && !args.prompt_ids.contains(&id) {
                continue;
            }
            let question = string(prompt, "question")?.to_owned();
            let concepts = strings(prompt, "expected_concepts")?;
            let anchors = strings(prompt, "expected_anchors")?;
            let prompt_dir = args.output.join(&repository_name).join(&id);
            fs::create_dir_all(&prompt_dir)?;

            eprintln!("Evaluating {repository_name}/{id} with {}", args.model);
            let codegraph_path = results_root.join(string(&prompt["codegraph"], "raw_output")?);
            let codegraph_context = fs::read_to_string(&codegraph_path)?;
            let codegraph = evaluate(
                &key,
                &args.model,
                &question,
                &codegraph_context,
                None,
                &args.image_detail,
                &concepts,
                &anchors,
                &prompt_dir.join("codegraph-response.json"),
                &args.output,
            )?;

            let spectra_stdout = string(&prompt["spectra"], "raw_output")?;
            let spectra_metadata = spectra_stdout
                .lines()
                .skip(2)
                .collect::<Vec<_>>()
                .join("\n");
            let png = resolve_artifact(string(&prompt["spectra"], "png_path")?, results_root);
            let spectra = evaluate(
                &key,
                &args.model,
                &question,
                &spectra_metadata,
                Some(&png),
                &args.image_detail,
                &concepts,
                &anchors,
                &prompt_dir.join("spectra-response.json"),
                &args.output,
            )?;

            prompts.push(PromptEval {
                repository: repository_name.clone(),
                id,
                question,
                codegraph,
                spectra,
            });
        }
    }

    let report = EvalReport {
        schema_version: 1,
        generated_at_unix_ms: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
        source_results: args.results.display().to_string(),
        model: args.model,
        reasoning_effort: "low",
        image_detail: args.image_detail,
        prompts,
    };
    let report_path = args.output.join("grok-evaluation.json");
    fs::write(&report_path, serde_json::to_vec_pretty(&report)?)?;
    println!("Wrote {}", report_path.display());
    print_summary(&report);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn evaluate(
    key: &str,
    model: &str,
    question: &str,
    context: &str,
    image: Option<&Path>,
    image_detail: &str,
    concepts: &[String],
    anchors: &[String],
    raw_path: &Path,
    output_root: &Path,
) -> Result<ArmEval, Box<dyn std::error::Error>> {
    if raw_path.exists() {
        let value: Value = serde_json::from_slice(&fs::read(raw_path)?)?;
        return arm_eval(&value, 0, concepts, anchors, raw_path, output_root);
    }
    let text = format!("Question: {question}\n\nContext:\n{context}");
    let content = if let Some(image) = image {
        let encoded = STANDARD.encode(fs::read(image)?);
        json!([
            {"type": "input_image", "image_url": format!("data:image/png;base64,{encoded}"), "detail": image_detail},
            {"type": "input_text", "text": text}
        ])
    } else {
        json!([{"type": "input_text", "text": text}])
    };
    let request = json!({
        "model": model,
        "store": false,
        "reasoning": {"effort": "low"},
        "max_output_tokens": 500,
        "input": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": content}
        ]
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
                fs::write(raw_path, serde_json::to_vec_pretty(&value)?)?;
                return arm_eval(
                    &value,
                    started.elapsed().as_millis(),
                    concepts,
                    anchors,
                    raw_path,
                    output_root,
                );
            }
            if attempt < 3 && retriable_api_error(&value["error"]) {
                let delay = Duration::from_secs(1_u64 << attempt);
                eprintln!("xAI transient error; retrying in {}s", delay.as_secs());
                std::thread::sleep(delay);
                continue;
            }
            return Err(format!("xAI API error: {}", value["error"]).into());
        }
        if attempt < 3 {
            let delay = Duration::from_secs(1_u64 << attempt);
            eprintln!("xAI transport error; retrying in {}s", delay.as_secs());
            std::thread::sleep(delay);
            continue;
        }
        return Err(format!(
            "xAI request failed: {}",
            String::from_utf8_lossy(&response.stderr)
        )
        .into());
    }
    unreachable!("retry loop always returns")
}

fn arm_eval(
    value: &Value,
    elapsed_ms: u128,
    concepts: &[String],
    anchors: &[String],
    raw_path: &Path,
    output_root: &Path,
) -> Result<ArmEval, Box<dyn std::error::Error>> {
    let answer = extract_answer(value);
    let usage = &value["usage"];
    Ok(ArmEval {
        elapsed_ms,
        input_tokens: number(usage, "input_tokens"),
        cached_input_tokens: number(&usage["input_tokens_details"], "cached_tokens"),
        output_tokens: number(usage, "output_tokens"),
        reasoning_tokens: number(&usage["output_tokens_details"], "reasoning_tokens"),
        total_tokens: number(usage, "total_tokens"),
        cost_usd: usage["cost_in_usd_ticks"]
            .as_u64()
            .map(|ticks| ticks as f64 / 10_000_000_000.0),
        concept_recall_proxy: recall(concepts, &answer, false),
        anchor_recall_proxy: recall(anchors, &answer, true),
        answer,
        raw_response: relative(raw_path, output_root),
    })
}

fn retriable_api_error(error: &Value) -> bool {
    let code = error["code"].as_str().unwrap_or_default();
    let message = error["message"].as_str().unwrap_or_default();
    matches!(code, "rate_limit_exceeded" | "server_error")
        || message.contains("rate limit")
        || message.contains("temporarily unavailable")
}

fn api_key(env_file: &Path) -> Result<String, Box<dyn std::error::Error>> {
    if let Ok(key) = std::env::var("XAI_KEY")
        && !key.trim().is_empty()
    {
        return Ok(key);
    }
    for line in fs::read_to_string(env_file)?.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        if let Some(value) = line.strip_prefix("XAI_KEY=") {
            let value = value.trim().trim_matches(['\'', '"']);
            if !value.is_empty() {
                return Ok(value.to_owned());
            }
        }
    }
    Err(format!(
        "XAI_KEY was not found in the environment or {}",
        env_file.display()
    )
    .into())
}

fn resolve_artifact(value: &str, results_root: &Path) -> PathBuf {
    let path = PathBuf::from(value);
    if path.exists() {
        return path;
    }
    let relative = results_root.join(&path);
    if relative.exists() {
        return relative;
    }
    path
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

fn recall(expected: &[String], answer: &str, path_only: bool) -> f64 {
    if expected.is_empty() {
        return 1.0;
    }
    let answer = answer.to_ascii_lowercase();
    let found = expected
        .iter()
        .filter(|item| {
            let needle = if path_only {
                item.split(':').next().unwrap_or(item)
            } else {
                item
            };
            answer.contains(&needle.to_ascii_lowercase())
        })
        .count();
    found as f64 / expected.len() as f64
}

fn array<'a>(value: &'a Value, key: &str) -> Result<&'a [Value], Box<dyn std::error::Error>> {
    value[key]
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| format!("missing array {key}").into())
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, Box<dyn std::error::Error>> {
    value[key]
        .as_str()
        .ok_or_else(|| format!("missing string {key}").into())
}

fn strings(value: &Value, key: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    Ok(array(value, key)?
        .iter()
        .map(|item| item.as_str().unwrap_or_default().to_owned())
        .collect())
}

fn number(value: &Value, key: &str) -> u64 {
    value[key].as_u64().unwrap_or_default()
}

fn relative(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn print_summary(report: &EvalReport) {
    println!(
        "repository,prompt,arm,input_tokens,output_tokens,reasoning_tokens,concept_recall,anchor_recall,cost_usd"
    );
    for prompt in &report.prompts {
        for (name, arm) in [
            ("codegraph", &prompt.codegraph),
            ("spectra", &prompt.spectra),
        ] {
            println!(
                "{},{},{},{},{},{},{:.3},{:.3},{:.6}",
                prompt.repository,
                prompt.id,
                name,
                arm.input_tokens,
                arm.output_tokens,
                arm.reasoning_tokens,
                arm.concept_recall_proxy,
                arm.anchor_recall_proxy,
                arm.cost_usd.unwrap_or_default()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_only_output_text_messages() {
        let response = json!({
            "output": [
                {"type": "reasoning", "content": "hidden"},
                {"type": "message", "content": [
                    {"type": "output_text", "text": "first"},
                    {"type": "refusal", "refusal": "no"},
                    {"type": "output_text", "text": "second"}
                ]}
            ]
        });
        assert_eq!(extract_answer(&response), "first\nsecond");
    }

    #[test]
    fn anchor_recall_uses_paths_not_symbol_suffixes() {
        let expected = vec![
            "src/runtime.rs:Runtime::new".to_owned(),
            "src/task.rs:spawn".to_owned(),
        ];
        assert_eq!(
            recall(&expected, "Inspect src/task.rs near spawn", true),
            0.5
        );
    }

    #[test]
    fn api_key_parser_accepts_export_and_quotes() {
        let root = std::env::temp_dir().join(format!(
            "spectra-grok-env-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&root, "export XAI_KEY='test-key'\n").unwrap();
        assert_eq!(api_key(&root).unwrap(), "test-key");
        fs::remove_file(root).unwrap();
    }
}
