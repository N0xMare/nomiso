#!/usr/bin/env bash
# Offline snapshot / staged restore of the local durable store.
#
# Copies Surreal Rocks files + CAS blobs. This is not a live hot backup of an
# open Rocks engine: on Linux the script refuses to run while the store's
# LOCK file is held by a live process (flock probe); elsewhere it warns and
# proceeds (stop writers yourself).
#
# Integrity + frontier (OPS-004/005):
#   * every archived file is sha256-listed in checksums.sha256 and verified
#     before restore copies anything — truncation/tamper fails closed;
#   * the manifest carries a `frontier` block (file/blob counts and bytes,
#     plus per-table row counts when NOMISO_STATS_CMD provides them);
#   * restore is staged: archives land in <dest>.staged.<id>, are fully
#     validated (checksums, CAS layout, no symlinks, no path escape), then
#     activated by atomic rename — a failed validate never touches live dirs;
#   * each activated dir gets a FRONTIER.json marker carrying the snapshot
#     id — `verify` compares markers across data/blob dirs so a mixed-
#     generation install (data from Tuesday + blobs from Wednesday) is
#     detectable, not silent.
#
#   scripts/nomiso-snapshot.sh snapshot [archive.tar.gz]
#   scripts/nomiso-snapshot.sh restore  <archive.tar.gz>
#   scripts/nomiso-snapshot.sh verify
#
# Restore refuses unless manifest.json kind == nomiso-snapshot-v2
# (nomiso-snapshot-v1 archives restore identically — the v1 manifest predates
# frontier/staging but the file layout is the same). If the manifest has
# schema_version and it differs from NOMISO_SCHEMA_VERSION (default 0.3.7),
# restore refuses unless NOMISO_SNAPSHOT_FORCE=1.
#
# Env:
#   NOMISO_DATA_DIR          default ./.nomiso-data
#   NOMISO_BLOB_DIR          default $VEGAPUNK_BLOB_ROOT or ./.nomiso-blobs
#   NOMISO_SCHEMA_VERSION    expected manifest schema_version (default 0.3.7)
#   NOMISO_STATS_CMD         command emitting {"tables": {name: n, ...}} JSON
#                            (default: ./target/debug/vegapunk store-stats
#                            when that binary exists). Row counts land in the
#                            manifest frontier and are rechecked post-restore.
#   NOMISO_SNAPSHOT_FORCE=1  overwrite restore destinations; also allow
#                            schema_version mismatch (kind is never waived)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

DATA_DIR="${NOMISO_DATA_DIR:-./.nomiso-data}"
BLOB_DIR="${VEGAPUNK_BLOB_ROOT:-./.nomiso-blobs}"
if [[ -n "${NOMISO_BLOB_DIR:-}" ]]; then
  BLOB_DIR="${NOMISO_BLOB_DIR}"
fi
SCHEMA_VERSION="${NOMISO_SCHEMA_VERSION:-0.3.7}"
FORCE="${NOMISO_SNAPSHOT_FORCE:-0}"
STATS_CMD="${NOMISO_STATS_CMD:-}"
if [[ -z "$STATS_CMD" && -x ./target/debug/vegapunk ]]; then
  STATS_CMD="./target/debug/vegapunk store-stats"
fi

usage() {
  echo "usage: $0 snapshot [archive.tar.gz]" >&2
  echo "       $0 restore  <archive.tar.gz>" >&2
  echo "       $0 verify" >&2
  exit 2
}

need_dir() {
  local d="$1"
  [[ -d "$d" ]]
}

# Refuse to run while a live process holds the RocksDB LOCK file. flock is
# Linux-only; where it is missing we warn and proceed rather than pretend
# quiescence was verified.
check_quiesced() {
  local lock="$DATA_DIR/LOCK"
  [[ -f "$lock" ]] || return 0
  [[ "${NOMISO_SNAPSHOT_NO_LOCKCHECK:-0}" == "1" ]] && return 0
  if ! command -v flock >/dev/null 2>&1; then
    echo "snapshot: flock unavailable; cannot prove writers stopped — proceeding" >&2
    return 0
  fi
  if ! flock -n "$lock" -c true 2>/dev/null; then
    echo "snapshot: $lock is held by a live process — stop writers first" >&2
    exit 1
  fi
}

# Optional live frontier: {"tables": {...}} JSON from NOMISO_STATS_CMD.
# Absence is honest — the manifest records what was actually declared.
run_stats_cmd() {
  [[ -n "$STATS_CMD" ]] || return 1
  local out
  out="$($STATS_CMD 2>/dev/null)" || return 1
  python3 -c 'import json,sys; d=json.loads(sys.argv[1]); assert isinstance(d.get("tables"), dict)' \
    "$out" 2>/dev/null || return 1
  printf '%s' "$out"
}

# Validate a staged blob-root tree: only regular files whose relative path
# is ab/cd/<64-hex> (plus the root-level FRONTIER.json marker this script
# writes itself); no symlinks, no '..' components, nothing else.
# $1 = staged blob-root dir.
validate_cas_tree() {
  local dir="$1"
  # No symlinks anywhere in the staged payload.
  if find "$dir" -type l | grep -q .; then
    echo "restore: staged tree contains symlinks — refusing" >&2
    return 1
  fi
  # Every file must be a CAS object (xx/yy/<64-hex>) or the marker we add.
  local bad
  bad="$(cd "$dir" && find . -regextype posix-extended -type f \
      ! -path './FRONTIER.json' \
      ! -regex '\./[0-9a-f]{2}/[0-9a-f]{2}/[0-9a-f]{64}' \
      -print -quit 2>/dev/null || true)"
  if [[ -n "$bad" ]]; then
    echo "restore: non-CAS file in blob payload: $bad" >&2
    return 1
  fi
  if (cd "$dir" && find . -type f | grep -qE '(^|/)\.\.(/|$)'); then
    echo "restore: staged tree contains '..' path components — refusing" >&2
    return 1
  fi
  # Content-address verification when b3sum is available — a blob whose
  # bytes don't match its name is corruption a checksum can't catch
  # (checksum proves the archive, the name-hash proves the object).
  if command -v b3sum >/dev/null 2>&1; then
    local f name actual
    while IFS= read -r -d '' f; do
      name="$(basename "$f")"
      [[ "$name" == "FRONTIER.json" ]] && continue
      actual="$(b3sum --no-names "$f" | cut -d' ' -f1)"
      if [[ "$actual" != "$name" ]]; then
        echo "restore: blob content blake3 $actual != object name $name" >&2
        return 1
      fi
    done < <(find "$dir" -type f -print0)
  fi
  return 0
}

# Atomically swap $staged into place at $dest. The previous $dest (if any)
# moves to $dest.replaced.<id> and is left for the operator — never deleted.
activate_staged() {
  local staged="$1" dest="$2" tag="$3"
  local aside="${dest}.replaced.${tag}"
  if [[ -e "$dest" ]]; then
    if [[ "$FORCE" != "1" ]]; then
      echo "restore: $dest exists (set NOMISO_SNAPSHOT_FORCE=1)" >&2
      return 1
    fi
    mv "$dest" "$aside"
  fi
  mv "$staged" "$dest"
  [[ -e "$aside" ]] && echo "restore: prior $dest moved to $aside" >&2
  return 0
}

cmd="${1:-}"
shift || true

case "$cmd" in
  snapshot)
    dest="${1:-nomiso-snapshot.tar.gz}"
    has_data=0
    has_blobs=0
    need_dir "$DATA_DIR" && has_data=1
    need_dir "$BLOB_DIR" && has_blobs=1
    if [[ "$has_data" -eq 0 && "$has_blobs" -eq 0 ]]; then
      echo "snapshot: nothing to copy (missing $DATA_DIR and $BLOB_DIR)" >&2
      exit 1
    fi
    check_quiesced
    work="$(mktemp -d "${TMPDIR:-/tmp}/nomiso-snap-XXXXXX")"
    cleanup() { rm -rf "$work"; }
    trap cleanup EXIT
    mkdir -p "$work/snapshot"
    # Capture the declared frontier BEFORE copying: row counts come from the
    # stats command while the store is quiesced; file/blob counts come from
    # the copied tree below.
    tables_json="$(run_stats_cmd || true)"
    if [[ "$has_data" -eq 1 ]]; then
      mkdir -p "$work/snapshot/data"
      cp -a "$DATA_DIR"/. "$work/snapshot/data/"
      # The restore-time marker is regenerated per install — never archived.
      rm -f "$work/snapshot/data/FRONTIER.json"
    fi
    if [[ "$has_blobs" -eq 1 ]]; then
      mkdir -p "$work/snapshot/blobs"
      cp -a "$BLOB_DIR"/. "$work/snapshot/blobs/"
      # The restore-time marker is regenerated per install — never archived.
      rm -f "$work/snapshot/blobs/FRONTIER.json"
    fi
    snap_id="$(python3 -c 'import uuid; print(uuid.uuid4().hex)')"
    data_files=0; data_bytes=0; blob_objects=0; blob_bytes=0
    if [[ "$has_data" -eq 1 ]]; then
      data_files="$(find "$work/snapshot/data" -type f | wc -l | tr -d ' ')"
      data_bytes="$(find "$work/snapshot/data" -type f -exec stat -c %s {} + 2>/dev/null | awk '{s+=$1} END{print s+0}')"
    fi
    if [[ "$has_blobs" -eq 1 ]]; then
      blob_objects="$(find "$work/snapshot/blobs" -type f | wc -l | tr -d ' ')"
      blob_bytes="$(find "$work/snapshot/blobs" -type f -exec stat -c %s {} + 2>/dev/null | awk '{s+=$1} END{print s+0}')"
    fi
    TABLES_JSON="$tables_json" python3 - \
      "$work/snapshot/manifest.json" "$SCHEMA_VERSION" "$has_data" "$has_blobs" \
      "$DATA_DIR" "$BLOB_DIR" "$snap_id" \
      "$data_files" "$data_bytes" "$blob_objects" "$blob_bytes" <<'PY'
import json, os, sys, datetime
(path, ver, has_data, has_blobs, data_dir, blob_dir, snap_id,
 data_files, data_bytes, blob_objects, blob_bytes) = sys.argv[1:]
frontier = {
    "snapshot_id": snap_id,
    "data_files": int(data_files),
    "data_bytes": int(data_bytes),
    "blob_objects": int(blob_objects),
    "blob_bytes": int(blob_bytes),
}
raw = os.environ.get("TABLES_JSON") or ""
tables = None
if raw:
    try:
        tables = json.loads(raw).get("tables")
    except Exception:
        tables = None
if isinstance(tables, dict):
    frontier["tables"] = tables
doc = {
    "kind": "nomiso-snapshot-v2",
    "created_at": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "schema_version": ver,
    "data_present": has_data == "1",
    "blobs_present": has_blobs == "1",
    "source_data_dir": data_dir,
    "source_blob_dir": blob_dir,
    "frontier": frontier,
    "note": "Offline copy. Stop writers before snapshot/restore.",
}
with open(path, "w", encoding="utf-8") as f:
    json.dump(doc, f, indent=2)
    f.write("\n")
PY
    # Integrity manifest: hash every archived file (incl. manifest.json) so
    # restore fails closed on truncation/tamper instead of installing it.
    (cd "$work/snapshot" && find . -type f ! -name checksums.sha256 -print0 \
      | sort -z | xargs -0 sha256sum > checksums.sha256)
    mkdir -p "$(dirname "$dest")"
    COPYFILE_DISABLE=1 tar -C "$work" -czf "$dest" snapshot
    echo "OK snapshot $dest  data=$has_data blobs=$has_blobs schema=$SCHEMA_VERSION id=$snap_id"
    ;;
  restore)
    src="${1:-}"
    [[ -n "$src" && -f "$src" ]] || usage
    check_quiesced
    work="$(mktemp -d "${TMPDIR:-/tmp}/nomiso-restore-XXXXXX")"
    cleanup() { rm -rf "$work"; }
    trap cleanup EXIT
    tar -C "$work" -xzf "$src"
    if [[ ! -f "$work/snapshot/manifest.json" ]]; then
      echo "restore: not a nomiso-snapshot archive" >&2
      exit 1
    fi
    python3 - "$work/snapshot/manifest.json" "$SCHEMA_VERSION" "$FORCE" <<'PY'
import json, sys
path, expect_ver, force = sys.argv[1:]
with open(path, encoding="utf-8") as f:
    doc = json.load(f)
kind = doc.get("kind")
if kind not in ("nomiso-snapshot-v1", "nomiso-snapshot-v2"):
    raise SystemExit(f"restore: manifest kind {kind!r} is not a nomiso snapshot")
ver = doc.get("schema_version")
if ver is not None and str(ver) != expect_ver and force != "1":
    raise SystemExit(
        f"restore: schema_version {ver!r} != {expect_ver!r} "
        "(set NOMISO_SNAPSHOT_FORCE=1 to allow)"
    )
PY
    if [[ -f "$work/snapshot/checksums.sha256" ]]; then
      (cd "$work/snapshot" && sha256sum -c checksums.sha256 >/dev/null) || {
        echo "restore: checksum verification failed — archive is corrupt or tampered" >&2
        exit 1
      }
      # sha256sum -c only verifies listed files; an archive carrying EXTRA
      # files not in the manifest must also fail closed.
      if ! (cd "$work/snapshot" \
              && find . -type f ! -name checksums.sha256 | sort > "$work/actual.txt" \
              && sed 's/^\*[0-9a-f]*  //; s/^[0-9a-f]*  //' checksums.sha256 | sort > "$work/listed.txt" \
              && cmp -s "$work/actual.txt" "$work/listed.txt"); then
        echo "restore: archive file set differs from manifest — refusing" >&2
        exit 1
      fi
    else
      echo "restore: no checksums.sha256 — refusing unverified archive" >&2
      exit 1
    fi
    # Reject tar members that would have escaped the extract dir (defense in
    # depth — GNU tar already strips, but verify before staging).
    if (cd "$work/snapshot" && find . -type l | grep -q .); then
      echo "restore: archive contains symlinks — refusing" >&2
      exit 1
    fi

    snap_id="$(python3 - "$work/snapshot/manifest.json" <<'PY'
import json, sys
doc = json.load(open(sys.argv[1]))
print(doc.get("frontier", {}).get("snapshot_id") or "pre-frontier")
PY
)"
    tag="$(python3 -c 'import uuid; print(uuid.uuid4().hex[:12])')"

    # --- Stage into dest-adjacent dirs (same filesystem → atomic rename) ---
    staged_data=""; staged_blobs=""
    if [[ -d "$work/snapshot/data" ]]; then
      staged_data="${DATA_DIR%/}.staged.${tag}"
      mkdir -p "$staged_data"
      cp -a "$work/snapshot/data"/. "$staged_data/"
      # Declared-frontier check: file count must match the manifest.
      want="$(python3 - "$work/snapshot/manifest.json" <<'PY'
import json, sys
print(json.load(open(sys.argv[1])).get("frontier", {}).get("data_files", -1))
PY
)"
      if [[ "$want" != "-1" ]]; then
        got="$(find "$staged_data" -type f | wc -l | tr -d ' ')"
        if [[ "$got" != "$want" ]]; then
          echo "restore: staged data file count $got != frontier $want" >&2
          rm -rf "$staged_data"
          exit 1
        fi
      fi
      python3 - "$staged_data/FRONTIER.json" "$snap_id" <<'PY'
import json, sys
json.dump({"snapshot_id": sys.argv[2], "component": "data"},
          open(sys.argv[1], "w"), indent=2)
PY
    fi
    if [[ -d "$work/snapshot/blobs" ]]; then
      staged_blobs="${BLOB_DIR%/}.staged.${tag}"
      mkdir -p "$staged_blobs"
      cp -a "$work/snapshot/blobs"/. "$staged_blobs/"
      if ! validate_cas_tree "$staged_blobs"; then
        rm -rf "$staged_blobs" ${staged_data:+"$staged_data"}
        exit 1
      fi
      want="$(python3 - "$work/snapshot/manifest.json" <<'PY'
import json, sys
print(json.load(open(sys.argv[1])).get("frontier", {}).get("blob_objects", -1))
PY
)"
      if [[ "$want" != "-1" ]]; then
        got="$(find "$staged_blobs" -type f | wc -l | tr -d ' ')"
        if [[ "$got" != "$want" ]]; then
          echo "restore: staged blob count $got != frontier $want" >&2
          rm -rf "$staged_blobs" ${staged_data:+"$staged_data"}
          exit 1
        fi
      fi
      python3 - "$staged_blobs/FRONTIER.json" "$snap_id" <<'PY'
import json, sys
json.dump({"snapshot_id": sys.argv[2], "component": "blobs"},
          open(sys.argv[1], "w"), indent=2)
PY
    fi

    # --- Preflight BOTH destinations before any activation: a refusal must
    # never leave one component swapped and the other stale (that IS the
    # mixed-generation failure mode this stage exists to prevent) ---
    if [[ "$FORCE" != "1" ]]; then
      if [[ -n "$staged_data" && -e "$DATA_DIR" && -n "$(ls -A "$DATA_DIR" 2>/dev/null)" ]]; then
        echo "restore: $DATA_DIR is not empty (set NOMISO_SNAPSHOT_FORCE=1)" >&2
        rm -rf "$staged_data" ${staged_blobs:+"$staged_blobs"}
        exit 1
      fi
      if [[ -n "$staged_blobs" && -e "$BLOB_DIR" && -n "$(ls -A "$BLOB_DIR" 2>/dev/null)" ]]; then
        echo "restore: $BLOB_DIR is not empty (set NOMISO_SNAPSHOT_FORCE=1)" >&2
        rm -rf ${staged_data:+"$staged_data"} "$staged_blobs"
        exit 1
      fi
    fi
    # --- Activate: rename staged over live; previous dirs move aside ---
    if [[ -n "$staged_data" ]]; then
      activate_staged "$staged_data" "$DATA_DIR" "$tag"
    fi
    if [[ -n "$staged_blobs" ]]; then
      activate_staged "$staged_blobs" "$BLOB_DIR" "$tag"
    fi

    # --- Post-activation: mixed-generation detection + optional row-count
    # recheck against the declared frontier ---
    if [[ -f "$DATA_DIR/FRONTIER.json" && -f "$BLOB_DIR/FRONTIER.json" ]]; then
      if ! python3 - "$DATA_DIR/FRONTIER.json" "$BLOB_DIR/FRONTIER.json" <<'PY'
import json, sys
a = json.load(open(sys.argv[1])).get("snapshot_id")
b = json.load(open(sys.argv[2])).get("snapshot_id")
raise SystemExit(0 if a == b else 1)
PY
      then
        echo "restore: WARNING data/blob snapshot ids differ — mixed generation" >&2
      fi
    elif [[ -d "$BLOB_DIR" && ! -f "$BLOB_DIR/FRONTIER.json" && -n "$staged_data" ]]; then
      echo "restore: WARNING $BLOB_DIR predates frontier markers — generation unknown" >&2
    fi

    # If the manifest declared table counts, verify the live store matches.
    if [[ -n "$STATS_CMD" ]]; then
      post="$(run_stats_cmd || true)"
      if [[ -n "$post" ]]; then
        if ! POST="$post" python3 - "$work/snapshot/manifest.json" <<'PY'
import json, os, sys
want = json.load(open(sys.argv[1])).get("frontier", {}).get("tables")
if want is None:
    sys.exit(0)
got = json.loads(os.environ["POST"]).get("tables", {})
bad = {t: (want[t], got.get(t)) for t in want if want[t] != got.get(t)}
if bad:
    print(f"restore: table counts differ from frontier: {bad}", file=sys.stderr)
    sys.exit(1)
PY
        then
          echo "restore: restored store does not match declared frontier" >&2
          exit 1
        fi
      fi
    fi
    echo "OK restore $src → data=$DATA_DIR blobs=$BLOB_DIR id=$snap_id"
    ;;
  verify)
    # Mixed-generation check on the live dirs: both FRONTIER markers must
    # agree when both are present.
    dm="$DATA_DIR/FRONTIER.json"; bm="$BLOB_DIR/FRONTIER.json"
    if [[ -f "$dm" && -f "$bm" ]]; then
      if python3 - "$dm" "$bm" <<'PY'
import json, sys
a = json.load(open(sys.argv[1])).get("snapshot_id")
b = json.load(open(sys.argv[2])).get("snapshot_id")
print(f"data={a} blobs={b}")
raise SystemExit(0 if a == b else 1)
PY
      then
        echo "OK verify: consistent generation"
      else
        echo "verify: MIXED GENERATION — data and blobs from different snapshots" >&2
        exit 1
      fi
    elif [[ -f "$dm" || -f "$bm" ]]; then
      # One marker only: fine when the other component is absent entirely
      # (single-component install); suspect when a populated sibling dir
      # exists without a marker — its generation is unknown.
      other="$BLOB_DIR"; [[ -f "$bm" ]] && other="$DATA_DIR"
      if [[ -d "$other" && -n "$(ls -A "$other" 2>/dev/null)" ]]; then
        echo "verify: $other is populated but carries no frontier marker — generation unknown" >&2
        exit 1
      fi
      echo "OK verify: single-component install (only one frontier marker)"
    else
      echo "verify: no frontier markers (pre-v2 snapshot or never restored)" >&2
      exit 1
    fi
    ;;
  *)
    usage
    ;;
esac
