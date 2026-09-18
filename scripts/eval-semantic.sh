#!/usr/bin/env bash
# Labeled HTTP-embed plane compare vs Hindsight.
# Skip-honest (exit 0) when VEGAPUNK_EMBED_URL + key are unset.
# Fail if URL is set but the key is missing.
# If URL+key are set: ping :8888/health quickly. Down → skip-Hindsight and
# run Nomiso HTTP scoreboard only (no hindsight.sh wait). Up → existing
# eval-compare-hindsight-http recipe.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

url="${VEGAPUNK_EMBED_URL:-}"
key="${VEGAPUNK_EMBED_API_KEY:-${OPENAI_API_KEY:-}}"

if [[ -z "${url// }" ]]; then
  echo "eval-semantic: SKIP — VEGAPUNK_EMBED_URL unset"
  echo "hashing-fair table remains evals/compare/last-miss.md"
  echo "set VEGAPUNK_EMBED_URL + VEGAPUNK_EMBED_API_KEY (or OPENAI_API_KEY), then:"
  echo "  just hindsight-up && just eval-semantic"
  exit 0
fi
if [[ -z "${key// }" ]]; then
  echo "eval-semantic: FAIL — VEGAPUNK_EMBED_URL set but no VEGAPUNK_EMBED_API_KEY / OPENAI_API_KEY" >&2
  exit 1
fi

if curl -fsS --max-time 2 http://127.0.0.1:8888/health >/dev/null 2>&1; then
  echo "eval-semantic: Hindsight :8888 healthy — HTTP scoreboard + Hindsight"
  just eval-compare-hindsight-http
else
  echo "eval-semantic: skip-Hindsight — :8888/health down; Nomiso HTTP scoreboard only"
  env -u EVAL_HINDSIGHT_URL \
    cargo test -p nomiso-eval --features http channel_scoreboard_http -- --nocapture
fi
