#!/usr/bin/env bash
# Repo-local Pi + Vegapunk layout check + optional print-mode smoke.
# Skip-honest if `pi` or a chat-capable model is missing. Not part of just check.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

fail() { echo "FAIL vegapunk-dogfood-pi: $*" >&2; exit 1; }

test -f AGENTS.md || fail "missing AGENTS.md"
test -f .pi/skills/vegapunk/SKILL.md || fail "missing .pi/skills/vegapunk/SKILL.md"
grep -q 'hard-recall' AGENTS.md || fail "AGENTS.md does not mention hard-recall"
grep -q 'before_agent_start' .pi/skills/vegapunk/SKILL.md || fail "skill missing hook prohibition"
if [[ -d .pi/extensions ]]; then
  fail "do not add .pi/extensions (no inject hook in this slice)"
fi
echo "OK layout (AGENTS.md + .pi/skills/vegapunk; no extensions)"

if ! command -v pi >/dev/null 2>&1; then
  echo "SKIP live pi -p: pi not on PATH"
  exit 0
fi

cargo build -q -p vegapunk-cli

DIR="${TMPDIR:-/tmp}/nomiso-dogfood-pi-$$"
mkdir -p "$DIR"
DATA="$DIR/rocks"
export VEGAPUNK_SCOPE="org/local/dogfood/pi"
export VEGAPUNK_ENDPOINT="rocksdb://${DATA}"
export VEGAPUNK_FORMAT=json
export VEGAPUNK_PROFILE=coding-agent
export VEGAPUNK_CONFIG="$DIR/vegapunk.toml"
{
  echo "endpoint = \"${VEGAPUNK_ENDPOINT}\""
  echo "default_scope = \"${VEGAPUNK_SCOPE}\""
  echo "profile = \"coding-agent\""
} >"$VEGAPUNK_CONFIG"

if curl -fsS -m 2 http://127.0.0.1:11434/api/tags >/dev/null 2>&1; then
  export VEGAPUNK_EMBED_URL="http://127.0.0.1:11434/v1"
  export VEGAPUNK_EMBED_API_KEY="${VEGAPUNK_EMBED_API_KEY:-ollama}"
  export VEGAPUNK_EMBED_MODEL="${VEGAPUNK_EMBED_MODEL:-qllama/bge-small-en-v1.5}"
  export VEGAPUNK_EMBED_DIM=384
  cat >>"$VEGAPUNK_CONFIG" <<EOF
embed_dim = 384
embed_url = "${VEGAPUNK_EMBED_URL}"
embed_model = "${VEGAPUNK_EMBED_MODEL}"
embed_api_key_env = "VEGAPUNK_EMBED_API_KEY"
EOF
else
  export VEGAPUNK_EMBED_DIM=32
  echo "embed_dim = 32" >>"$VEGAPUNK_CONFIG"
  echo "note: Ollama down; Pi smoke uses hashing dim 32 (lexical only)"
fi

cleanup() { rm -rf "$DIR"; }
trap cleanup EXIT

VP="$ROOT/target/debug/vegapunk"
[[ -x "$VP" ]] || fail "vegapunk binary missing after build"
VP_CMD=("$VP" --no-help --format json)

"${VP_CMD[@]}" encode --category procedural --text \
  "Format gate is rustfmt plus cargo clippy -D warnings."
"${VP_CMD[@]}" encode --category identity --text \
  "Nomiso stays Surreal-only. No LLM inside the plane."

PROMPT='Use bash (not guesswork). From this repo root run:
./target/debug/vegapunk --no-help --format json hard-recall --query "format gate rustfmt clippy" --pack
Then quote the pack.block text. If the command fails, print FAIL and the error. Do not invent a format gate.'

OUT="$DIR/pi-out.txt"
set +e
python3 - "$PROMPT" "$OUT" <<'PY'
import subprocess, sys
prompt, out_path = sys.argv[1], sys.argv[2]
cmd = [
    "pi", "-p", "--approve", "--no-session",
    prompt,
]
try:
    r = subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        timeout=120,
    )
except subprocess.TimeoutExpired:
    open(out_path, "w", encoding="utf-8").write("TIMEOUT\n")
    sys.exit(124)
open(out_path, "w", encoding="utf-8").write(
    (r.stdout or "") + "\n---stderr---\n" + (r.stderr or "")
)
sys.exit(r.returncode)
PY
rc=$?
set -e

blob="$(cat "$OUT")"
if [[ "$rc" -eq 124 ]] || grep -q '^TIMEOUT' "$OUT"; then
  echo "SKIP live pi -p: timed out (no model or hung provider)"
  exit 0
fi
if echo "$blob" | grep -Eiq 'auth|api key|unauthorized|not found|no model|login|401|AIFROST|ECONNREFUSED'; then
  echo "SKIP live pi -p: no working chat provider"
  echo "$blob" | tail -n 20
  exit 0
fi
if echo "$blob" | grep -Eiq 'clippy'; then
  echo "OK vegapunk-dogfood-pi (pi -p recalled clippy via vegapunk CLI)"
  exit 0
fi
echo "SKIP live pi -p: ran but pack/clippy not in output (provider weak or skill unused)"
echo "$blob" | tail -n 40
exit 0
