#!/usr/bin/env bash
# Local Hindsight compare stack. Interface: just hindsight-*
# Default (plane) uses HINDSIGHT_API_LLM_PROVIDER=none — no API key.
set -euo pipefail

NAME="${HINDSIGHT_CONTAINER:-nomiso-hindsight}"
# NOTE: the `latest` pull of 2026-09-18 (image 3edcb6165cef) fails at boot —
# its local embedding model download dies inside the startup client. The
# last-known-good image here is 84ab276b8f50 (running as
# nomiso-hindsight-retain); override with HINDSIGHT_IMAGE=<ref> if `latest`
# is still broken upstream.
IMAGE="${HINDSIGHT_IMAGE:-ghcr.io/vectorize-io/hindsight:latest}"
VOLUME="${HINDSIGHT_VOLUME:-hindsight-data}"
URL="${HINDSIGHT_URL:-http://127.0.0.1:8888}"
UI="${HINDSIGHT_UI:-http://127.0.0.1:9999}"
WORKER_ID="${HINDSIGHT_API_WORKER_ID:-hindsight-nomiso-eval}"
LABEL_KEY="nomiso.hindsight.mode"

usage() {
  cat <<EOF
Usage: $0 <up plane|up llm|down|reset|status|wait|logs>
  up plane  — full image, LLM_PROVIDER=none (fair plane ingest; no API key)
  up llm    — same image, needs HINDSIGHT_API_LLM_API_KEY (system-track retain)
  down      — stop+remove container; keep volume
  reset     — down + delete named volume
  status    — docker + /health
  wait      — poll /health (default 180s)
  logs      — docker logs -f
EOF
}

mode_of() {
  docker inspect -f "{{index .Config.Labels \"${LABEL_KEY}\"}}" "$NAME" 2>/dev/null || true
}

exists() {
  docker inspect "$NAME" >/dev/null 2>&1
}

running() {
  [[ "$(docker inspect -f '{{.State.Running}}' "$NAME" 2>/dev/null || echo false)" == "true" ]]
}

cmd_wait() {
  local deadline="${HINDSIGHT_WAIT_SECS:-180}"
  local i=0
  echo "waiting for ${URL}/health (up to ${deadline}s)…"
  while (( i < deadline )); do
    if curl -fsS "${URL}/health" >/dev/null 2>&1; then
      echo "hindsight ready: ${URL}  ui: ${UI}"
      return 0
    fi
    sleep 2
    i=$((i + 2))
  done
  echo "hindsight did not become healthy at ${URL}/health" >&2
  docker logs --tail 80 "$NAME" >&2 || true
  return 1
}

cmd_status() {
  if ! exists; then
    echo "container ${NAME}: absent"
    return 1
  fi
  local mode
  mode="$(mode_of)"
  echo "container ${NAME}: $(running && echo running || echo stopped)  mode=${mode:-unknown}  image=$(docker inspect -f '{{.Config.Image}}' "$NAME")"
  if running && curl -fsS "${URL}/health" >/dev/null 2>&1; then
    echo "health: ok  ${URL}  ${UI}"
    return 0
  fi
  echo "health: not ready"
  return 1
}

run_new() {
  local mode="$1"
  local -a env_args=(
    -e "HINDSIGHT_API_WORKER_ID=${WORKER_ID}"
    --label "${LABEL_KEY}=${mode}"
  )
  if [[ "$mode" == "plane" ]]; then
    env_args+=(-e HINDSIGHT_API_LLM_PROVIDER=none)
  else
    local provider="${HINDSIGHT_API_LLM_PROVIDER:-openai}"
    local key="${HINDSIGHT_API_LLM_API_KEY:-${OPENAI_API_KEY:-}}"
    if [[ -z "$key" ]]; then
      echo "up llm requires HINDSIGHT_API_LLM_API_KEY (or OPENAI_API_KEY)" >&2
      exit 1
    fi
    env_args+=(
      -e "HINDSIGHT_API_LLM_PROVIDER=${provider}"
      -e "HINDSIGHT_API_LLM_API_KEY=${key}"
    )
    if [[ -n "${HINDSIGHT_API_LLM_MODEL:-}" ]]; then
      env_args+=(-e "HINDSIGHT_API_LLM_MODEL=${HINDSIGHT_API_LLM_MODEL}")
    fi
    if [[ -n "${HINDSIGHT_API_LLM_BASE_URL:-}" ]]; then
      env_args+=(-e "HINDSIGHT_API_LLM_BASE_URL=${HINDSIGHT_API_LLM_BASE_URL}")
    fi
  fi
  docker run -d --name "$NAME" --restart unless-stopped \
    --pull always \
    -p 8888:8888 -p 9999:9999 \
    "${env_args[@]}" \
    -v "${VOLUME}:/home/hindsight/.pg0" \
    "$IMAGE"
}

cmd_up() {
  local want="${1:-plane}"
  if [[ "$want" != "plane" && "$want" != "llm" ]]; then
    echo "up expects plane|llm" >&2
    exit 1
  fi
  if exists; then
    local have
    have="$(mode_of)"
    if [[ -n "$have" && "$have" != "$want" ]]; then
      echo "container ${NAME} is mode=${have}; wanted ${want}." >&2
      echo "run: just hindsight-down && just hindsight-up   # or just hindsight-up-llm" >&2
      echo "(volume is kept; only recreate the container)" >&2
      exit 1
    fi
    docker start "$NAME" >/dev/null
    echo "started existing ${NAME} (mode=${have:-unknown})"
  else
    run_new "$want"
    echo "created ${NAME} mode=${want}"
  fi
  cmd_wait
}

cmd_down() {
  if exists; then
    docker rm -f "$NAME" >/dev/null
    echo "removed ${NAME} (volume ${VOLUME} kept)"
  else
    echo "container ${NAME}: already absent"
  fi
}

cmd_reset() {
  cmd_down
  if docker volume inspect "$VOLUME" >/dev/null 2>&1; then
    docker volume rm "$VOLUME" >/dev/null
    echo "removed volume ${VOLUME}"
  fi
}

cmd="${1:-}"
shift || true
case "$cmd" in
  up) cmd_up "${1:-plane}" ;;
  down) cmd_down ;;
  reset) cmd_reset ;;
  status) cmd_status ;;
  wait) cmd_wait ;;
  logs) docker logs -f --tail 100 "$NAME" ;;
  -h|--help|help|"") usage ;;
  *) usage >&2; exit 1 ;;
esac
