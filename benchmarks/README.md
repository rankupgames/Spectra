# Comparative encoding benchmark: Spectra and CodeGraph

CodeGraph is used here as a mature, source-rich local graph reference. This benchmark asks whether Spectra's visual-topology encoding can reduce model input while preserving enough architecture and navigation quality to justify further research. It is not intended as a general ranking of the two projects.

Spectra's internal prototype gate is at least 50% fewer median model input tokens than the `codegraph_explore` reference arm while retaining at least 90% of that arm's architecture/navigation task score.

## Corpus

Use pinned commits of three Rust repositories representing different scales:

1. ripgrep
2. tokio
3. rust-analyzer

Record every repository URL and exact commit SHA beside the result. Run `codegraph init` and `spectra init` from the same clean checkout.

## Procedure

For each prompt in `prompts.json`:

1. Start a fresh conversation using the same multimodal model, system prompt, temperature, and maximum output tokens.
2. CodeGraph arm: allow `codegraph_explore` and ordinary targeted reads.
3. Spectra arm: allow `spectra_map` and ordinary targeted reads.
4. Record provider-reported input tokens, output tokens, image settings, tool calls, wall time, returned payload bytes, and final answer.
5. Grade expected architecture concepts and expected source anchors blind to the arm.

Count all cached and uncached input tokens reported by the provider, including image-token charges. Do not approximate PNG bytes as text tokens. Report medians per repository and across the full corpus. A passing run must meet both the token and task-score gates; payload size alone is not a substitute.

## Deterministic runner

Build and run the tooling/payload arm with:

```sh
cargo build --release --workspace
./target/release/spectra-bench \
  --manifest benchmarks/prompts.json \
  --corpus-root /path/to/pinned/corpus \
  --output benchmarks/results/run-name \
  --codegraph-bin /path/to/codegraph \
  --spectra-bin /path/to/spectra \
  --reindex \
  --repeats 3
```

The runner verifies commit SHAs, records tool versions and cold indexing times, performs repeated warm queries, saves every raw CodeGraph response and Spectra image, estimates text-only tokens at four characters per token, and computes expected-anchor path recall. The estimate deliberately excludes vision tokens; only provider-reported usage can complete the final comparison.

Generated reports are written under `benchmarks/results/`. That directory is intentionally ignored by Git; publish selected results separately when they have been reviewed for release.

## State Machine Ledger benchmark

The deterministic Ledger benchmark compares replaying a verbose conversational/terminal transcript with replaying the bounded immutable projection. It covers successful edit/verification, failed-then-repaired verification, and a blocked session containing a credential fixture.

```sh
./target/release/spectra-ledger-bench \
  --output benchmarks/results/ledger-run \
  --repeats 20
```

It records projection token reduction, exact state-fact retention, durable append latency, replay latency, replay determinism, storage bytes, and secret redaction. The Grok arm consumes that frozen result:

```sh
./target/release/spectra-ledger-grok-eval \
  --benchmark benchmarks/results/ledger-run/ledger-benchmark.json \
  --output benchmarks/results/ledger-grok-run
```

Both model arms use identical state-recovery instructions. The evaluator records provider input/output/reasoning tokens, cost, answers, and arm-agnostic fact recall. Controlled transcripts are useful for regression but do not replace later evaluation on captured live-agent sessions.

The Codex hook adapter also has a recorded-wire backtest fixture at [`fixtures/codex-hook-session.jsonl`](fixtures/codex-hook-session.jsonl). The CLI acceptance test replays each lifecycle payload through a separate `spectra hook` process, including duplicate edit delivery, a failed verification, repair, successful verification, stop, and projection reinjection. It requires:

- final state `Complete`
- exact edited-path and verification fact retention
- fewer than 150 estimated projection tokens and fewer than 200 injected-context tokens
- no duplicate immutable event after retry

The post-hook deterministic regression must preserve the 93.4% median reduction and 100% minimum fact-recall baselines recorded during prototype development.

## Grok multimodal evaluation

`spectra-grok-eval` consumes an existing deterministic `results.json`, so model calls do not rebuild indexes or regenerate payloads. It uses xAI's Responses API with `grok-4.5`, low reasoning effort, low image detail, `store: false`, and identical system instructions for both arms. `XAI_KEY` is loaded from the process environment or an ignored `.env` file and is never written to an artifact.

Start with one prompt before spending on the full corpus:

```sh
./target/release/spectra-grok-eval \
  --results benchmarks/results/pilot-selector-v5-2026-07-12/results.json \
  --output benchmarks/results/grok-smoke-2026-07-12 \
  --limit 1
```

Use `--limit 0` for the complete nine-prompt run. The evaluator records provider-reported input, cached, output, reasoning, and total tokens; billed cost; raw answers; and simple concept/anchor lexical-recall proxies. The proxies are diagnostics, not a substitute for a blind human or independent model judge.

For a one-prompt-per-project gate, combine `--limit 0` with repeated filters:

```sh
--prompt-id rg-cli-args \
--prompt-id tokio-scheduler \
--prompt-id ra-lsp-dispatch
```
