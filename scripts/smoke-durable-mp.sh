#!/usr/bin/env bash
# Multi-process durable smoke: write in process A, read in process B on the same rocksdb path.
# Requires nomiso-store embedded-rocks feature (compiled into the small helper via cargo test binary).
# durable_mp_write / durable_mp_read are #[ignore]; this script passes --ignored.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
DIR="${TMPDIR:-/tmp}/nomiso-durable-mp-$$"
mkdir -p "$DIR"
export NOMISO_DURABLE_MP_PATH="$DIR/data"
export NOMISO_DURABLE_MP_PHASE="${NOMISO_DURABLE_MP_PHASE:-}"

cleanup() { rm -rf "$DIR"; }
trap cleanup EXIT

echo "durable-mp path=$NOMISO_DURABLE_MP_PATH"

# Phase write (process 1). Tests are #[ignore] unless invoked with --ignored.
NOMISO_DURABLE_MP_PHASE=write cargo test -p nomiso-store --features embedded-rocks \
  --lib durable_mp_write -- --nocapture --ignored

# Phase read (process 2) — must see the put from process 1
NOMISO_DURABLE_MP_PHASE=read cargo test -p nomiso-store --features embedded-rocks \
  --lib durable_mp_read -- --nocapture --ignored

echo "OK smoke-durable-mp"
