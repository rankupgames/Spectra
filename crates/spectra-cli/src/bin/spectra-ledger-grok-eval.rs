use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use clap::Parser;
use serde::Serialize;
use serde_json::{Value, json};

const SYSTEM: &str = "Recover the current coding-agent state only from the supplied context. Return six concise lines named state, map, anchors, edited, verification, and terminal. Copy identifiers, paths, success status, and blocker/completion wording exactly when present. Mark missing facts unknown.";

#[derive(Debug, Parser)]
#[command(about = "Compare Grok state recovery from transcripts and Ledger projections")]
struct Args {
    #[arg(long)]
    benchmark: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long, default_value = ".env")]
    env_file: PathBuf,
    #[arg(long, default_value = "grok-4.5")]
    model: String,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    generated_at_unix_ms: u128,
    model: String,
    reasoning_effort: &'static str,
    summary: Summary,
    scenarios: Vec<ScenarioEval>,
}

#[derive(Debug, Serialize)]
struct Summary {
    median_input_reduction: f64,
    transcript_mean_fact_recall: f64,
    projection_mean_fact_recall: f64,
    quality_retention: f64,
    transcript_total_cost_usd: f64,
    projection_total_cost_usd: f64,
    cost_reduction: f64,
}

#[derive(Debug, Serialize)]
struct ScenarioEval {
    id: String,
    transcript: ArmEval,
    projection: ArmEval,
}

#[derive(Debug, Serialize)]
struct ArmEval {
    elapsed_ms: u128,
    input_tokens: u64,
    output_tokens: u64,
    reasoning_tokens: u64,
    total_tokens: u64,
    cost_usd: f64,
    fact_recall: f64,
    answer: String,
    raw_response: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let key = api_key(&args.env_file)?;
    let benchmark: Value = serde_json::from_slice(&fs::read(&args.benchmark)?)?;
    fs::create_dir_all(&args.output)?;
    let mut scenarios = Vec::new();
    for scenario in benchmark["scenarios"]
        .as_array()
        .ok_or("benchmark scenarios are missing")?
    {
        let id = scenario["id"].as_str().ok_or("scenario id is missing")?;
        let facts: Vec<String> = scenario["expected_fact_values"]
            .as_array()
            .ok_or("scenario facts are missing")?
            .iter()
            .filter_map(|fact| fact.as_str().map(str::to_owned))
            .collect();
        let scenario_dir = args.output.join(id);
        fs::create_dir_all(&scenario_dir)?;
        eprintln!("Evaluating {id}");
        let transcript = evaluate(
            &key,
            &args.model,
            scenario["transcript"]
                .as_str()
                .ok_or("transcript missing")?,
            &facts,
            &scenario_dir.join("transcript-response.json"),
            &args.output,
        )?;
        let projection = evaluate(
            &key,
            &args.model,
            scenario["projection"]
                .as_str()
                .ok_or("projection missing")?,
            &facts,
            &scenario_dir.join("projection-response.json"),
            &args.output,
        )?;
        scenarios.push(ScenarioEval {
            id: id.into(),
            transcript,
            projection,
        });
    }
    let mut reductions: Vec<_> = scenarios
        .iter()
        .map(|scenario| {
            1.0 - scenario.projection.input_tokens as f64 / scenario.transcript.input_tokens as f64
        })
        .collect();
    reductions.sort_by(f64::total_cmp);
    let transcript_recall = mean(
        scenarios
            .iter()
            .map(|scenario| scenario.transcript.fact_recall),
    );
    let projection_recall = mean(
        scenarios
            .iter()
            .map(|scenario| scenario.projection.fact_recall),
    );
    let transcript_cost: f64 = scenarios
        .iter()
        .map(|scenario| scenario.transcript.cost_usd)
        .sum();
    let projection_cost: f64 = scenarios
        .iter()
        .map(|scenario| scenario.projection.cost_usd)
        .sum();
    let summary = Summary {
        median_input_reduction: reductions[reductions.len() / 2],
        transcript_mean_fact_recall: transcript_recall,
        projection_mean_fact_recall: projection_recall,
        quality_retention: if transcript_recall == 0.0 {
            1.0
        } else {
            projection_recall / transcript_recall
        },
        transcript_total_cost_usd: transcript_cost,
        projection_total_cost_usd: projection_cost,
        cost_reduction: 1.0 - projection_cost / transcript_cost,
    };
    let report = Report {
        schema_version: 1,
        generated_at_unix_ms: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
        model: args.model,
        reasoning_effort: "low",
        summary,
        scenarios,
    };
    let report_path = args.output.join("ledger-grok-evaluation.json");
    fs::write(&report_path, serde_json::to_vec_pretty(&report)?)?;
    fs::write(args.output.join("SUMMARY.md"), markdown(&report))?;
    println!("Wrote {}", report_path.display());
    println!(
        "median_input_reduction={:.1}% transcript_recall={:.1}% projection_recall={:.1}% retention={:.1}%",
        report.summary.median_input_reduction * 100.0,
        report.summary.transcript_mean_fact_recall * 100.0,
        report.summary.projection_mean_fact_recall * 100.0,
        report.summary.quality_retention * 100.0
    );
    Ok(())
}

fn evaluate(
    key: &str,
    model: &str,
    context: &str,
    facts: &[String],
    raw_path: &Path,
    output_root: &Path,
) -> Result<ArmEval, Box<dyn std::error::Error>> {
    if raw_path.exists() {
        let value: Value = serde_json::from_slice(&fs::read(raw_path)?)?;
        return arm_eval(&value, 0, facts, raw_path, output_root);
    }
    let request = json!({
        "model": model,
        "store": false,
        "reasoning": {"effort": "low"},
        "max_output_tokens": 300,
        "input": [
            {"role": "system", "content": SYSTEM},
            {"role": "user", "content": format!("Context:\n{context}")}
        ]
    });
    let bytes = serde_json::to_vec(&request)?;
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
            .write_all(&bytes)?;
        let response = child.wait_with_output()?;
        if response.status.success()
            && let Ok(value) = serde_json::from_slice::<Value>(&response.stdout)
        {
            if value["error"].is_null() {
                fs::write(raw_path, serde_json::to_vec_pretty(&value)?)?;
                return arm_eval(
                    &value,
                    started.elapsed().as_millis(),
                    facts,
                    raw_path,
                    output_root,
                );
            }
            if attempt < 3 && retriable(&value["error"]) {
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
    unreachable!("retry loop always returns")
}

fn arm_eval(
    response: &Value,
    elapsed_ms: u128,
    facts: &[String],
    raw_path: &Path,
    output_root: &Path,
) -> Result<ArmEval, Box<dyn std::error::Error>> {
    let answer = extract_answer(response);
    let usage = &response["usage"];
    Ok(ArmEval {
        elapsed_ms,
        input_tokens: number(usage, "input_tokens"),
        output_tokens: number(usage, "output_tokens"),
        reasoning_tokens: number(&usage["output_tokens_details"], "reasoning_tokens"),
        total_tokens: number(usage, "total_tokens"),
        cost_usd: number(usage, "cost_in_usd_ticks") as f64 / 10_000_000_000.0,
        fact_recall: fact_recall(facts, &answer),
        answer,
        raw_response: relative(raw_path, output_root),
    })
}

fn fact_recall(facts: &[String], answer: &str) -> f64 {
    let answer = answer.to_ascii_lowercase();
    let retained = facts
        .iter()
        .filter(|fact| {
            fact.to_ascii_lowercase()
                .split(|ch: char| !ch.is_alphanumeric() && ch != '/' && ch != '.' && ch != '=')
                .filter(|word| word.len() > 2)
                .all(|word| answer.contains(word))
        })
        .count();
    retained as f64 / facts.len() as f64
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

fn api_key(env_file: &Path) -> Result<String, Box<dyn std::error::Error>> {
    if let Ok(key) = std::env::var("XAI_KEY") {
        if !key.trim().is_empty() {
            return Ok(key);
        }
    }
    for line in fs::read_to_string(env_file)?.lines() {
        let line = line.trim().strip_prefix("export ").unwrap_or(line.trim());
        if let Some(value) = line.strip_prefix("XAI_KEY=") {
            let value = value.trim().trim_matches(['\'', '"']);
            if !value.is_empty() {
                return Ok(value.into());
            }
        }
    }
    Err("XAI_KEY was not found".into())
}

fn retriable(error: &Value) -> bool {
    let code = error["code"].as_str().unwrap_or_default();
    let message = error["message"].as_str().unwrap_or_default();
    matches!(code, "rate_limit_exceeded" | "server_error")
        || message.contains("rate limit")
        || message.contains("temporarily unavailable")
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

fn mean(values: impl Iterator<Item = f64>) -> f64 {
    let values: Vec<_> = values.collect();
    values.iter().sum::<f64>() / values.len() as f64
}

fn markdown(report: &Report) -> String {
    let mut text = String::from(
        "# Grok 4.5 State Machine Ledger benchmark\n\n\
         Each scenario asks the same model to recover current agent state from either the replayed transcript or the bounded Ledger projection.\n\n\
         | Scenario | Transcript input | Projection input | Reduction | Transcript facts | Projection facts |\n\
         | --- | ---: | ---: | ---: | ---: | ---: |\n",
    );
    for scenario in &report.scenarios {
        let reduction =
            1.0 - scenario.projection.input_tokens as f64 / scenario.transcript.input_tokens as f64;
        text.push_str(&format!(
            "| {} | {} | {} | {:.1}% | {:.1}% | {:.1}% |\n",
            scenario.id,
            scenario.transcript.input_tokens,
            scenario.projection.input_tokens,
            reduction * 100.0,
            scenario.transcript.fact_recall * 100.0,
            scenario.projection.fact_recall * 100.0
        ));
    }
    text.push_str(&format!(
        "\nMedian provider-input reduction: **{:.1}%**. Mean fact recall: **{:.1}% transcript / {:.1}% projection**. Quality retention: **{:.1}%**. Total cost: **${:.6} transcript / ${:.6} projection**, a **{:.1}% reduction**.\n",
        report.summary.median_input_reduction * 100.0,
        report.summary.transcript_mean_fact_recall * 100.0,
        report.summary.projection_mean_fact_recall * 100.0,
        report.summary.quality_retention * 100.0,
        report.summary.transcript_total_cost_usd,
        report.summary.projection_total_cost_usd,
        report.summary.cost_reduction * 100.0
    ));
    text
}
