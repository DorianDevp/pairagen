#!/usr/bin/env bash
#
# Real-model A/B report for Project Intelligence and selected Skills.
#
# before: no project profile and no selected Skills
# profile: detected project profile, no selected Skills
# after: detected project profile plus the fixture's selected Skills
#
# LM Studio must already expose its OpenAI-compatible API. Model runs are
# sequential so a local machine never serves more than one generation at once.

set -euo pipefail

MODELS=(
  "LOOPBIOTIC_REPORT_MODEL=gemma-4-12b;LOOPBIOTIC_BACKEND=lm_studio;LOOPBIOTIC_OPENAI_MODEL=google/gemma-4-12b;LOOPBIOTIC_OPENAI_MAX_TOKENS=768;LOOPBIOTIC_TURN_TIMEOUT_SECS=180"
  "LOOPBIOTIC_REPORT_MODEL=gpt-5.4-low;LOOPBIOTIC_BACKEND=codex;LOOPBIOTIC_CODEX_MODEL=gpt-5.4;LOOPBIOTIC_CODEX_DISCOVERY_MODEL=gpt-5.4;LOOPBIOTIC_CODEX_EFFORT=low;LOOPBIOTIC_CODEX_DISCOVERY_EFFORT=low;LOOPBIOTIC_TURN_TIMEOUT_SECS=180"
)

if [[ -n "${LOOPBIOTIC_MODELS:-}" ]]; then
  mapfile -t MODELS <<< "$LOOPBIOTIC_MODELS"
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

FIXTURES="tests/fixtures/project-intelligence"
VARIANTS="before,profile,after"
CASES=""
REPEAT=2
KEEP_OUT=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --out) KEEP_OUT="$2"; shift 2 ;;
    --fixtures) FIXTURES="$2"; shift 2 ;;
    --variants) VARIANTS="$2"; shift 2 ;;
    --cases) CASES="$2"; shift 2 ;;
    --repeat) REPEAT="$2"; shift 2 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done

BIN="${LOOPBIOTIC_LOOPBIOTICD:-}"
if [[ -z "$BIN" ]]; then
  echo "building loopbioticd (release)..." >&2
  cargo build --release -p loopbioticd >&2
  BIN="$ROOT/target/release/loopbioticd"
fi

OUT="$(mktemp)"
trap 'rm -f "$OUT"' EXIT
RUN_ARGS=(dev ab-report --fixtures "$FIXTURES" --variants "$VARIANTS" --repeat "$REPEAT" --json "$OUT")
[[ -n "$CASES" ]] && RUN_ARGS+=(--cases "$CASES")

for entry in "${MODELS[@]}"; do
  label="${entry#LOOPBIOTIC_REPORT_MODEL=}"; label="${label%%;*}"
  echo "running $label ..." >&2
  if ! (
    IFS=';'
    for kv in $entry; do export "${kv?}"; done
    "$BIN" "${RUN_ARGS[@]}"
  ); then
    echo "  ! skipped $label (backend unavailable or misconfigured)" >&2
  fi
done

if [[ -n "$KEEP_OUT" ]]; then
  cp "$OUT" "$KEEP_OUT"
  echo "wrote $KEEP_OUT" >&2
fi
