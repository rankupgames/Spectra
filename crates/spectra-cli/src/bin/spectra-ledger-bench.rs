use std::{
    fs,
    path::PathBuf,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use clap::Parser;
use serde::Serialize;
use spectra_core::{LedgerAnchor, LedgerEventKind, LedgerState, LedgerStore};

#[derive(Debug, Parser)]
#[command(about = "Benchmark transcript replay against Spectra Ledger projections")]
struct Args {
    #[arg(long)]
    output: PathBuf,
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u16).range(1..=100))]
    repeats: u16,
}

#[derive(Clone)]
struct Scenario {
    id: &'static str,
    transcript: String,
    events: Vec<LedgerEventKind>,
    expected_state: LedgerState,
    expected_facts: Vec<&'static str>,
    forbidden_persisted: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    generated_at_unix_ms: u128,
    repeats: u16,
    summary: Summary,
    scenarios: Vec<ScenarioResult>,
}

#[derive(Debug, Serialize)]
struct Summary {
    median_token_reduction: f64,
    minimum_fact_recall: f64,
    maximum_projection_tokens: usize,
    median_append_micros: u128,
    median_replay_micros: u128,
    all_replays_deterministic: bool,
    all_secrets_redacted: bool,
}

#[derive(Debug, Serialize)]
struct ScenarioResult {
    id: String,
    events: usize,
    final_state: LedgerState,
    transcript_bytes: usize,
    estimated_transcript_tokens: usize,
    projection_bytes: usize,
    estimated_projection_tokens: usize,
    token_reduction: f64,
    retained_facts: usize,
    expected_facts: usize,
    fact_recall: f64,
    ledger_bytes: usize,
    median_append_micros: u128,
    median_replay_micros: u128,
    replay_deterministic: bool,
    secrets_redacted: bool,
    expected_fact_values: Vec<String>,
    transcript: String,
    projection: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    fs::create_dir_all(&args.output)?;
    let mut results = Vec::new();
    for scenario in scenarios() {
        results.push(run_scenario(&scenario, args.repeats)?);
    }
    let mut reductions: Vec<_> = results
        .iter()
        .map(|result| result.token_reduction)
        .collect();
    reductions.sort_by(f64::total_cmp);
    let summary = Summary {
        median_token_reduction: reductions[reductions.len() / 2],
        minimum_fact_recall: results
            .iter()
            .map(|result| result.fact_recall)
            .fold(1.0_f64, f64::min),
        maximum_projection_tokens: results
            .iter()
            .map(|result| result.estimated_projection_tokens)
            .max()
            .unwrap_or_default(),
        median_append_micros: median(
            &results
                .iter()
                .map(|result| result.median_append_micros)
                .collect::<Vec<_>>(),
        ),
        median_replay_micros: median(
            &results
                .iter()
                .map(|result| result.median_replay_micros)
                .collect::<Vec<_>>(),
        ),
        all_replays_deterministic: results.iter().all(|result| result.replay_deterministic),
        all_secrets_redacted: results.iter().all(|result| result.secrets_redacted),
    };
    let report = Report {
        schema_version: 1,
        generated_at_unix_ms: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
        repeats: args.repeats,
        summary,
        scenarios: results,
    };
    let json_path = args.output.join("ledger-benchmark.json");
    fs::write(&json_path, serde_json::to_vec_pretty(&report)?)?;
    let summary_path = args.output.join("SUMMARY.md");
    fs::write(&summary_path, markdown(&report))?;
    println!("Wrote {}", json_path.display());
    println!("Wrote {}", summary_path.display());
    println!(
        "median_reduction={:.1}% min_fact_recall={:.1}% max_projection_tokens={} append={}us replay={}us",
        report.summary.median_token_reduction * 100.0,
        report.summary.minimum_fact_recall * 100.0,
        report.summary.maximum_projection_tokens,
        report.summary.median_append_micros,
        report.summary.median_replay_micros
    );
    Ok(())
}

fn run_scenario(
    scenario: &Scenario,
    repeats: u16,
) -> Result<ScenarioResult, Box<dyn std::error::Error>> {
    let mut append_samples = Vec::new();
    let mut replay_samples = Vec::new();
    let mut final_projection = None;
    let mut final_ledger_bytes = 0;
    let mut deterministic = true;
    let mut redacted = true;
    for repeat in 0..repeats {
        let root = temp_project(scenario.id, repeat);
        let mut store = LedgerStore::open(&root)?;
        let started = Instant::now();
        for event in &scenario.events {
            store.append(event.clone())?;
        }
        append_samples.push(started.elapsed().as_micros());
        let projection = store.projection();
        let ledger_path = root.join(".spectra/ledger-v1.jsonl");
        let persisted = fs::read_to_string(&ledger_path)?;
        final_ledger_bytes = persisted.len();
        redacted &= scenario
            .forbidden_persisted
            .iter()
            .all(|secret| !persisted.contains(secret));
        let replay_started = Instant::now();
        let replay = LedgerStore::open(&root)?;
        replay_samples.push(replay_started.elapsed().as_micros());
        deterministic &= replay.events() == store.events()
            && replay.state() == scenario.expected_state
            && replay.projection() == projection;
        if repeat == 0 {
            final_projection = Some(projection);
        }
        fs::remove_dir_all(root)?;
    }
    let projection = final_projection.expect("at least one repeat");
    let transcript_tokens = estimate_tokens(&scenario.transcript);
    let retained = scenario
        .expected_facts
        .iter()
        .filter(|fact| projection.text.contains(**fact))
        .count();
    let fact_recall = retained as f64 / scenario.expected_facts.len() as f64;
    Ok(ScenarioResult {
        id: scenario.id.into(),
        events: scenario.events.len(),
        final_state: projection.state,
        transcript_bytes: scenario.transcript.len(),
        estimated_transcript_tokens: transcript_tokens,
        projection_bytes: projection.text.len(),
        estimated_projection_tokens: projection.estimated_tokens,
        token_reduction: 1.0 - projection.estimated_tokens as f64 / transcript_tokens as f64,
        retained_facts: retained,
        expected_facts: scenario.expected_facts.len(),
        fact_recall,
        ledger_bytes: final_ledger_bytes,
        median_append_micros: median(&append_samples),
        median_replay_micros: median(&replay_samples),
        replay_deterministic: deterministic,
        secrets_redacted: redacted,
        expected_fact_values: scenario
            .expected_facts
            .iter()
            .map(|fact| (*fact).to_owned())
            .collect(),
        transcript: scenario.transcript.clone(),
        projection: projection.text,
    })
}

fn scenarios() -> Vec<Scenario> {
    vec![successful_edit(), repaired_failure(), blocked_session()]
}

fn base_events() -> Vec<LedgerEventKind> {
    vec![
        LedgerEventKind::RepositorySynced {
            files: 42,
            changed: 1,
            removed: 0,
            nodes: 310,
            edges: 488,
        },
        LedgerEventKind::MapRendered {
            map_id: "topology-parser".into(),
            query: "How does parsing reach validation?".into(),
            anchors: vec![
                LedgerAnchor {
                    visual_id: "N1".into(),
                    path: "src/parser.rs".into(),
                    start_line: 12,
                    end_line: 48,
                },
                LedgerAnchor {
                    visual_id: "N2".into(),
                    path: "src/validate.rs".into(),
                    start_line: 7,
                    end_line: 35,
                },
            ],
            nodes: 38,
            truncated: false,
        },
    ]
}

fn edit_cycle(path: &str, success: bool, output_bytes: usize) -> Vec<LedgerEventKind> {
    vec![
        LedgerEventKind::AuthorizationRequested {
            action: format!("edit {path}"),
        },
        LedgerEventKind::EditAuthorized {
            action: format!("edit {path}"),
        },
        LedgerEventKind::EditApplied {
            paths: vec![path.into()],
        },
        LedgerEventKind::VerificationStarted {
            command: "cargo test --workspace".into(),
        },
        LedgerEventKind::VerificationFinished {
            command: "cargo test --workspace".into(),
            success,
            exit_code: Some(if success { 0 } else { 101 }),
            output_bytes,
        },
    ]
}

fn successful_edit() -> Scenario {
    let mut events = base_events();
    events.extend(edit_cycle("src/parser.rs", true, 18_420));
    events.push(LedgerEventKind::Completed {
        summary: "parser validation fixed; tests pass".into(),
    });
    Scenario {
        id: "successful_edit",
        transcript: verbose_transcript("All 27 tests passed", 72),
        events,
        expected_state: LedgerState::Complete,
        expected_facts: vec![
            "Complete",
            "N1=src/parser.rs:12",
            "edit src/parser.rs",
            "success=true",
            "complete parser validation fixed",
        ],
        forbidden_persisted: vec![],
    }
}

fn repaired_failure() -> Scenario {
    let mut events = base_events();
    events.extend(edit_cycle("src/parser.rs", false, 31_004));
    events.push(LedgerEventKind::Blocked {
        reason: "parser fixture failed at line 88".into(),
    });
    events.extend(edit_cycle("src/parser.rs", true, 19_882));
    events.push(LedgerEventKind::Completed {
        summary: "repair applied; workspace green".into(),
    });
    Scenario {
        id: "failed_then_repaired",
        transcript: verbose_transcript("Initial failure repaired; 27 tests passed", 120),
        events,
        expected_state: LedgerState::Complete,
        expected_facts: vec![
            "Complete",
            "N1=src/parser.rs:12",
            "edit src/parser.rs",
            "success=true",
            "complete repair applied",
        ],
        forbidden_persisted: vec![],
    }
}

fn blocked_session() -> Scenario {
    let mut events = base_events();
    events.extend(vec![
        LedgerEventKind::AuthorizationRequested {
            action: "edit src/validate.rs".into(),
        },
        LedgerEventKind::EditAuthorized {
            action: "edit src/validate.rs".into(),
        },
        LedgerEventKind::EditApplied {
            paths: vec!["src/validate.rs".into()],
        },
        LedgerEventKind::VerificationStarted {
            command: "XAI_KEY=benchmark-secret cargo test".into(),
        },
        LedgerEventKind::VerificationFinished {
            command: "XAI_KEY=benchmark-secret cargo test".into(),
            success: false,
            exit_code: Some(101),
            output_bytes: 44_120,
        },
        LedgerEventKind::Blocked {
            reason: "requires upstream schema decision".into(),
        },
    ]);
    Scenario {
        id: "blocked_with_secret",
        transcript: verbose_transcript("Blocked on upstream schema decision", 90),
        events,
        expected_state: LedgerState::Blocked,
        expected_facts: vec![
            "Blocked",
            "N1=src/parser.rs:12",
            "edit src/validate.rs",
            "success=false",
            "blocked requires upstream schema decision",
        ],
        forbidden_persisted: vec!["benchmark-secret"],
    }
}

fn verbose_transcript(outcome: &str, repeated_lines: usize) -> String {
    let terminal = (0..repeated_lines)
        .map(|index| format!("test parser::fixture_{index:02} ... ok\n"))
        .collect::<String>();
    format!(
        "User: Inspect the parser architecture and fix validation.\n\
         Assistant: I mapped the repository and selected src/parser.rs:12-48 and src/validate.rs:7-35.\n\
         Tool: Authorization requested for editing parser validation.\n\
         User: Approved.\n\
         Assistant: Applied the edit and started cargo test --workspace.\n\
         Terminal:\n{terminal}\
         Terminal summary: {outcome}. The command output contained repeated build and test status lines.\n\
         Assistant: The relevant map was topology-parser, N1 parser and N2 validation. Final state recorded.\n"
    )
}

fn temp_project(label: &str, repeat: u16) -> PathBuf {
    std::env::temp_dir().join(format!(
        "spectra-ledger-bench-{label}-{}-{repeat}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

fn median(values: &[u128]) -> u128 {
    let mut values = values.to_vec();
    values.sort_unstable();
    values[values.len() / 2]
}

fn markdown(report: &Report) -> String {
    let mut text = String::from(
        "# State Machine Ledger deterministic benchmark\n\n\
         The baseline is a replayed conversational/terminal transcript. Spectra receives only the immutable Ledger projection. Token counts use the same four-characters-per-token estimate as the topology payload benchmark.\n\n\
         | Scenario | Transcript tokens | Projection tokens | Reduction | Fact recall | Final state |\n\
         | --- | ---: | ---: | ---: | ---: | --- |\n",
    );
    for scenario in &report.scenarios {
        text.push_str(&format!(
            "| {} | {} | {} | {:.1}% | {:.1}% | {:?} |\n",
            scenario.id,
            scenario.estimated_transcript_tokens,
            scenario.estimated_projection_tokens,
            scenario.token_reduction * 100.0,
            scenario.fact_recall * 100.0,
            scenario.final_state
        ));
    }
    text.push_str(&format!(
        "\nMedian token reduction: **{:.1}%**. Minimum fact recall: **{:.1}%**. Maximum projection: **{} tokens**. Median append/replay latency: **{}/{} µs**. Deterministic replay: **{}**. Secret redaction: **{}**.\n",
        report.summary.median_token_reduction * 100.0,
        report.summary.minimum_fact_recall * 100.0,
        report.summary.maximum_projection_tokens,
        report.summary.median_append_micros,
        report.summary.median_replay_micros,
        report.summary.all_replays_deterministic,
        report.summary.all_secrets_redacted
    ));
    text
}
