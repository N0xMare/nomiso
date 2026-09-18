#!/usr/bin/env bash
# Single-process Vegapunk product smoke (encode → hard-recall → apply-ops).
# Forces in-process memory so `just smoke` stays CI-safe and does not write .nomiso-data.
# Multi-session durable proof: `just smoke-agent-durable` (rocks).
# Isolates VEGAPUNK_CONFIG and unsets embed URL/keys so hashing is forced.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
DIR="${TMPDIR:-/tmp}/nomiso-agent-smoke-$$"
mkdir -p "$DIR"
cleanup() { rm -rf "$DIR"; }
trap cleanup EXIT
export VEGAPUNK_SCOPE="${VEGAPUNK_SCOPE:-org/smoke/user/agent}"
export VEGAPUNK_ENDPOINT="${VEGAPUNK_ENDPOINT:-memory}"

# Force hashing; do not inherit maintainer HTTP embed / vegapunk.toml.
unset VEGAPUNK_EMBED_URL VEGAPUNK_EMBED_API_KEY OPENAI_API_KEY
{
  echo '# isolated foundation smoke — hashing path (no embed_url)'
  echo "endpoint = \"${VEGAPUNK_ENDPOINT}\""
  echo "default_scope = \"${VEGAPUNK_SCOPE}\""
} >"$DIR/vegapunk.toml"
export VEGAPUNK_CONFIG="$DIR/vegapunk.toml"

cargo run -q -p vegapunk-cli -- --no-help smoke
echo "OK smoke complete (scope=${VEGAPUNK_SCOPE})"
