#!/usr/bin/env bash
# Regression test for scripts/nomiso-snapshot.sh: integrity manifest,
# schema guard, live-writer flock probe, staged restore, frontier markers,
# and mixed-generation detection.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SNAP="$ROOT/scripts/nomiso-snapshot.sh"
work="$(mktemp -d "${TMPDIR:-/tmp}/nomiso-snaptest-XXXXXX")"
cleanup() { rm -rf "$work"; }
trap cleanup EXIT

export NOMISO_DATA_DIR="$work/data"
export NOMISO_BLOB_DIR="$work/blobs"
export NOMISO_STATS_CMD=""   # no live stats binary in the self-test
mkdir -p "$NOMISO_DATA_DIR" "$NOMISO_BLOB_DIR"
echo "rocksdb-bytes" > "$NOMISO_DATA_DIR/CURRENT"
: > "$NOMISO_DATA_DIR/LOCK"
# A real CAS-shaped object: ab/cd/<64-hex>.
H="0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
mkdir -p "$NOMISO_BLOB_DIR/01/23"
echo "blob-bytes" > "$NOMISO_BLOB_DIR/01/23/$H"

fail() { echo "FAIL: $*" >&2; exit 1; }

# 1. Snapshot produces an integrity-verified archive with a v2 frontier.
archive="$work/snap.tar.gz"
"$SNAP" snapshot "$archive" >/dev/null || fail "snapshot exited nonzero"
tar -tzf "$archive" | grep -q "snapshot/checksums.sha256" \
  || fail "archive lacks checksums.sha256"
mkdir -p "$work/peek" && tar -C "$work/peek" -xzf "$archive" snapshot/manifest.json
python3 - "$work/peek/snapshot/manifest.json" <<'PY' || fail "manifest frontier missing/wrong"
import json, sys
m = json.load(open(sys.argv[1]))
assert m["kind"] == "nomiso-snapshot-v2", m["kind"]
f = m["frontier"]
assert f["snapshot_id"] and f["blob_objects"] == 1 and f["data_files"] >= 2, f
PY

# 2. Clean restore into empty destinations: staged → validated → activated.
rm -rf "$NOMISO_DATA_DIR" "$NOMISO_BLOB_DIR"
"$SNAP" restore "$archive" >/dev/null || fail "restore exited nonzero"
[[ "$(cat "$NOMISO_DATA_DIR/CURRENT")" == "rocksdb-bytes" ]] || fail "data mismatch"
[[ "$(cat "$NOMISO_BLOB_DIR/01/23/$H")" == "blob-bytes" ]] || fail "blob mismatch"
[[ -f "$NOMISO_DATA_DIR/FRONTIER.json" ]] || fail "data frontier marker missing"
[[ -f "$NOMISO_BLOB_DIR/FRONTIER.json" ]] || fail "blob frontier marker missing"
"$SNAP" verify >/dev/null || fail "verify after restore failed"
# No staged dirs or replaced dirs leak on success.
ls -d "$work"/*.staged.* >/dev/null 2>&1 && fail "staged dir leaked"

# 3. Tampered archive must fail closed on checksum verification.
rm -rf "$NOMISO_DATA_DIR" "$NOMISO_BLOB_DIR"
tampered="$work/tampered"
mkdir -p "$tampered"
tar -C "$tampered" -xzf "$archive"
echo "evil" > "$tampered/snapshot/data/INJECTED"
tar -C "$tampered" -czf "$work/bad.tar.gz" snapshot
"$SNAP" restore "$work/bad.tar.gz" >/dev/null 2>&1 \
  && fail "tampered archive restored"
[[ ! -e "$NOMISO_DATA_DIR/INJECTED" ]] || fail "tampered content installed"

# 4. Non-CAS payload in blobs/ is refused at staging.
rm -rf "$NOMISO_DATA_DIR" "$NOMISO_BLOB_DIR"
badcas="$work/badcas"
mkdir -p "$badcas"
tar -C "$badcas" -xzf "$archive"
echo "not-a-blob" > "$badcas/snapshot/blobs/stray.txt"
# Re-hash so the archive passes integrity and reaches the CAS shape check.
(cd "$badcas/snapshot" && find . -type f ! -name checksums.sha256 -print0 \
  | sort -z | xargs -0 sha256sum > checksums.sha256)
tar -C "$badcas" -czf "$work/badcas.tar.gz" snapshot
"$SNAP" restore "$work/badcas.tar.gz" >/dev/null 2>&1 \
  && fail "non-CAS blob payload restored"
[[ ! -e "$NOMISO_BLOB_DIR/stray.txt" ]] || fail "non-CAS file installed"
ls -d "$work"/*.staged.* >/dev/null 2>&1 && fail "staged dir leaked after refuse"

# 5. Symlinked member is refused (crafted archive).
rm -rf "$NOMISO_DATA_DIR" "$NOMISO_BLOB_DIR"
sl="$work/sl"
mkdir -p "$sl/snapshot"
ln -s /etc/passwd "$sl/snapshot/evil-link"
echo '{"kind":"nomiso-snapshot-v2","schema_version":"0.3.7"}' > "$sl/snapshot/manifest.json"
(cd "$sl/snapshot" && find . \( -type f -o -type l \) ! -name checksums.sha256 -print0 \
  | sort -z | xargs -0 sha256sum > checksums.sha256 2>/dev/null || true)
tar -C "$sl" -czf "$work/sl.tar.gz" snapshot
"$SNAP" restore "$work/sl.tar.gz" >/dev/null 2>&1 && fail "symlinked archive restored"
[[ ! -L "$work/data/evil-link" ]] || fail "symlink installed"

# 6. FORCE semantics: restore over non-empty dest refuses, then moves aside.
"$SNAP" restore "$archive" >/dev/null 2>&1 || fail "clean restore failed"
echo "stale" > "$NOMISO_DATA_DIR/stale-file"
"$SNAP" restore "$archive" >/dev/null 2>&1 \
  && fail "restore overwrote non-empty dest without FORCE"
NOMISO_SNAPSHOT_FORCE=1 "$SNAP" restore "$archive" >/dev/null \
  || fail "FORCE restore failed"
ls -d "${NOMISO_DATA_DIR%/}".replaced.* >/dev/null 2>&1 \
  || fail "replaced dir not preserved"
rm -rf "${NOMISO_DATA_DIR%/}".replaced.* "${NOMISO_BLOB_DIR%/}".replaced.*

# 7. Mixed-generation detection: divergent markers fail `verify`.
python3 - "$NOMISO_BLOB_DIR/FRONTIER.json" <<'PY'
import json, sys
p = sys.argv[1]
d = json.load(open(p)); d["snapshot_id"] = "different-snapshot"
json.dump(d, open(p, "w"))
PY
"$SNAP" verify >/dev/null 2>&1 && fail "mixed generation not detected"
"$SNAP" restore "$archive" >/dev/null 2>&1 \
  && fail "restore over mismatched-gen dirs should refuse without FORCE"

# 8. Live-writer probe: holding the LOCK must refuse the snapshot.
mkdir -p "$NOMISO_DATA_DIR"
: > "$NOMISO_DATA_DIR/LOCK"
if command -v flock >/dev/null 2>&1; then
  rc=0
  (
    flock -x 9 || exit 9
    "$SNAP" snapshot "$work/should-fail.tar.gz" >/dev/null 2>&1 && exit 7
    exit 0
  ) 9<"$NOMISO_DATA_DIR/LOCK" || rc=$?
  [[ $rc -eq 7 ]] && fail "snapshot ran while LOCK held"
  [[ $rc -eq 9 ]] && fail "test could not take LOCK"
fi

echo "nomiso-snapshot self-test: OK"
