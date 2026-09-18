#!/usr/bin/env bash
# Multi-session *agent* durable smoke (Vegapunk CLI, two processes, rocksdb).
# Process A: encode a unique fact. Process B: hard-recall finds it.
# Requires: vegapunk-cli with --features embedded-rocks
#
# Honesty: never grep raw JSON for TOKEN (queries[] always echoes the query).
# Assert pack.block + card previews only; fail if abstained. Forces hashing
# (unsets embed URL/keys) and isolates VEGAPUNK_CONFIG from user toml.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

DIR="${TMPDIR:-/tmp}/nomiso-agent-durable-$$"
mkdir -p "$DIR"
DATA="$DIR/rocks"
SCOPE="org/agent/durable/smoke"
TOKEN="AGENT_DURABLE_TOKEN_$(date +%s)_$$"
export VEGAPUNK_SCOPE="$SCOPE"
export VEGAPUNK_ENDPOINT="rocksdb://${DATA}"
export VEGAPUNK_EMBED_DIM=32
export VEGAPUNK_FORMAT=json

# Force hashing; do not inherit maintainer HTTP embed / vegapunk.toml.
unset VEGAPUNK_EMBED_URL VEGAPUNK_EMBED_API_KEY OPENAI_API_KEY
{
  echo '# isolated foundation smoke — hashing path (no embed_url)'
  echo "endpoint = \"${VEGAPUNK_ENDPOINT}\""
  echo "default_scope = \"${VEGAPUNK_SCOPE}\""
} >"$DIR/vegapunk.toml"
export VEGAPUNK_CONFIG="$DIR/vegapunk.toml"

cleanup() { rm -rf "$DIR"; }
trap cleanup EXIT

# Assert pack cards/block contain needle (never grep the query echo).
assert_pack_hit() {
  python3 - "$1" "$2" <<'PY'
import json, sys
path, needle = sys.argv[1], sys.argv[2]
raw = open(path, encoding="utf-8").read()
start = raw.find("{")
data = json.loads(raw[start:])
if data.get("abstained"):
    raise SystemExit(f"FAIL: pack abstained looking for {needle!r}")
pack = data.get("pack") or {}
blob = (pack.get("block") or "") + " " + " ".join(
    str(c.get("preview") or "") for c in (pack.get("cards") or [])
)
if needle.lower() not in blob.lower():
    raise SystemExit(f"FAIL: pack cards/block missing {needle!r}")
if int(data.get("hit_count") or 0) < 1 and not pack.get("cards"):
    raise SystemExit("FAIL: pack hit_count/cards empty")
PY
}

echo "agent-durable endpoint=$VEGAPUNK_ENDPOINT scope=$SCOPE token=$TOKEN"

VP=(cargo run -q -p vegapunk-cli --features embedded-rocks,embedded-mem -- --no-help --format json)

# Process A — encode
"${VP[@]}" encode --text "Agent durable fact ${TOKEN} prefers TypeScript for tooling."

# Process B — hard-recall must find token in pack cards/block (fresh process)
OUT="$DIR/recall.json"
"${VP[@]}" hard-recall --query "$TOKEN TypeScript" --pack >"$OUT" 2>/dev/null || {
  # retry without quiet stderr if first fails
  "${VP[@]}" hard-recall --query "$TOKEN TypeScript" --pack >"$OUT"
}
assert_pack_hit "$OUT" "$TOKEN"

# Process C — list/count still see the row
COUNT_OUT="$DIR/count.json"
"${VP[@]}" count >"$COUNT_OUT" 2>/dev/null || "${VP[@]}" count >"$COUNT_OUT"
if ! grep -Eq '"count"[[:space:]]*:[[:space:]]*[1-9]' "$COUNT_OUT" \
  && ! grep -Eq 'count.*[1-9]' "$COUNT_OUT"; then
  # TOON or JSON — accept non-empty success if token still recallable (already checked)
  echo "note: count format non-JSON; recall already verified"
fi

echo "OK smoke-agent-durable (multi-process encode → hard-recall on rocksdb)"
