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

## Adapter parity gate

The v0.2 adapter gate compares framework route labels on five pinned, real repositories covering FastAPI/React, Laravel, NestJS, Spring MVC, and Vapor. [`adapter-repositories.json`](adapter-repositories.json) records the exact URLs, commits, and minimum Spectra route counts. Materialize each checkout beneath a corpus root using its manifest `name`, then run:

```sh
cargo run --release -p spectra-context --bin spectra-adapter-eval -- \
  --manifest benchmarks/adapter-repositories.json \
  --corpus-root /path/to/adapter-corpus \
  --output benchmarks/results/adapter-parity \
  --codegraph-bin /path/to/codegraph \
  --reindex
```

The runner verifies every commit, rebuilds both indexes, compares the complete CodeGraph route-label set with Spectra, requires the pinned minimum Spectra route count, measures resolved Spectra route edges, and exits nonzero on a gap. Additional Spectra routes are reported rather than treated as failures because the reference may detect no routes in a framework it otherwise recognizes. The reviewed v0.2 baseline is recorded in [`v0.2-baseline.md`](v0.2-baseline.md).

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

Recorded-wire backtest fixtures cover [Codex](fixtures/codex-hook-session.jsonl), [Claude Code](fixtures/claude-hook-session.jsonl), [Gemini CLI](fixtures/gemini-hook-session.jsonl), and [Cursor](fixtures/cursor-hook-session.jsonl). CLI acceptance tests replay each lifecycle payload through a separate `spectra hook --agent <agent>` process, including duplicate delivery, a failed verification, repair, successful verification, completion, and projection reinjection where the provider supports it. They require:

- final state `Complete`
- exact edited-path and verification fact retention
- fewer than 150 estimated projection tokens and fewer than 200 injected-context tokens
- no duplicate immutable event after retry
- provider-valid stdout and secret exclusion

Codex, Claude, and Gemini must retain equivalent session facts. Cursor is gated separately as `topology+ledger-partial` and may reinject only at session start.

The post-hook deterministic regression must preserve the 93.4% median reduction and 100% minimum fact-recall baselines recorded during prototype development.

## v0.4 adaptive-context release gate

The v0.4 release uses [`v0.4-holdout.json`](v0.4-holdout.json): 20 public repositories pinned to exact commits across Rust, Python, JavaScript, TypeScript, Ruby, PHP, Java, C#, Go, Swift, Dart, and C++. Its five task templates—navigation, change impact, flow, repair, and resume—are instantiated once per repository for 100 tasks. Holdout repositories must not influence selector rules. Each task runs baseline and Spectra arms with the same harness, model, task instructions, provider settings, and clean repository state.

Record provider input as separate schema, text, and image counts; record cached input as the provider-reported subset rather than adding it to the total. Also record output tokens, tool calls, latency, repeated context bytes, and final task success. The reviewed report format is defined by [`v0.4-report-schema.json`](v0.4-report-schema.json). A privacy review populates `forbidden_findings`; a release report must contain no source bodies, prompts, patches, terminal bodies, credentials, or raw session IDs in receipts or metrics.

Validate a reviewed report with:

```sh
cargo run --release -p spectra-context --bin spectra-v04-gate -- \
  benchmarks/results/reviewed-v0.4.json
```

The gate rejects fewer than 20 repositories, fewer than 100 unique tasks, fewer than two models or three harnesses, missing task categories, inconsistent provider-input accounting, or any privacy finding. It then requires at least 35% lower median provider input, at least 20% lower input at p75, solve rate within two percentage points of baseline, at least 70% fewer repeated context bytes, a median of at most two Spectra calls, and at least 95% budget-compliant text packets. Paid provider runs and task grading remain a reviewed release step; deterministic CI validates the gate itself and does not fabricate results.

## Agent efficiency scenarios

[`fixtures/efficiency-tool-scenarios.json`](fixtures/efficiency-tool-scenarios.json) freezes three common agent workflows: resuming after failed verification, discovering worktree impact and tests, and tracing an A-to-B flow. The release test compares the previous focused-query call count with the composite `brief`, `changes`, or `path` call and requires at least 40% fewer median calls. Separate query tests gate token budgets, changed-path and verification retention, deterministic paths, session isolation, and exclusion of raw diffs, source bodies, terminal output, and credentials.

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
