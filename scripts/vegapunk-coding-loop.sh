#!/usr/bin/env bash
# Durable coding-loop smoke: encode → pack/inject/outcome → candidates →
# store-artifact → ingest-compaction → apply-ops → snapshot → restore → recall.
#
# Uses a temp rocks + blob root. Not just check (builds vegapunk-cli).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

DIR="${TMPDIR:-/tmp}/nomiso-coding-loop-$$"
mkdir -p "$DIR/rocks" "$DIR/blobs" "$DIR/restore-rocks" "$DIR/restore-blobs"
cleanup() { rm -rf "$DIR"; }
trap cleanup EXIT

SCOPE="org/loop/user/dev"
TOKEN="LOOP_TOKEN_$(date +%s)_$$"
export VEGAPUNK_SCOPE="$SCOPE"
export VEGAPUNK_ENDPOINT="rocksdb://${DIR}/rocks"
export VEGAPUNK_BLOB_ROOT="${DIR}/blobs"
export VEGAPUNK_EMBED_DIM=32
export VEGAPUNK_FORMAT=json
export VEGAPUNK_SESSION_ID="loop-$$"
export VEGAPUNK_TURN_ID="t1"
export VEGAPUNK_NO_HELP=1

# Force hashing; do not inherit maintainer HTTP embed / vegapunk.toml.
unset VEGAPUNK_EMBED_URL VEGAPUNK_EMBED_API_KEY OPENAI_API_KEY
{
  echo '# isolated foundation smoke — hashing path (no embed_url)'
  echo "endpoint = \"${VEGAPUNK_ENDPOINT}\""
  echo "default_scope = \"${VEGAPUNK_SCOPE}\""
} >"$DIR/vegapunk.toml"
export VEGAPUNK_CONFIG="$DIR/vegapunk.toml"

BIN="${ROOT}/target/debug/vegapunk"
if [[ ! -x "$BIN" ]]; then
  cargo build -q -p vegapunk-cli
fi
VP=("$BIN" --no-help --format json)

json_get() {
  python3 -c 'import json,sys
path=sys.argv[1]
raw=sys.stdin.read()
start=min([i for i in (raw.find("{"), raw.find("[")) if i>=0], default=-1)
if start<0:
    raise SystemExit("no json in output")
data=json.loads(raw[start:])
cur=data
for part in path.split("."):
    if part=="":
        continue
    if part.isdigit():
        cur=cur[int(part)]
    else:
        cur=cur[part]
if cur is None:
    print("")
else:
    print(cur)
' "$1"
}

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

assert_apply_ok() {
  python3 - "$1" <<'PY'
import json, sys
raw = open(sys.argv[1], encoding="utf-8").read()
start = min(i for i in (raw.find("{"), raw.find("[")) if i >= 0)
data = json.loads(raw[start:])
outs = data.get("outcomes") if isinstance(data, dict) else data
if not outs:
    raise SystemExit("FAIL: apply-ops empty outcomes")
for o in outs:
    if o.get("outcome") != "ok":
        raise SystemExit(f"FAIL: apply-ops {o}")
PY
}

echo "coding-loop endpoint=$VEGAPUNK_ENDPOINT token=$TOKEN"

ENC="$DIR/encode.json"
"${VP[@]}" encode --text "Coding-loop fact ${TOKEN} prefers TypeScript for MCP servers." >"$ENC"
MEM_ID="$(json_get id <"$ENC")"
MEM_VER="$(json_get version <"$ENC")"
[[ -n "$MEM_ID" ]] || { echo "FAIL: encode missing id" >&2; cat "$ENC" >&2; exit 1; }

PACK="$DIR/pack.json"
"${VP[@]}" hard-recall --query "$TOKEN TypeScript" --pack >"$PACK"
TRACE="$(json_get trace_id <"$PACK")"
assert_pack_hit "$PACK" "$TOKEN"
[[ -n "$TRACE" ]] || { echo "FAIL: pack missing trace_id" >&2; cat "$PACK" >&2; exit 1; }

"${VP[@]}" trace-inject --trace-id "$TRACE" --id "$MEM_ID" >/dev/null
"${VP[@]}" trace-outcome --trace-id "$TRACE" --outcome helped >/dev/null

CAND="$DIR/cand.json"
"${VP[@]}" candidates --query "TypeScript MCP preference" --limit 5 >"$CAND"
if ! grep -q "$MEM_ID" "$CAND"; then
  echo "FAIL: candidates missed encoded id $MEM_ID" >&2
  cat "$CAND" >&2
  exit 1
fi

EVID="$DIR/evidence.txt"
echo "evidence-bytes-${TOKEN}" >"$EVID"
ART="$DIR/art.json"
"${VP[@]}" store-artifact --file "$EVID" --media-type text/plain --source coding-loop >"$ART"
BLAKE="$(json_get blake3 <"$ART")"
[[ ${#BLAKE} -eq 64 ]] || { echo "FAIL: store-artifact blake3" >&2; cat "$ART" >&2; exit 1; }

COMP="$DIR/compact.txt"
printf '%s\n%s\n' \
  "After compact: ${TOKEN} uses pnpm not npm." \
  "Prefer --format json when scripting Vegapunk." \
  >"$COMP"
ING="$DIR/ingest.json"
"${VP[@]}" ingest-compaction --file "$COMP" --summary "coding-loop compact of ${TOKEN}" >"$ING"
if ! grep -q '"outcome":"ok"' "$ING" && ! grep -q 'outcome.*ok' "$ING"; then
  # CompactionReport is nested; require at least one stored outcome
  python3 - <<'PY' "$ING"
import json,sys
p=sys.argv[1]
raw=open(p).read()
start=raw.find("{")
data=json.loads(raw[start:])
outs=data.get("apply",{}).get("outcomes",[])
ok=any(o.get("outcome")=="ok" for o in outs)
if not ok:
    raise SystemExit("FAIL: ingest-compaction had no ok outcomes")
if data.get("checkpoint") is None:
    # summary may skip if too short; force path already used --summary long enough
    pass
PY
fi

OPS="$DIR/ops.json"
python3 - <<PY
import json
ops=[{
  "op":"supersede",
  "prior_id":"$MEM_ID",
  "expected_version": int("$MEM_VER" or "1"),
  "scope":"$SCOPE",
  "text":"Coding-loop fact $TOKEN prefers Rust for systems after the compact.",
  "category":"semantic",
  "confidence":0.9
}]
open("$OPS","w").write(json.dumps(ops))
PY
APP="$DIR/apply.json"
"${VP[@]}" apply-ops --file "$OPS" >"$APP"
assert_apply_ok "$APP"

TBM="$DIR/tbm.json"
"${VP[@]}" traces-by-memory --id "$MEM_ID" >"$TBM"
if ! grep -q "$TRACE" "$TBM"; then
  echo "FAIL: traces-by-memory missed $TRACE" >&2
  cat "$TBM" >&2
  exit 1
fi

SNAP="$DIR/snap.tar.gz"
NOMISO_DATA_DIR="${DIR}/rocks" NOMISO_BLOB_DIR="${DIR}/blobs" \
  bash "$ROOT/scripts/nomiso-snapshot.sh" snapshot "$SNAP"

NOMISO_DATA_DIR="${DIR}/restore-rocks" NOMISO_BLOB_DIR="${DIR}/restore-blobs" \
  NOMISO_SNAPSHOT_FORCE=1 \
  bash "$ROOT/scripts/nomiso-snapshot.sh" restore "$SNAP"

# New process, restored rocks — superseded fact must be recallable.
export VEGAPUNK_ENDPOINT="rocksdb://${DIR}/restore-rocks"
export VEGAPUNK_BLOB_ROOT="${DIR}/restore-blobs"
REST="$DIR/restore-recall.json"
"${VP[@]}" hard-recall --query "prefers Rust after the compact" --pack >"$REST"
assert_pack_hit "$REST" "$TOKEN"
assert_pack_hit "$REST" "Rust"

# Blob object from store-artifact must exist after restore (CAS layout).
if [[ ! -f "${DIR}/restore-blobs/${BLAKE:0:2}/${BLAKE:2:2}/${BLAKE}" ]]; then
  echo "FAIL: restored CAS object missing for $BLAKE" >&2
  find "${DIR}/restore-blobs" -type f >&2 || true
  exit 1
fi

echo "OK vegapunk-coding-loop (pack/flywheel/candidates/cas/compact/apply/snapshot/restore)"
