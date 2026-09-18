#!/usr/bin/env bash
# Bounded Vegapunk dogfood: encode standing locks + working-state + Layer-2 pack.
# Temp rocks store. Local Ollama BGE-384. Skip-honest if Ollama is down.
# Not part of `just check` / `just foundation`.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

OLLAMA="${VEGAPUNK_EMBED_URL:-http://127.0.0.1:11434/v1}"
MODEL="${VEGAPUNK_EMBED_MODEL:-qllama/bge-small-en-v1.5}"

if ! curl -fsS -m 2 "${OLLAMA%/v1}/api/tags" >/dev/null 2>&1 \
  && ! curl -fsS -m 2 "${OLLAMA}/models" >/dev/null 2>&1; then
  echo "SKIP vegapunk-dogfood: Ollama not reachable at ${OLLAMA}"
  exit 0
fi

DIR="${TMPDIR:-/tmp}/nomiso-dogfood-$$"
mkdir -p "$DIR"
DATA="$DIR/rocks"
SCOPE="org/local/dogfood/nomiso"
export VEGAPUNK_SCOPE="$SCOPE"
export VEGAPUNK_ENDPOINT="rocksdb://${DATA}"
export VEGAPUNK_EMBED_URL="$OLLAMA"
export VEGAPUNK_EMBED_API_KEY="${VEGAPUNK_EMBED_API_KEY:-ollama}"
export VEGAPUNK_EMBED_MODEL="$MODEL"
export VEGAPUNK_EMBED_DIM=384
export VEGAPUNK_FORMAT=json
export VEGAPUNK_PROFILE=coding-agent
{
  echo "endpoint = \"${VEGAPUNK_ENDPOINT}\""
  echo "default_scope = \"${VEGAPUNK_SCOPE}\""
  echo "profile = \"coding-agent\""
  echo "embed_dim = 384"
  echo "embed_url = \"${VEGAPUNK_EMBED_URL}\""
  echo "embed_model = \"${VEGAPUNK_EMBED_MODEL}\""
  echo "embed_api_key_env = \"VEGAPUNK_EMBED_API_KEY\""
} >"$DIR/vegapunk.toml"
export VEGAPUNK_CONFIG="$DIR/vegapunk.toml"

cleanup() { rm -rf "$DIR"; }
trap cleanup EXIT

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
    raise SystemExit(f"FAIL: pack cards/block missing {needle!r}\n{blob[:400]}")
PY
}

VP=(cargo run -q -p vegapunk-cli --features embedded-rocks,embedded-mem,embed-http -- --no-help --format json)

echo "dogfood endpoint=$VEGAPUNK_ENDPOINT embed=$VEGAPUNK_EMBED_URL model=$MODEL"

# Standing locks (encoded as ordinary memories; retrieved via Layer 2, not auto-injected).
"${VP[@]}" encode --category identity --text \
  "Nomiso stays Surreal-only. No LLM inside the plane."
"${VP[@]}" encode --category procedural --text \
  "Never auto-dump Nomiso memory into the prompt. Host injects pack.block only."
"${VP[@]}" encode --category procedural --text \
  "Format gate is rustfmt plus cargo clippy -D warnings."

# Coding working-state slot (restore-on; not semantic dump).
"${VP[@]}" working-state --put-json '{"goal":"dogfood standing locks","file":"docs/spec/07-controller-and-context.md"}'

WS="$DIR/ws.json"
"${VP[@]}" working-state >"$WS"
python3 - "$WS" <<'PY'
import json, sys
raw = open(sys.argv[1], encoding="utf-8").read()
data = json.loads(raw[raw.find("{"):])
body = data.get("body") or {}
assert data.get("status") == "ok", data
assert body.get("goal") == "dogfood standing locks", body
print("working-state ok")
PY

OUT="$DIR/recall.json"
"${VP[@]}" hard-recall --query "format gate rustfmt clippy" --pack >"$OUT"
assert_pack_hit "$OUT" "clippy"

OUT2="$DIR/recall2.json"
"${VP[@]}" hard-recall --query "is there an LLM inside the Nomiso plane?" --pack >"$OUT2"
assert_pack_hit "$OUT2" "Surreal"

echo "OK vegapunk-dogfood (standing locks encoded; Layer 2 pack hit; WM slot set; nothing auto-injected)"
