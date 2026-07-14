#!/usr/bin/env bash
#
# Live token-consumption report across every supported backend/model.
#
# Drives each configured model through the buggy-TypeScript fixtures under
# tests/fixtures/token/ (easy=1 step, medium=3, hard=6) and prints one combined
# table of goal steps, agent turns, and tokens spent per (model x case).
#
# This is a MANUAL tool: it needs the real backend CLIs/servers and credentials
# installed (codex, claude, a running Ollama). Missing or misconfigured models
# are skipped with a warning; the report continues with the rest.
#
# Usage:
#   scripts/token-report.sh                    # run the model matrix below
#   scripts/token-report.sh --mock             # also include the mock backend
#   scripts/token-report.sh --check base.jsonl # compare against a saved baseline
#   scripts/token-report.sh --out my.jsonl     # keep the machine-readable JSONL
#   scripts/token-report.sh --max-turns 40
#
# Edit the MODELS array to match the models you actually have installed. Each
# entry is a ';'-separated list of env assignments; LOOPBIOTIC_REPORT_MODEL is the
# label shown in the table.

set -euo pipefail

MODELS=(
  "LOOPBIOTIC_REPORT_MODEL=codex/gpt-5.1;LOOPBIOTIC_BACKEND=codex;LOOPBIOTIC_CODEX_MODEL=gpt-5.1"
  "LOOPBIOTIC_REPORT_MODEL=claude/opus-4.8;LOOPBIOTIC_BACKEND=claude;LOOPBIOTIC_CLAUDE_MODEL=claude-opus-4-8"
  "LOOPBIOTIC_REPORT_MODEL=ollama/qwen2.5-coder;LOOPBIOTIC_BACKEND=ollama;LOOPBIOTIC_OLLAMA_MODEL=qwen2.5-coder"
)

# Override the matrix without editing this file: set LOOPBIOTIC_MODELS to a
# newline-separated list of the same ';'-separated env entries.
if [[ -n "${LOOPBIOTIC_MODELS:-}" ]]; then
  mapfile -t MODELS <<< "$LOOPBIOTIC_MODELS"
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

FIXTURES="tests/fixtures/token"
CHECK_BASELINE=""
KEEP_OUT=""
MAX_TURNS=""
INCLUDE_MOCK=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --check) CHECK_BASELINE="$2"; shift 2 ;;
    --out) KEEP_OUT="$2"; shift 2 ;;
    --fixtures) FIXTURES="$2"; shift 2 ;;
    --max-turns) MAX_TURNS="$2"; shift 2 ;;
    --mock) INCLUDE_MOCK=1; shift ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done

if [[ "$INCLUDE_MOCK" == "1" ]]; then
  MODELS+=("LOOPBIOTIC_REPORT_MODEL=mock;LOOPBIOTIC_BACKEND=mock")
fi

# Locate (or build) the daemon binary.
BIN="${LOOPBIOTIC_LOOPBIOTICD:-}"
if [[ -z "$BIN" ]]; then
  echo "building loopbioticd (release)..." >&2
  cargo build --release -p loopbioticd >&2
  BIN="$ROOT/target/release/loopbioticd"
fi

OUT="$(mktemp)"
: > "$OUT"

RUN_ARGS=(dev token-report --fixtures "$FIXTURES" --json "$OUT")
[[ -n "$MAX_TURNS" ]] && RUN_ARGS+=(--max-turns "$MAX_TURNS")

for entry in "${MODELS[@]}"; do
  label="${entry#LOOPBIOTIC_REPORT_MODEL=}"; label="${label%%;*}"
  echo "running $label ..." >&2
  # Each model runs in a subshell with only its env applied. Per-model stdout
  # (its own table) is discarded; the JSONL is what we aggregate. A failure to
  # construct the backend skips the model instead of aborting the report.
  if ! (
    IFS=';'
    for kv in $entry; do export "${kv?}"; done
    "$BIN" "${RUN_ARGS[@]}"
  ) >/dev/null; then
    echo "  ! skipped $label (backend unavailable or misconfigured)" >&2
  fi
done

echo
"$BIN" dev token-report --render "$OUT"

if [[ -n "$CHECK_BASELINE" ]]; then
  echo
  "$BIN" dev token-report --check "$CHECK_BASELINE" "$OUT"
fi

if [[ -n "$KEEP_OUT" ]]; then
  cp "$OUT" "$KEEP_OUT"
  echo "wrote $KEEP_OUT" >&2
fi
rm -f "$OUT"
