use std::{collections::BTreeSet, fs, path::PathBuf};

use clap::Parser;
use serde::{Deserialize, Serialize};

#[derive(Debug, Parser)]
#[command(about = "Verify a reviewed Spectra v0.4 end-to-end evaluation report")]
struct Args {
    report: PathBuf,
    #[arg(long)]
    json: bool,
}

#[derive(Clone, Debug, Deserialize)]
struct Report {
    schema_version: u32,
    environments: Vec<Environment>,
    tasks: Vec<Task>,
    #[serde(default)]
    forbidden_findings: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct Environment {
    id: String,
    harness: String,
    model: String,
}

#[derive(Clone, Debug, Deserialize)]
struct Task {
    id: String,
    repository: String,
    environment_id: String,
    category: String,
    baseline: Arm,
    spectra: Arm,
    packets_within_budget: u64,
    packets_total: u64,
}

#[derive(Clone, Debug, Deserialize)]
struct Arm {
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
struct Summary {
    repositories: usize,
    tasks: usize,
    models: usize,
    harnesses: usize,
    median_input_reduction: f64,
    p75_input_reduction: f64,
    baseline_solve_rate: f64,
    spectra_solve_rate: f64,
    repeated_context_reduction: f64,
    median_spectra_tool_calls: f64,
    packets_within_budget: f64,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let report: Report = serde_json::from_slice(&fs::read(args.report)?)?;
    let summary = evaluate(&report)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        println!(
            "v0.4 gate passed: repositories={} tasks={} median_input_reduction={:.1}% p75_input_reduction={:.1}% solve={:.1}% repeated_context_reduction={:.1}% median_tool_calls={:.1} budget_compliance={:.1}%",
            summary.repositories,
            summary.tasks,
            summary.median_input_reduction * 100.0,
            summary.p75_input_reduction * 100.0,
            summary.spectra_solve_rate * 100.0,
            summary.repeated_context_reduction * 100.0,
            summary.median_spectra_tool_calls,
            summary.packets_within_budget * 100.0,
        );
    }
    Ok(())
}

fn evaluate(report: &Report) -> Result<Summary, Box<dyn std::error::Error>> {
    if report.schema_version != 1 {
        return Err(format!("unsupported report schema {}", report.schema_version).into());
    }
    let repositories = report
        .tasks
        .iter()
        .map(|task| task.repository.as_str())
        .collect::<BTreeSet<_>>();
    let models = report
        .environments
        .iter()
        .map(|environment| environment.model.as_str())
        .collect::<BTreeSet<_>>();
    let harnesses = report
        .environments
        .iter()
        .map(|environment| environment.harness.as_str())
        .collect::<BTreeSet<_>>();
    if repositories.len() < 20 || report.tasks.len() < 100 {
        return Err("v0.4 requires at least 20 repositories and 100 tasks".into());
    }
    if models.len() < 2 || harnesses.len() < 3 {
        return Err("v0.4 requires at least two models across three harnesses".into());
    }
    let environment_ids = report
        .environments
        .iter()
        .map(|environment| environment.id.as_str())
        .collect::<BTreeSet<_>>();
    if environment_ids.len() != report.environments.len()
        || report
            .tasks
            .iter()
            .any(|task| !environment_ids.contains(task.environment_id.as_str()))
    {
        return Err("environment IDs must be unique and every task must reference one".into());
    }
    let categories = report
        .tasks
        .iter()
        .map(|task| task.category.as_str())
        .collect::<BTreeSet<_>>();
    for required in ["navigation", "change-impact", "flow", "repair", "resume"] {
        if !categories.contains(required) {
            return Err(format!("v0.4 report is missing {required} tasks").into());
        }
    }
    if !report.forbidden_findings.is_empty() {
        return Err(format!(
            "privacy scan reported: {}",
            report.forbidden_findings.join(", ")
        )
        .into());
    }
    let unique_ids = report
        .tasks
        .iter()
        .map(|task| task.id.as_str())
        .collect::<BTreeSet<_>>();
    if unique_ids.len() != report.tasks.len() {
        return Err("task IDs must be unique".into());
    }
    if report.tasks.iter().any(|task| {
        task.baseline.input_tokens == 0
            || task.spectra.input_tokens == 0
            || task.baseline.input_text_tokens == 0
            || task.spectra.input_text_tokens == 0
            || task.baseline.output_tokens == 0
            || task.spectra.output_tokens == 0
            || task.baseline.latency_ms == 0
            || task.spectra.latency_ms == 0
            || task.packets_total == 0
    }) {
        return Err(
            "text/input/output token counts, latency, and packet totals must be nonzero".into(),
        );
    }
    for task in &report.tasks {
        for arm in [&task.baseline, &task.spectra] {
            let counted_input = arm.input_schema_tokens + arm.input_text_tokens + arm.image_tokens;
            if counted_input != arm.input_tokens || arm.cached_input_tokens > arm.input_tokens {
                return Err(
                    format!("{} has inconsistent provider input accounting", task.id).into(),
                );
            }
        }
    }

    let baseline_tokens = report
        .tasks
        .iter()
        .map(|task| task.baseline.input_tokens as f64)
        .collect::<Vec<_>>();
    let spectra_tokens = report
        .tasks
        .iter()
        .map(|task| task.spectra.input_tokens as f64)
        .collect::<Vec<_>>();
    let median_input_reduction = reduction(
        percentile(&baseline_tokens, 0.5),
        percentile(&spectra_tokens, 0.5),
    );
    let p75_input_reduction = reduction(
        percentile(&baseline_tokens, 0.75),
        percentile(&spectra_tokens, 0.75),
    );
    let baseline_solve_rate = rate(
        report
            .tasks
            .iter()
            .filter(|task| task.baseline.solved)
            .count(),
        report.tasks.len(),
    );
    let spectra_solve_rate = rate(
        report
            .tasks
            .iter()
            .filter(|task| task.spectra.solved)
            .count(),
        report.tasks.len(),
    );
    let baseline_repeated = report
        .tasks
        .iter()
        .map(|task| task.baseline.repeated_context_bytes)
        .sum::<u64>();
    let spectra_repeated = report
        .tasks
        .iter()
        .map(|task| task.spectra.repeated_context_bytes)
        .sum::<u64>();
    let repeated_context_reduction = reduction(baseline_repeated as f64, spectra_repeated as f64);
    let median_spectra_tool_calls = percentile(
        &report
            .tasks
            .iter()
            .map(|task| task.spectra.tool_calls as f64)
            .collect::<Vec<_>>(),
        0.5,
    );
    let packets_within = report
        .tasks
        .iter()
        .map(|task| task.packets_within_budget)
        .sum::<u64>();
    let packets_total = report
        .tasks
        .iter()
        .map(|task| task.packets_total)
        .sum::<u64>();
    let packets_within_budget = rate(packets_within as usize, packets_total as usize);

    let summary = Summary {
        repositories: repositories.len(),
        tasks: report.tasks.len(),
        models: models.len(),
        harnesses: harnesses.len(),
        median_input_reduction,
        p75_input_reduction,
        baseline_solve_rate,
        spectra_solve_rate,
        repeated_context_reduction,
        median_spectra_tool_calls,
        packets_within_budget,
    };
    if median_input_reduction < 0.35 {
        return Err("median provider-input reduction is below 35%".into());
    }
    if p75_input_reduction < 0.20 {
        return Err("p75 provider-input reduction is below 20%".into());
    }
    if baseline_solve_rate - spectra_solve_rate > 0.02 + f64::EPSILON {
        return Err("Spectra solve rate is more than two points below baseline".into());
    }
    if repeated_context_reduction < 0.70 {
        return Err("repeated-context reduction is below 70%".into());
    }
    if median_spectra_tool_calls > 2.0 {
        return Err("median Spectra tool calls exceed two".into());
    }
    if packets_within_budget < 0.95 {
        return Err("fewer than 95% of packets stayed within budget".into());
    }
    Ok(summary)
}

fn percentile(values: &[f64], percentile: f64) -> f64 {
    let mut values = values.to_vec();
    values.sort_by(f64::total_cmp);
    let index = ((values.len() as f64 * percentile).ceil() as usize)
        .saturating_sub(1)
        .min(values.len().saturating_sub(1));
    values[index]
}

fn reduction(baseline: f64, spectra: f64) -> f64 {
    if baseline == 0.0 {
        0.0
    } else {
        1.0 - spectra / baseline
    }
}

fn rate(part: usize, total: usize) -> f64 {
    part as f64 / total as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn passing_report() -> Report {
        Report {
            schema_version: 1,
            environments: vec![
                Environment {
                    id: "codex-frontier".into(),
                    harness: "codex".into(),
                    model: "frontier".into(),
                },
                Environment {
                    id: "claude-frontier".into(),
                    harness: "claude".into(),
                    model: "frontier".into(),
                },
                Environment {
                    id: "gemini-small".into(),
                    harness: "gemini".into(),
                    model: "small".into(),
                },
            ],
            tasks: (0..100)
                .map(|index| Task {
                    id: format!("task-{index}"),
                    repository: format!("repo-{}", index % 20),
                    environment_id: ["codex-frontier", "claude-frontier", "gemini-small"]
                        [index % 3]
                        .into(),
                    category: ["navigation", "change-impact", "flow", "repair", "resume"]
                        [index % 5]
                        .into(),
                    baseline: Arm {
                        input_tokens: 1_000,
                        input_schema_tokens: 100,
                        input_text_tokens: 900,
                        image_tokens: 0,
                        cached_input_tokens: 200,
                        output_tokens: 100,
                        solved: true,
                        tool_calls: 5,
                        latency_ms: 1_000,
                        repeated_context_bytes: 1_000,
                    },
                    spectra: Arm {
                        input_tokens: 500,
                        input_schema_tokens: 50,
                        input_text_tokens: 450,
                        image_tokens: 0,
                        cached_input_tokens: 100,
                        output_tokens: 100,
                        solved: true,
                        tool_calls: 2,
                        latency_ms: 500,
                        repeated_context_bytes: 200,
                    },
                    packets_within_budget: 1,
                    packets_total: 1,
                })
                .collect(),
            forbidden_findings: Vec::new(),
        }
    }

    #[test]
    fn accepts_a_report_that_meets_every_release_gate() {
        let summary = evaluate(&passing_report()).unwrap();
        assert_eq!(summary.repositories, 20);
        assert_eq!(summary.tasks, 100);
        assert_eq!(summary.median_input_reduction, 0.5);
    }

    #[test]
    fn rejects_privacy_findings_and_regressions() {
        let mut report = passing_report();
        report.forbidden_findings.push("raw session id".into());
        assert!(
            evaluate(&report)
                .unwrap_err()
                .to_string()
                .contains("privacy")
        );
        report.forbidden_findings.clear();
        report.tasks[0].spectra.input_tokens = 2_000;
        for task in &mut report.tasks[1..] {
            task.spectra.input_tokens = 900;
        }
        assert!(evaluate(&report).is_err());
    }
}
