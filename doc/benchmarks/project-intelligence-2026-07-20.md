# Project Intelligence real-model A/B benchmark

Date: 2026-07-20

## Question

Does the marker-driven ProjectProfile and selected Markdown Skills make a weak
local model capable of work it could not reliably complete before, while making
a frontier model faster and more accurate?

This is a controlled counterfactual on the current engine, not a comparison of
two historical binaries. The runner toggles only the tested context features:

- `before`: no ProjectProfile and no selected Skills;
- `profile`: ProjectProfile enabled, selected Skills absent;
- `after`: ProjectProfile and the case's selected Skills enabled.

Removing configured Skill files from the temporary workspace in the first two
variants prevents tool-capable agents from discovering `AGENTS.md` on their own.
Every other input and the backend implementation remain the same.

## Setup

- Local: `google/gemma-4-12b` through LM Studio's OpenAI-compatible API,
  temperature 0, seed 42, 8,192-token context, 768-token output limit, one
  generation at a time.
- Frontier: GPT-5.4 through the Codex app-server, low reasoning effort.
- Host: Intel i5-8600K (6 cores), 31 GiB RAM, no discrete NVIDIA device exposed.
- Cases: exact polyglot stack mapping, an Angular 22 signal-input patch, and an
  Angular-to-React Nx boundary review.
- Sample: three repetitions of each case and variant: 9 responses per
  model/variant, 27 per model, 54 total.

`Pass` requires the mandated card kind and every deterministic rubric item.
`Content` measures only the rubric facts, including useful intent preserved in
an error card after malformed structured output. `Accepted` is the share that
returned the required card kind. Tokens are provider-reported and comparable
between variants of one model, not across providers.

## Aggregate results

| Model | Variant | Pass | Content | Accepted | Avg tokens | Avg time | Median time | Avg attempts |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| Gemma 4 12B | before | 0.0% | 35.6% | 55.6% | 2,494 | 23.16 s | 13.19 s | 1.44 |
| Gemma 4 12B | profile | 22.2% | 75.6% | 55.6% | 2,885 | 26.65 s | 17.27 s | 1.44 |
| Gemma 4 12B | after | 22.2% | 80.0% | 44.4% | 3,521 | 23.75 s | 20.28 s | 1.56 |
| GPT-5.4 low | before | 22.2% | 77.8% | 100.0% | 9,646 | 16.71 s | 17.81 s | 1.00 |
| GPT-5.4 low | profile | 77.8% | 95.6% | 100.0% | 9,836 | 16.12 s | 16.46 s | 1.00 |
| GPT-5.4 low | after | 88.9% | 97.8% | 100.0% | 9,995 | 16.52 s | 12.23 s | 1.00 |

## Before-to-after delta

| Model | Pass | Content | Accepted | Avg tokens | Avg time | Median time |
|---|---:|---:|---:|---:|---:|---:|
| Gemma 4 12B | +22.2 pp | +44.4 pp | -11.1 pp | +41.2% | +2.6% | +53.7% |
| GPT-5.4 low | +66.7 pp | +20.0 pp | 0.0 pp | +3.6% | -1.2% | -31.3% |

The frontier result matches the intended product effect: substantially more
complete answers with almost unchanged average cost and a much better median
latency. One 48.7-second `after` stack-map outlier hides that median gain in the
mean.

For the local model, the architecture makes the answer materially smarter but
not yet reliably executable. ProjectProfile alone accounts for most of the
content gain: +40.0 percentage points over `before`. Skills add another 4.4
points in aggregate, at 22.0% more tokens than `profile`, and their benefit is
task-dependent rather than uniformly positive.

## Failure analysis

- Gemma's exact stack-map content rose from 0.0% to 83.3%; the model could use
  detected Angular 22.0.6, TypeScript 6.0.3, React 18.3.1, Rust edition 2024,
  and Nx ownership facts that were impossible to recover from the active buffer.
- Gemma's Angular patch content rose from 50.0% to 83.3%, but all three `after`
  patch attempts were unusable. The main failure was malformed unified diff or
  truncated structured JSON, not missing Angular knowledge.
- Only the selected-Skill GPT-5.4 variant produced valid Angular patches in all
  three runs (profile produced one; `before` produced none). Its stack and
  boundary tasks were already strong, while ProjectProfile removed most
  remaining misses without a discovery turn.
- Five of nine local `after` responses ended as Error cards. The weak model's
  remaining bottleneck is therefore output mechanics and validation recovery,
  not primarily project discovery.

## Decision

The ProjectProfile investment paid off for both model classes and should remain
the foundation. The current Skill injection is useful, especially for exact
framework conventions, but should be made more selective: activate a small
adapter-matched section instead of spending context on whole files whenever
possible.

The next high-leverage local-model change is a structured edit intermediate
representation owned by Rust. A weak model should select a file, range, and
replacement (or a typed transformation); Loopbiotic should generate and
validate the unified diff. Follow that with adapter-provided compile/type-check
commands and a bounded repair loop. Teaching the model more facts will not fix
the malformed-diff failure demonstrated here.

## Reproduction

Start LM Studio's local server and load the configured model, then run:

```sh
scripts/project-intelligence-report.sh --repeat 3 --out results.jsonl
```

The model matrix can be replaced with newline-separated entries in
`LOOPBIOTIC_MODELS`. Individual cases and variants can be selected with
`--cases` and `--variants`.

This is a deliberately small POC benchmark with deterministic lexical rubrics,
not a general coding-model leaderboard. Re-run it after changes to patch IR,
knowledge packs, context selection, or repair policy and compare distributions,
not a single sample.
