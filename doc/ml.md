# Classical ML for Loopbiotic context optimization

## Status

This document describes an optional future direction. The current context
optimizer is fully deterministic and does not depend on an ML model, Python
runtime, or training pipeline.

## Goal

The most useful classical ML application in Loopbiotic is not code generation.
It is context and execution-policy ranking: selecting project fragments with
the highest expected usefulness at the lowest token and latency cost.

The model should remain an optional ranking function over candidates produced
by the deterministic index and dependency graph. Missing data, model failures,
or a missing model must always fall back to the heuristic ranker.

```text
prompt + cursor + selection + diagnostics
                    ↓
       deterministic candidate generator
                    ↓
          symbol and dependency graph
                    ↓
       heuristic or optional ML ranker
                    ↓
             token-budget packing
                    ↓
                  agent
```

A useful objective is expected accepted-step improvement per token:

```text
value(candidate) = P(useful | task, candidate) - λ * token_cost
```

## Candidate features

Features should be cheap, stable, and mostly language-independent:

- call/import graph distance,
- directory distance from the active file,
- cursor-symbol definition or reference,
- prompt overlap with symbols and paths,
- active diagnostic relationship,
- symbol and artifact kind,
- reference count,
- test-file indicator,
- historical file co-change,
- fragment token count,
- index freshness,
- historical success for similar candidates,
- session mode and expected card type,
- previous retry and rejection counts.

The first model should not consume raw source code or embeddings. It should
improve deterministic analysis rather than replace it.

## Interaction labels

Loopbiotic naturally produces weak supervision without manual annotation.

Positive signals include:

- a fragment appears in an accepted patch,
- the user opens a suggested location,
- the user selects `Follow`,
- a diagnostic disappears after applying a patch,
- a patch is accepted without edits,
- the goal completes without retry.

Negative signals include:

- patch rejection,
- `Other` or `Retry`,
- an unused supplied fragment,
- an agent search for a missing file,
- patch-contract repair,
- new diagnostics or failing checks after the change.

The strongest label is an accepted patch with zero user edit distance.

## Training telemetry

Candidate-level telemetry should contain versioned, privacy-aware features:

```json
{
  "session_id": "s_123",
  "repository_revision": "abc123",
  "candidate_id": "src/users/email.rs::UserEmail::parse",
  "features": {
    "graph_distance": 1,
    "is_definition": true,
    "is_test": false,
    "prompt_name_overlap": 0.67,
    "estimated_tokens": 184
  },
  "heuristic_score": 8.4,
  "selected": true,
  "later_used": true,
  "patch_accepted": true,
  "patch_edit_distance": 0
}
```

Step-level telemetry should include patch hashes, edit distance, time to user
decision, diagnostics and checks before/after, token usage, backend latency,
retry count, backend, model, and effort.

Source code and prompts may contain private data. Training datasets should stay
local by default, support content-free feature logging, and use explicit schema
and model versions.

## Model sequence

1. Logistic regression as an interpretable baseline.
2. Gradient boosting such as LightGBM or XGBoost.
3. LambdaMART once enough complete sessions exist.
4. A contextual bandit only for controlled backend, model, or budget choices.

Every model must be evaluated against the production heuristic, not only a
random baseline.

## Evaluation

Data splits must happen by session, repository, and time. Splitting individual
candidates would leak strongly correlated examples between train and test.

Primary metrics:

- accepted steps per input token,
- time to first accepted patch,
- retries per accepted step,
- context precision and recall,
- NDCG or MRR,
- tasks completed within a fixed budget.

Before activation, run the model in shadow mode: record its ranking while the
heuristic remains authoritative.

## Integration

Preferred architecture:

- offline Python training,
- JSON coefficients or ONNX export,
- inference inside the Rust process,
- no per-turn Python process,
- explicit feature/model schema versions,
- automatic heuristic fallback.

A long-lived local `loopbiotic-ranker` process over stdio is acceptable during early
experiments.

## Start conditions

ML implementation should begin only when:

- candidate generation and ranking contracts are stable,
- interaction outcome telemetry is available,
- enough independent sessions exist,
- offline replay and a heuristic baseline exist,
- shadow-mode results demonstrate a measurable cost or quality improvement.

Until then, Loopbiotic should collect versioned decisions and features while the
production system remains deterministic.
