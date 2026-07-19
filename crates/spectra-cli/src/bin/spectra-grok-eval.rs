use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use clap::{Parser, ValueEnum};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const SYSTEM_PROMPT: &str = "You are evaluating code-navigation context. Answer only from the supplied context. Explain the architecture path concisely, name the most relevant source paths or visual IDs, and explicitly mark uncertainty.";
const REASONING_EFFORT: &str = "low";
const MAX_OUTPUT_TOKENS: u64 = 500;
const OPENROUTER_TEMPERATURE: u8 = 0;
const OPENROUTER_TOP_P: u8 = 1;
const OPENROUTER_SEED: u64 = 20_260_715;
const MIN_PROVIDER_INPUT_REDUCTION: f64 = 0.883;
const MIN_COMPOSITE_RETENTION: f64 = 1.18;
const MIN_SPECTRA_ANCHOR_RECALL: f64 = 0.852;
const BINDING_FILE: &str = "evaluation-binding.json";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
enum Provider {
    Xai,
    Openrouter,
}

impl Provider {
    fn name(self) -> &'static str {
        match self {
            Self::Xai => "xai",
            Self::Openrouter => "openrouter",
        }
    }

    fn api(self) -> &'static str {
        match self {
            Self::Xai => "responses-v1",
            Self::Openrouter => "chat-completions-v1",
        }
    }

    fn endpoint(self) -> &'static str {
        match self {
            Self::Xai => "https://api.x.ai/v1/responses",
            Self::Openrouter => "https://openrouter.ai/api/v1/chat/completions",
        }
    }

    fn default_model(self) -> &'static str {
        match self {
            Self::Xai => "grok-4.5",
            Self::Openrouter => "x-ai/grok-4.5",
        }
    }

    fn key_name(self) -> &'static str {
        match self {
            Self::Xai => "XAI_KEY",
            Self::Openrouter => "OPENROUTER_API_KEY",
        }
    }
}

#[derive(Debug, Parser)]
#[command(about = "Evaluate saved CodeGraph and Spectra payloads with Grok")]
struct Args {
    /// results.json produced by spectra-bench.
    #[arg(long)]
    results: PathBuf,
    /// Directory for API responses and the evaluation report.
    #[arg(long)]
    output: PathBuf,
    /// Environment file containing the selected provider's API key. The process environment wins.
    #[arg(long, default_value = ".env")]
    env_file: PathBuf,
    /// API provider. Direct xAI remains the default historical benchmark path.
    #[arg(long, value_enum, default_value_t = Provider::Xai)]
    provider: Provider,
    /// Provider model ID. Defaults to grok-4.5 for xAI and x-ai/grok-4.5 for OpenRouter.
    #[arg(long)]
    model: Option<String>,
    /// Evaluate at most this many prompts; zero means all prompts.
    #[arg(long, default_value_t = 1)]
    limit: usize,
    /// Evaluate only these prompt IDs. Repeat the flag to select a representative set.
    #[arg(long = "prompt-id")]
    prompt_ids: Vec<String>,
    /// Image detail sent to Grok. Low is the token-efficient default.
    #[arg(long, default_value = "low", value_parser = ["low", "high", "auto"])]
    image_detail: String,
    /// Require a new output directory instead of resuming matching bound responses.
    #[arg(long)]
    fresh_output: bool,
    /// Reuse the exact CodeGraph provider responses from a content-compatible report.
    /// This makes direct and compatibility renderer comparisons paired without
    /// issuing or cherry-picking another control response.
    #[arg(long)]
    reference_codegraph_report: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct EvaluationBinding {
    source_results: String,
    source_results_sha256: String,
    provider_input_bundle_sha256: String,
    codegraph_control_bundle_sha256: String,
    reference_codegraph_report_sha256: Option<String>,
    provider: Provider,
    api: String,
    endpoint: String,
    model: String,
    system_prompt: String,
    reasoning_effort: String,
    image_detail: String,
    max_output_tokens: u64,
    temperature: Option<u8>,
    top_p: Option<u8>,
    seed: Option<u64>,
    provider_fallbacks: bool,
    require_all_parameters: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct EvalReport {
    schema_version: u32,
    generated_at_unix_ms: u128,
    binding: EvaluationBinding,
    fresh_output: bool,
    summary: EvaluationSummary,
    prompts: Vec<PromptEval>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct EvaluationSummary {
    codegraph: ArmSummary,
    spectra: ArmSummary,
    provider_input_reduction: ThresholdResult,
    composite_retention: ThresholdResult,
    spectra_anchor_recall: ThresholdResult,
    all_published_topology_thresholds_passed: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ArmSummary {
    median_input_tokens: u64,
    mean_concept_recall: f64,
    mean_anchor_recall: f64,
    composite_recall: f64,
    total_cost_usd: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ThresholdResult {
    actual: f64,
    minimum: f64,
    passed: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PromptEval {
    repository: String,
    id: String,
    question: String,
    codegraph: ArmEval,
    spectra: ArmEval,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ArmEval {
    elapsed_ms: u128,
    response_model: String,
    upstream_provider: Option<String>,
    input_tokens: u64,
    cached_input_tokens: Option<u64>,
    output_tokens: u64,
    reasoning_tokens: Option<u64>,
    total_tokens: u64,
    cost_usd: Option<f64>,
    concept_recall_proxy: f64,
    anchor_recall_proxy: f64,
    answer: String,
    raw_response: String,
}

struct ReferenceCodegraphResponses {
    report_sha256: String,
    responses: BTreeMap<(String, String), Vec<u8>>,
}

fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    format!("sha256:{encoded}")
}

fn update_hash_part(hasher: &mut Sha256, label: &str, bytes: &[u8]) {
    hasher.update((label.len() as u64).to_le_bytes());
    hasher.update(label.as_bytes());
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn finish_hash(hasher: Sha256) -> String {
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    format!("sha256:{encoded}")
}

fn hash_prompt_identity(
    hasher: &mut Sha256,
    repository_name: &str,
    prompt: &Value,
) -> Result<(), Box<dyn std::error::Error>> {
    update_hash_part(hasher, "repository", repository_name.as_bytes());
    update_hash_part(hasher, "prompt-id", string(prompt, "id")?.as_bytes());
    update_hash_part(hasher, "question", string(prompt, "question")?.as_bytes());
    for concept in strings(prompt, "expected_concepts")? {
        update_hash_part(hasher, "expected-concept", concept.as_bytes());
    }
    for anchor in strings(prompt, "expected_anchors")? {
        update_hash_part(hasher, "expected-anchor", anchor.as_bytes());
    }
    Ok(())
}

fn codegraph_control_bundle_sha256(
    source: &Value,
    results_root: &Path,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut hasher = Sha256::new();
    update_hash_part(
        &mut hasher,
        "domain",
        b"spectra-grok-eval-codegraph-control-v1",
    );
    for repository in array(source, "repositories")? {
        let repository_name = string(repository, "name")?;
        for prompt in array(repository, "prompts")? {
            hash_prompt_identity(&mut hasher, repository_name, prompt)?;
            let path = resolve_artifact(string(&prompt["codegraph"], "raw_output")?, results_root);
            update_hash_part(&mut hasher, "codegraph-context", &fs::read(path)?);
        }
    }
    Ok(finish_hash(hasher))
}

fn provider_input_bundle_sha256(
    source: &Value,
    results_root: &Path,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut hasher = Sha256::new();
    update_hash_part(
        &mut hasher,
        "domain",
        b"spectra-grok-eval-provider-input-v1",
    );
    for repository in array(source, "repositories")? {
        let repository_name = string(repository, "name")?;
        for prompt in array(repository, "prompts")? {
            hash_prompt_identity(&mut hasher, repository_name, prompt)?;
            let codegraph_path =
                resolve_artifact(string(&prompt["codegraph"], "raw_output")?, results_root);
            update_hash_part(&mut hasher, "codegraph-context", &fs::read(codegraph_path)?);
            let spectra_stdout = string(&prompt["spectra"], "raw_output")?;
            let spectra_metadata = spectra_stdout
                .lines()
                .skip(2)
                .collect::<Vec<_>>()
                .join("\n");
            update_hash_part(&mut hasher, "spectra-metadata", spectra_metadata.as_bytes());
            let png = resolve_artifact(string(&prompt["spectra"], "png_path")?, results_root);
            update_hash_part(&mut hasher, "spectra-png", &fs::read(png)?);
        }
    }
    Ok(finish_hash(hasher))
}

fn load_reference_codegraph(
    path: &Path,
    provider: Provider,
    model: &str,
    image_detail: &str,
    expected_control_bundle_sha256: &str,
) -> Result<ReferenceCodegraphResponses, Box<dyn std::error::Error>> {
    let bytes = fs::read(path)?;
    let report: EvalReport = serde_json::from_slice(&bytes)?;
    let binding = &report.binding;
    let expected_temperature = (provider == Provider::Openrouter).then_some(OPENROUTER_TEMPERATURE);
    let expected_top_p = (provider == Provider::Openrouter).then_some(OPENROUTER_TOP_P);
    let expected_seed = (provider == Provider::Openrouter).then_some(OPENROUTER_SEED);
    if binding.provider != provider
        || binding.api != provider.api()
        || binding.endpoint != provider.endpoint()
        || binding.model != model
        || binding.system_prompt != SYSTEM_PROMPT
        || binding.reasoning_effort != REASONING_EFFORT
        || binding.image_detail != image_detail
        || binding.max_output_tokens != MAX_OUTPUT_TOKENS
        || binding.temperature != expected_temperature
        || binding.top_p != expected_top_p
        || binding.seed != expected_seed
        || binding.provider_fallbacks
        || binding.require_all_parameters != (provider == Provider::Openrouter)
        || binding.codegraph_control_bundle_sha256 != expected_control_bundle_sha256
    {
        return Err(format!(
            "reference CodeGraph report is not content- and settings-compatible: {}",
            path.display()
        )
        .into());
    }

    let report_root = path.parent().unwrap_or(Path::new("."));
    let mut responses = BTreeMap::new();
    for prompt in report.prompts {
        let response_path = resolve_artifact(&prompt.codegraph.raw_response, report_root);
        let key = (prompt.repository, prompt.id);
        if responses
            .insert(key.clone(), fs::read(response_path)?)
            .is_some()
        {
            return Err(format!(
                "reference CodeGraph report contains duplicate response for {}/{}",
                key.0, key.1
            )
            .into());
        }
    }
    Ok(ReferenceCodegraphResponses {
        report_sha256: sha256(&bytes),
        responses,
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let model = args
        .model
        .clone()
        .unwrap_or_else(|| args.provider.default_model().to_owned());
    let results_bytes = fs::read(&args.results)?;
    let source: Value = serde_json::from_slice(&results_bytes)?;
    let results_root = args.results.parent().unwrap_or(Path::new("."));
    let provider_input_bundle_sha256 = provider_input_bundle_sha256(&source, results_root)?;
    let codegraph_control_bundle_sha256 = codegraph_control_bundle_sha256(&source, results_root)?;
    let reference = args
        .reference_codegraph_report
        .as_deref()
        .map(|path| {
            load_reference_codegraph(
                path,
                args.provider,
                &model,
                &args.image_detail,
                &codegraph_control_bundle_sha256,
            )
        })
        .transpose()?;
    let binding = EvaluationBinding {
        source_results: args.results.display().to_string(),
        source_results_sha256: sha256(&results_bytes),
        provider_input_bundle_sha256,
        codegraph_control_bundle_sha256,
        reference_codegraph_report_sha256: reference
            .as_ref()
            .map(|reference| reference.report_sha256.clone()),
        provider: args.provider,
        api: args.provider.api().to_owned(),
        endpoint: args.provider.endpoint().to_owned(),
        model: model.clone(),
        system_prompt: SYSTEM_PROMPT.to_owned(),
        reasoning_effort: REASONING_EFFORT.to_owned(),
        image_detail: args.image_detail.clone(),
        max_output_tokens: MAX_OUTPUT_TOKENS,
        temperature: (args.provider == Provider::Openrouter).then_some(OPENROUTER_TEMPERATURE),
        top_p: (args.provider == Provider::Openrouter).then_some(OPENROUTER_TOP_P),
        seed: (args.provider == Provider::Openrouter).then_some(OPENROUTER_SEED),
        provider_fallbacks: false,
        require_all_parameters: args.provider == Provider::Openrouter,
    };
    let key = api_key(&args.env_file, args.provider)?;
    prepare_output(&args.output, &binding, args.fresh_output)?;

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

            eprintln!(
                "Evaluating {repository_name}/{id} with {}/{}",
                args.provider.name(),
                model
            );
            let codegraph_response_path = prompt_dir.join("codegraph-response.json");
            let codegraph = if let Some(reference) = &reference {
                let raw = reference
                    .responses
                    .get(&(repository_name.clone(), id.clone()))
                    .ok_or_else(|| {
                        format!(
                            "reference CodeGraph report has no response for {repository_name}/{id}"
                        )
                    })?;
                let value: Value = serde_json::from_slice(raw)?;
                let evaluation = arm_eval(
                    args.provider,
                    &value,
                    0,
                    &model,
                    &concepts,
                    &anchors,
                    &codegraph_response_path,
                    &args.output,
                )?;
                fs::write(&codegraph_response_path, raw)?;
                evaluation
            } else {
                let codegraph_path = results_root.join(string(&prompt["codegraph"], "raw_output")?);
                let codegraph_context = fs::read_to_string(&codegraph_path)?;
                evaluate(
                    args.provider,
                    &key,
                    &model,
                    &question,
                    &codegraph_context,
                    None,
                    &args.image_detail,
                    &concepts,
                    &anchors,
                    &codegraph_response_path,
                    &args.output,
                )?
            };

            let spectra_stdout = string(&prompt["spectra"], "raw_output")?;
            let spectra_metadata = spectra_stdout
                .lines()
                .skip(2)
                .collect::<Vec<_>>()
                .join("\n");
            let png = resolve_artifact(string(&prompt["spectra"], "png_path")?, results_root);
            let spectra = evaluate(
                args.provider,
                &key,
                &model,
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

    let summary = evaluation_summary(&prompts)?;
    let report = EvalReport {
        schema_version: 2,
        generated_at_unix_ms: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
        binding,
        fresh_output: args.fresh_output,
        summary,
        prompts,
    };
    let report_path = args.output.join("grok-evaluation.json");
    fs::write(&report_path, serde_json::to_vec_pretty(&report)?)?;
    println!("Wrote {}", report_path.display());
    print_summary(&report);
    Ok(())
}

fn evaluation_summary(
    prompts: &[PromptEval],
) -> Result<EvaluationSummary, Box<dyn std::error::Error>> {
    if prompts.is_empty() {
        return Err("model evaluation selected no prompts".into());
    }
    let summarize = |select: fn(&PromptEval) -> &ArmEval| {
        let arms = prompts.iter().map(select).collect::<Vec<_>>();
        let mut input_tokens = arms.iter().map(|arm| arm.input_tokens).collect::<Vec<_>>();
        input_tokens.sort_unstable();
        let mean_concept_recall =
            arms.iter().map(|arm| arm.concept_recall_proxy).sum::<f64>() / arms.len() as f64;
        let mean_anchor_recall =
            arms.iter().map(|arm| arm.anchor_recall_proxy).sum::<f64>() / arms.len() as f64;
        let total_cost_usd = arms
            .iter()
            .map(|arm| arm.cost_usd)
            .collect::<Option<Vec<_>>>()
            .map(|costs| costs.into_iter().sum());
        ArmSummary {
            median_input_tokens: input_tokens[input_tokens.len() / 2],
            mean_concept_recall,
            mean_anchor_recall,
            composite_recall: (mean_concept_recall + mean_anchor_recall) / 2.0,
            total_cost_usd,
        }
    };
    let codegraph = summarize(|prompt| &prompt.codegraph);
    let spectra = summarize(|prompt| &prompt.spectra);
    if codegraph.median_input_tokens == 0 || codegraph.composite_recall == 0.0 {
        return Err("CodeGraph reference metrics cannot have a zero denominator".into());
    }
    let provider_input_reduction =
        1.0 - spectra.median_input_tokens as f64 / codegraph.median_input_tokens as f64;
    let composite_retention = spectra.composite_recall / codegraph.composite_recall;
    let provider_input_reduction =
        threshold(provider_input_reduction, MIN_PROVIDER_INPUT_REDUCTION);
    let composite_retention = threshold(composite_retention, MIN_COMPOSITE_RETENTION);
    let spectra_anchor_recall = threshold(spectra.mean_anchor_recall, MIN_SPECTRA_ANCHOR_RECALL);
    let all_published_topology_thresholds_passed = provider_input_reduction.passed
        && composite_retention.passed
        && spectra_anchor_recall.passed;
    Ok(EvaluationSummary {
        codegraph,
        spectra,
        provider_input_reduction,
        composite_retention,
        spectra_anchor_recall,
        all_published_topology_thresholds_passed,
    })
}

fn threshold(actual: f64, minimum: f64) -> ThresholdResult {
    ThresholdResult {
        actual,
        minimum,
        passed: actual >= minimum,
    }
}

#[allow(clippy::too_many_arguments)]
fn evaluate(
    provider: Provider,
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
        return arm_eval(
            provider,
            &value,
            0,
            model,
            concepts,
            anchors,
            raw_path,
            output_root,
        );
    }
    let text = format!("Question: {question}\n\nContext:\n{context}");
    let image_data_url = image
        .map(fs::read)
        .transpose()?
        .map(|bytes| format!("data:image/png;base64,{}", STANDARD.encode(bytes)));
    let request = request_body(
        provider,
        model,
        &text,
        image_data_url.as_deref(),
        image_detail,
    );

    let request_bytes = serde_json::to_vec(&request)?;
    let started = Instant::now();
    for attempt in 0..=3_u32 {
        let mut child = Command::new("curl")
            .args(["-sS", provider.endpoint(), "-m", "3600"])
            .args(["-H", "Content-Type: application/json"])
            .args(["-H", &format!("Authorization: Bearer {key}")])
            .args(["--data-binary", "@-"])
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
                let evaluation = arm_eval(
                    provider,
                    &value,
                    started.elapsed().as_millis(),
                    model,
                    concepts,
                    anchors,
                    raw_path,
                    output_root,
                )?;
                fs::write(raw_path, serde_json::to_vec_pretty(&value)?)?;
                return Ok(evaluation);
            }
            if attempt < 3 && retriable_api_error(&value["error"]) {
                let delay = Duration::from_secs(1_u64 << attempt);
                eprintln!(
                    "{} transient error; retrying in {}s",
                    provider.name(),
                    delay.as_secs()
                );
                std::thread::sleep(delay);
                continue;
            }
            return Err(format!("{} API error: {}", provider.name(), value["error"]).into());
        }
        if attempt < 3 {
            let delay = Duration::from_secs(1_u64 << attempt);
            eprintln!(
                "{} transport error; retrying in {}s",
                provider.name(),
                delay.as_secs()
            );
            std::thread::sleep(delay);
            continue;
        }
        return Err(format!(
            "{} request failed: {}",
            provider.name(),
            String::from_utf8_lossy(&response.stderr)
        )
        .into());
    }
    unreachable!("retry loop always returns")
}

fn request_body(
    provider: Provider,
    model: &str,
    text: &str,
    image_data_url: Option<&str>,
    image_detail: &str,
) -> Value {
    match provider {
        Provider::Xai => {
            let mut content = Vec::new();
            if let Some(image_url) = image_data_url {
                content.push(json!({
                    "type": "input_image",
                    "image_url": image_url,
                    "detail": image_detail
                }));
            }
            content.push(json!({"type": "input_text", "text": text}));
            json!({
                "model": model,
                "store": false,
                "reasoning": {"effort": REASONING_EFFORT},
                "max_output_tokens": MAX_OUTPUT_TOKENS,
                "input": [
                    {"role": "system", "content": SYSTEM_PROMPT},
                    {"role": "user", "content": content}
                ]
            })
        }
        Provider::Openrouter => {
            let mut content = vec![json!({"type": "text", "text": text})];
            if let Some(image_url) = image_data_url {
                content.push(json!({
                    "type": "image_url",
                    "image_url": {"url": image_url, "detail": image_detail}
                }));
            }
            json!({
                "model": model,
                "messages": [
                    {"role": "system", "content": SYSTEM_PROMPT},
                    {"role": "user", "content": content}
                ],
                "reasoning": {"effort": REASONING_EFFORT},
                "temperature": OPENROUTER_TEMPERATURE,
                "top_p": OPENROUTER_TOP_P,
                "seed": OPENROUTER_SEED,
                "max_tokens": MAX_OUTPUT_TOKENS,
                "stream": false,
                "provider": {
                    "allow_fallbacks": false,
                    "require_parameters": true
                }
            })
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn arm_eval(
    provider: Provider,
    value: &Value,
    elapsed_ms: u128,
    requested_model: &str,
    concepts: &[String],
    anchors: &[String],
    raw_path: &Path,
    output_root: &Path,
) -> Result<ArmEval, Box<dyn std::error::Error>> {
    let response_model = required_string(value, "model", "provider response")?.to_owned();
    if response_model != requested_model {
        return Err(format!(
            "{} response model {response_model:?} does not match requested model {requested_model:?}",
            provider.name()
        )
        .into());
    }
    let answer = match provider {
        Provider::Xai => extract_xai_answer(value),
        Provider::Openrouter => extract_openrouter_answer(value)?,
    };
    if answer.trim().is_empty() {
        return Err(format!("{} response contains no answer text", provider.name()).into());
    }
    let usage = &value["usage"];
    let (
        input_tokens,
        cached_input_tokens,
        output_tokens,
        reasoning_tokens,
        total_tokens,
        cost_usd,
    ) = match provider {
        Provider::Xai => (
            required_number(usage, "input_tokens", "xAI usage")?,
            optional_number(&usage["input_tokens_details"], "cached_tokens", "xAI usage")?,
            required_number(usage, "output_tokens", "xAI usage")?,
            optional_number(
                &usage["output_tokens_details"],
                "reasoning_tokens",
                "xAI usage",
            )?,
            required_number(usage, "total_tokens", "xAI usage")?,
            optional_number(usage, "cost_in_usd_ticks", "xAI usage")?
                .map(|ticks| ticks as f64 / 10_000_000_000.0),
        ),
        Provider::Openrouter => (
            required_number(usage, "prompt_tokens", "OpenRouter usage")?,
            optional_number(
                &usage["prompt_tokens_details"],
                "cached_tokens",
                "OpenRouter usage",
            )?,
            required_number(usage, "completion_tokens", "OpenRouter usage")?,
            optional_number(
                &usage["completion_tokens_details"],
                "reasoning_tokens",
                "OpenRouter usage",
            )?,
            required_number(usage, "total_tokens", "OpenRouter usage")?,
            optional_float(usage, "cost", "OpenRouter usage")?,
        ),
    };
    Ok(ArmEval {
        elapsed_ms,
        response_model,
        upstream_provider: value["provider"].as_str().map(str::to_owned),
        input_tokens,
        cached_input_tokens,
        output_tokens,
        reasoning_tokens,
        total_tokens,
        cost_usd,
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
        || error["code"]
            .as_u64()
            .is_some_and(|code| matches!(code, 408 | 409 | 429) || code >= 500)
        || message.contains("rate limit")
        || message.contains("temporarily unavailable")
}

fn api_key(env_file: &Path, provider: Provider) -> Result<String, Box<dyn std::error::Error>> {
    let key_name = provider.key_name();
    if let Ok(key) = std::env::var(key_name)
        && !key.trim().is_empty()
    {
        return Ok(key);
    }
    if let Some(key) = api_key_from_text(&fs::read_to_string(env_file)?, key_name) {
        return Ok(key);
    }
    Err(format!(
        "{key_name} was not found in the environment or {}",
        env_file.display()
    )
    .into())
}

fn api_key_from_text(text: &str, key_name: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        if let Some(value) = line.strip_prefix(&format!("{key_name}=")) {
            let value = value.trim().trim_matches(['\'', '"']);
            if !value.is_empty() {
                return Some(value.to_owned());
            }
        }
    }
    None
}

fn prepare_output(
    output: &Path,
    binding: &EvaluationBinding,
    fresh_output: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let binding_path = output.join(BINDING_FILE);
    if output.exists() {
        if fresh_output {
            return Err(format!(
                "fresh output directory already exists: {}",
                output.display()
            )
            .into());
        }
        let existing: EvaluationBinding =
            serde_json::from_slice(&fs::read(&binding_path).map_err(|error| {
                format!(
                    "existing output has no readable {}: {error}",
                    binding_path.display()
                )
            })?)?;
        if &existing != binding {
            return Err(format!(
                "existing output binding does not match provider, model, settings, or source results: {}",
                binding_path.display()
            )
            .into());
        }
        return Ok(());
    }

    fs::create_dir_all(output)?;
    fs::write(binding_path, serde_json::to_vec_pretty(binding)?)?;
    Ok(())
}

fn resolve_artifact(value: &str, results_root: &Path) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        return path;
    }
    let relative = results_root.join(&path);
    if relative.exists() {
        return relative;
    }
    path
}

fn extract_xai_answer(response: &Value) -> String {
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

fn extract_openrouter_answer(response: &Value) -> Result<String, Box<dyn std::error::Error>> {
    let choices = response["choices"]
        .as_array()
        .ok_or("OpenRouter response is missing choices")?;
    if choices.len() != 1 {
        return Err(format!(
            "OpenRouter response must contain exactly one choice, found {}",
            choices.len()
        )
        .into());
    }
    choices[0]["message"]["content"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "OpenRouter response choice is missing message content".into())
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

fn required_string<'a>(
    value: &'a Value,
    key: &str,
    context: &str,
) -> Result<&'a str, Box<dyn std::error::Error>> {
    value[key]
        .as_str()
        .ok_or_else(|| format!("{context} is missing string {key}").into())
}

fn strings(value: &Value, key: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    Ok(array(value, key)?
        .iter()
        .map(|item| item.as_str().unwrap_or_default().to_owned())
        .collect())
}

fn required_number(
    value: &Value,
    key: &str,
    context: &str,
) -> Result<u64, Box<dyn std::error::Error>> {
    value[key]
        .as_u64()
        .ok_or_else(|| format!("{context} is missing unsigned integer {key}").into())
}

fn optional_number(
    value: &Value,
    key: &str,
    context: &str,
) -> Result<Option<u64>, Box<dyn std::error::Error>> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("{context} field {key} is not an unsigned integer").into()),
    }
}

fn optional_float(
    value: &Value,
    key: &str,
    context: &str,
) -> Result<Option<f64>, Box<dyn std::error::Error>> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_f64()
            .map(Some)
            .ok_or_else(|| format!("{context} field {key} is not a number").into()),
    }
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
            let reasoning_tokens = arm
                .reasoning_tokens
                .map(|value| value.to_string())
                .unwrap_or_default();
            let cost_usd = arm
                .cost_usd
                .map(|value| format!("{value:.6}"))
                .unwrap_or_default();
            println!(
                "{},{},{},{},{},{},{:.3},{:.3},{}",
                prompt.repository,
                prompt.id,
                name,
                arm.input_tokens,
                arm.output_tokens,
                reasoning_tokens,
                arm.concept_recall_proxy,
                arm.anchor_recall_proxy,
                cost_usd
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
        assert_eq!(extract_xai_answer(&response), "first\nsecond");
    }

    #[test]
    fn direct_xai_remains_the_default_provider() {
        let args = Args::try_parse_from([
            "spectra-grok-eval",
            "--results",
            "results.json",
            "--output",
            "output",
        ])
        .unwrap();
        assert_eq!(args.provider, Provider::Xai);
        assert_eq!(args.model, None);
        assert_eq!(args.provider.default_model(), "grok-4.5");
        assert_eq!(args.reference_codegraph_report, None);
    }

    #[test]
    fn sha256_matches_the_published_abc_vector() {
        assert_eq!(
            sha256(b"abc"),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn provider_input_binding_covers_control_metadata_and_png_bytes() {
        let root = std::env::temp_dir().join(format!(
            "spectra-grok-input-binding-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("codegraph.txt"), b"control-a").unwrap();
        fs::write(root.join("map.png"), b"png-a").unwrap();
        let source = json!({
            "repositories": [{
                "name": "fixture",
                "prompts": [{
                    "id": "prompt",
                    "question": "How does it work?",
                    "expected_concepts": ["graph"],
                    "expected_anchors": ["src/lib.rs:1"],
                    "codegraph": {"raw_output": "codegraph.txt"},
                    "spectra": {
                        "raw_output": "line-one\nline-two\nmetadata",
                        "png_path": "map.png"
                    }
                }]
            }]
        });

        let control_a = codegraph_control_bundle_sha256(&source, &root).unwrap();
        let input_a = provider_input_bundle_sha256(&source, &root).unwrap();
        fs::write(root.join("map.png"), b"png-b").unwrap();
        assert_eq!(
            codegraph_control_bundle_sha256(&source, &root).unwrap(),
            control_a
        );
        assert_ne!(
            provider_input_bundle_sha256(&source, &root).unwrap(),
            input_a
        );
        fs::write(root.join("codegraph.txt"), b"control-b").unwrap();
        assert_ne!(
            codegraph_control_bundle_sha256(&source, &root).unwrap(),
            control_a
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn xai_request_and_usage_contract_remain_bound() {
        let request = request_body(
            Provider::Xai,
            "grok-4.5",
            "Question and context",
            Some("data:image/png;base64,cG5n"),
            "low",
        );
        assert_eq!(request["model"], "grok-4.5");
        assert_eq!(request["store"], false);
        assert_eq!(request["max_output_tokens"], MAX_OUTPUT_TOKENS);
        assert_eq!(request["reasoning"]["effort"], REASONING_EFFORT);
        assert_eq!(request["input"][1]["content"][0]["type"], "input_image");
        assert_eq!(request["input"][1]["content"][1]["type"], "input_text");

        let response = json!({
            "model": "grok-4.5",
            "output": [{
                "type": "message",
                "content": [{"type": "output_text", "text": "src/main.rs"}]
            }],
            "usage": {
                "input_tokens": 88,
                "output_tokens": 12,
                "total_tokens": 100,
                "input_tokens_details": {"cached_tokens": 3},
                "output_tokens_details": {"reasoning_tokens": 5},
                "cost_in_usd_ticks": 1230000
            }
        });
        let result = arm_eval(
            Provider::Xai,
            &response,
            4,
            "grok-4.5",
            &[],
            &[],
            Path::new("response.json"),
            Path::new("."),
        )
        .unwrap();
        assert_eq!(result.input_tokens, 88);
        assert_eq!(result.cached_input_tokens, Some(3));
        assert_eq!(result.output_tokens, 12);
        assert_eq!(result.reasoning_tokens, Some(5));
        assert_eq!(result.total_tokens, 100);
        assert_eq!(result.cost_usd, Some(0.000123));
    }

    #[test]
    fn openrouter_request_binds_multimodal_settings_without_fallbacks() {
        let request = request_body(
            Provider::Openrouter,
            "x-ai/grok-4.5",
            "Question and context",
            Some("data:image/png;base64,cG5n"),
            "low",
        );
        assert_eq!(request["model"], "x-ai/grok-4.5");
        assert_eq!(request["max_tokens"], MAX_OUTPUT_TOKENS);
        assert_eq!(request["reasoning"]["effort"], REASONING_EFFORT);
        assert_eq!(request["temperature"], OPENROUTER_TEMPERATURE);
        assert_eq!(request["top_p"], OPENROUTER_TOP_P);
        assert_eq!(request["seed"], OPENROUTER_SEED);
        assert_eq!(request["stream"], false);
        assert_eq!(request["provider"]["allow_fallbacks"], false);
        assert_eq!(request["provider"]["require_parameters"], true);
        assert_eq!(request["messages"][1]["content"][0]["type"], "text");
        assert_eq!(
            request["messages"][1]["content"][1]["image_url"]["detail"],
            "low"
        );
    }

    #[test]
    fn openrouter_response_requires_bound_model_and_reported_usage() {
        let response = json!({
            "model": "x-ai/grok-4.5",
            "provider": "xAI",
            "choices": [{"message": {"role": "assistant", "content": "src/main.rs"}}],
            "usage": {
                "prompt_tokens": 123,
                "completion_tokens": 17,
                "total_tokens": 140,
                "prompt_tokens_details": {"cached_tokens": 4},
                "completion_tokens_details": {"reasoning_tokens": 6},
                "cost": 0.00123
            }
        });
        let result = arm_eval(
            Provider::Openrouter,
            &response,
            12,
            "x-ai/grok-4.5",
            &[],
            &[],
            Path::new("response.json"),
            Path::new("."),
        )
        .unwrap();
        assert_eq!(result.response_model, "x-ai/grok-4.5");
        assert_eq!(result.upstream_provider.as_deref(), Some("xAI"));
        assert_eq!(result.input_tokens, 123);
        assert_eq!(result.cached_input_tokens, Some(4));
        assert_eq!(result.output_tokens, 17);
        assert_eq!(result.reasoning_tokens, Some(6));
        assert_eq!(result.total_tokens, 140);
        assert_eq!(result.cost_usd, Some(0.00123));

        let missing_usage = json!({
            "model": "x-ai/grok-4.5",
            "choices": [{"message": {"content": "answer"}}],
            "usage": {"completion_tokens": 1, "total_tokens": 1}
        });
        let error = arm_eval(
            Provider::Openrouter,
            &missing_usage,
            0,
            "x-ai/grok-4.5",
            &[],
            &[],
            Path::new("response.json"),
            Path::new("."),
        )
        .unwrap_err();
        assert!(error.to_string().contains("prompt_tokens"));

        let error = arm_eval(
            Provider::Openrouter,
            &response,
            0,
            "x-ai/grok-4.5:exacto",
            &[],
            &[],
            Path::new("response.json"),
            Path::new("."),
        )
        .unwrap_err();
        assert!(error.to_string().contains("does not match requested model"));
    }

    #[test]
    fn output_binding_supports_exact_resume_and_fails_closed() {
        let output = std::env::temp_dir().join(format!(
            "spectra-grok-binding-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let binding = EvaluationBinding {
            source_results: "results.json".into(),
            source_results_sha256: format!("sha256:{}", "1".repeat(64)),
            provider_input_bundle_sha256: format!("sha256:{}", "2".repeat(64)),
            codegraph_control_bundle_sha256: format!("sha256:{}", "3".repeat(64)),
            reference_codegraph_report_sha256: None,
            provider: Provider::Openrouter,
            api: Provider::Openrouter.api().into(),
            endpoint: Provider::Openrouter.endpoint().into(),
            model: "x-ai/grok-4.5".into(),
            system_prompt: SYSTEM_PROMPT.into(),
            reasoning_effort: REASONING_EFFORT.into(),
            image_detail: "low".into(),
            max_output_tokens: MAX_OUTPUT_TOKENS,
            temperature: Some(OPENROUTER_TEMPERATURE),
            top_p: Some(OPENROUTER_TOP_P),
            seed: Some(OPENROUTER_SEED),
            provider_fallbacks: false,
            require_all_parameters: true,
        };

        prepare_output(&output, &binding, true).unwrap();
        prepare_output(&output, &binding, false).unwrap();
        assert!(prepare_output(&output, &binding, true).is_err());

        let mut mismatched = binding.clone();
        mismatched.image_detail = "high".into();
        assert!(prepare_output(&output, &mismatched, false).is_err());
        fs::remove_dir_all(output).unwrap();
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
        assert_eq!(
            api_key_from_text("export XAI_KEY='test-key'\n", "XAI_KEY").as_deref(),
            Some("test-key")
        );
    }

    #[test]
    fn api_key_parser_supports_openrouter_without_xai_fallback() {
        assert_eq!(
            api_key_from_text(
                "XAI_KEY='wrong-key'\nOPENROUTER_API_KEY='router-key'\n",
                "OPENROUTER_API_KEY"
            )
            .as_deref(),
            Some("router-key")
        );
    }

    #[test]
    fn evaluation_summary_applies_all_published_topology_thresholds() {
        fn arm(input_tokens: u64, concept: f64, anchor: f64) -> ArmEval {
            ArmEval {
                elapsed_ms: 1,
                response_model: "model".into(),
                upstream_provider: Some("provider".into()),
                input_tokens,
                cached_input_tokens: Some(0),
                output_tokens: 1,
                reasoning_tokens: Some(0),
                total_tokens: input_tokens + 1,
                cost_usd: Some(0.001),
                concept_recall_proxy: concept,
                anchor_recall_proxy: anchor,
                answer: "answer".into(),
                raw_response: "response.json".into(),
            }
        }
        let prompts = vec![PromptEval {
            repository: "repository".into(),
            id: "prompt".into(),
            question: "question".into(),
            codegraph: arm(1_000, 0.5, 0.5),
            spectra: arm(100, 0.7, 0.9),
        }];
        let summary = evaluation_summary(&prompts).unwrap();
        assert_eq!(summary.provider_input_reduction.actual, 0.9);
        assert!(summary.provider_input_reduction.passed);
        assert!(summary.composite_retention.passed);
        assert!(summary.spectra_anchor_recall.passed);
        assert!(summary.all_published_topology_thresholds_passed);

        let failing = vec![PromptEval {
            repository: "repository".into(),
            id: "prompt".into(),
            question: "question".into(),
            codegraph: arm(1_000, 0.5, 0.5),
            spectra: arm(100, 0.7, 0.8),
        }];
        assert!(
            !evaluation_summary(&failing)
                .unwrap()
                .all_published_topology_thresholds_passed
        );
    }
}
