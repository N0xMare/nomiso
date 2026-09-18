# Nomiso / Vegapunk local development gate (no GitHub Actions required).
# Primary entrypoint: `just check`

set shell := ["bash", "-euo", "pipefail", "-c"]

# Unit gate: fmt + clippy (deny warnings) + lib tests. Durable bar is `just foundation`.
check: fixtures-check fmt-check clippy test-plane test-surfaces test-vegapunk test-tact snapshot-check
	@echo "just check: OK"

fixtures-check:
	python3 scripts/sync-eval-fixtures.py --check

# Release gate: archive + compile-verify every crate from packaged tarballs
# against packaged deps (offline tmp-registry; G15). Isolated CARGO_TARGET_DIR
# is required: registry deps are fingerprinted by name+version+source, not
# content, so a re-packaged same-version dep would otherwise reuse a stale
# rlib from target/debug and verify against phantom APIs. Slow (~15min cold).
package-check:
	rm -rf target/package target/package-check-target
	# Cargo treats registry sources as immutable: same-version repackaged deps
	# reuse the stale extraction under ~/.cargo/registry/src/<local-reg-hash>/.
	# Purge only our unpublished 0.2.0 extractions so verify sees fresh tarballs.
	rm -rf ~/.cargo/registry/src/*/nomiso-*-0.2.0 \
		~/.cargo/registry/src/*/{nomiso,nomisod,vegapunk,vegapunk-cli,tact-on-nomiso}-0.2.0
	CARGO_TARGET_DIR=target/package-check-target cargo package --workspace --allow-dirty --offline

# Snapshot integrity/quiescence regression (G13).
snapshot-check:
	./scripts/test-nomiso-snapshot.sh

# Thin Python SDK contract: spawns `vegapunk serve`, exercises the typed
# contract end-to-end (API-008). Needs python3 + a built binary.
sdk-check:
	cargo build -p vegapunk-cli
	python3 sdk/python/tests/test_contract.py ./target/debug/vegapunk

# Reference harness loop (API-009): task-state → events → proposal →
# host-selected insertion ack → attributed outcome, all over HTTP.
harness-check:
	cargo build -p vegapunk-cli
	python3 examples/reference-harness/harness.py ./target/debug/vegapunk

# PKG-003 feature closure: advertised minimal/optional feature rows must
# compile independently — an all-workspace build proves nothing about them.
feature-check:
	cargo check -p nomiso --no-default-features
	cargo check -p nomiso --no-default-features --features embedded-mem
	cargo check -p nomiso-store --no-default-features
	cargo check -p nomiso-store --no-default-features --features embedded-mem
	cargo check -p nomiso-embed --no-default-features
	cargo check -p nomiso-embed --features http
	cargo check -p nomiso-memory --no-default-features
	cargo check -p nomiso-eval --no-default-features
	cargo check -p nomiso-http -p nomiso-mcp --no-default-features
	cargo check -p vegapunk --no-default-features
	cargo check -p vegapunk --features cli-llm
	@echo "feature-check: OK"

# Release gate: packages, publish-order dry-run info, SDK + harness + features.
release-check: package-check feature-check sdk-check harness-check
	@echo "release-check: OK (publish order: scripts/publish-order.sh)"

# rustfmt (prototype: gate includes fmt --check)
fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

# Entire workspace (including examples/tact-on-nomiso). Do not list -p crates.
clippy:
	cargo clippy --workspace --all-targets -- -D warnings

# Plane crates
test-plane:
	# Hermetic: do not pick up a maintainer's live Hindsight/Mem0/Zep from the shell.
	env -u EVAL_HINDSIGHT_URL -u EVAL_MEM0_URL -u EVAL_ZEP_URL \
		cargo test -p nomiso-core -p nomiso-store -p nomiso-service -p nomiso-schema -p nomiso-embed -p nomiso-eval -p nomiso-blob -p nomiso -p nomiso-memory --lib --tests
	cargo test -p nomiso-embed --features http --lib --tests

# Surface crates (HTTP + MCP) previously omitted from the gate
test-surfaces:
	cargo test -p nomiso-http -p nomiso-mcp --lib --tests

# Product crate + CLI unit tests
test-vegapunk:
	cargo test -p vegapunk -p vegapunk-cli --lib --tests

# Tact-on-Nomiso production adapter (simple product on plane)
test-tact:
	cargo test -p tact-on-nomiso --lib --tests

# Release-ish build of primary artifacts
build:
	cargo build -p nomiso -p nomisod -p vegapunk -p vegapunk-cli

# End-to-end product smoke (single-process)
smoke:
	cargo build -p vegapunk-cli
	bash scripts/vegapunk-agent-smoke.sh

# Durable plane smoke (RocksDB embedded; single connection)
smoke-durable:
	cargo test -p nomiso-store --features embedded-rocks --lib durable_rocksdb_put_read_roundtrip -- --nocapture

# Multi-process durable proof: process A put → process B search (rocksdb path)
smoke-durable-mp:
	bash scripts/smoke-durable-mp.sh

# Multi-session *agent* path: Vegapunk CLI encode (proc A) → hard-recall (proc B)
smoke-agent-durable:
	bash scripts/smoke-agent-durable.sh

# Foundation gate: unit check + durable/agent smokes + coding-loop snapshot
foundation:
	just check
	just smoke
	just smoke-durable
	just smoke-durable-mp
	just smoke-agent-durable
	just smoke-coding-loop
	@echo "just foundation: OK — durable local release bar"

# Full coding loop on temp rocks: pack/flywheel/candidates/CAS/compact/snapshot/restore
smoke-coding-loop:
	cargo build -p vegapunk-cli
	bash scripts/vegapunk-coding-loop.sh

# Bounded dogfood: standing locks + working-state + hard-recall pack on Ollama BGE-384.
# Skip-honest if Ollama is down. Not part of `just check` / `just foundation`.
dogfood-coding:
	cargo build -p vegapunk-cli
	bash scripts/vegapunk-dogfood.sh

# Repo-local Pi harness: layout always; `pi -p` skip-honest without a chat model.
# Not part of `just check` / `just foundation`. Does not write ~/.pi.
dogfood-pi:
	bash scripts/vegapunk-dogfood-pi.sh

# Offline copy of local rocks + CAS blobs (stop writers first)
snapshot dest="nomiso-snapshot.tar.gz":
	bash scripts/nomiso-snapshot.sh snapshot {{dest}}

restore src:
	bash scripts/nomiso-snapshot.sh restore {{src}}

# Convenience: all workspace unit tests (heavier)
test-all:
	cargo test --workspace --lib --tests

# Lean packaging sanity (plane without default features when declared)
tree-lean:
	cargo tree -p nomiso --no-default-features 2>/dev/null || cargo tree -p nomiso

# Labeled HTTP embed probe + optional HTTP scoreboard (skips unless VEGAPUNK_EMBED_URL + key)
eval-embed-http:
	cargo test -p nomiso-eval --features http labeled_http_paraphrase -- --nocapture
	cargo test -p nomiso-eval --features http channel_scoreboard_http -- --nocapture

# Tagged plane scoreboard (Nomiso always; Hindsight/Mem0/Zep skip unless EVAL_*_URL)
eval-compare:
	cargo test -p nomiso-eval channel_scoreboard_nomiso_plane -- --nocapture

# Coding-agent pack (sota_coding_v2): validate + Nomiso plane + cost columns
eval-coding-plane:
	cargo test -p nomiso-eval coding_pack_validates_and_compiles -- --nocapture
	cargo test -p nomiso-eval coding_agent_plane_v2 -- --nocapture

# Same pack over HTTP embeds (skip-honest unless VEGAPUNK_EMBED_URL + key)
eval-coding-plane-http:
	cargo test -p nomiso-eval --features http coding_agent_plane_v2_http -- --nocapture

# Skill track on the same pack: gold apply (gate) + RuleWriter report + Hindsight retain (skip-honest)
# Unset EVAL_HINDSIGHT_URL so a leftover plane compare does not fail retain HTTP.
# Set EVAL_HINDSIGHT_RETAIN=llm (and hindsight-up-llm) to score retain.
eval-coding-skill:
	cargo test -p vegapunk coding_skill -- --nocapture
	@if [ "${EVAL_HINDSIGHT_RETAIN:-}" = "llm" ]; then \
		cargo test -p nomiso-eval coding_skill_hindsight_retain -- --nocapture; \
	else \
		env -u EVAL_HINDSIGHT_URL cargo test -p nomiso-eval coding_skill_hindsight_retain -- --nocapture; \
	fi

# Live Grok CLI extract on the coding pack (report-only). Needs grok on PATH.
# Preds written to evals/coding_agent/live/ (gitignored). Re-run is ~minutes.
eval-coding-skill-live:
	mkdir -p evals/coding_agent/live
	VEGAPUNK_SKILL_LIVE_DIR=evals/coding_agent/live \
		cargo run -p vegapunk-cli --features cli-llm -- --no-help eval --coding --live

# Re-score existing `{id}.ops.json` dumps (no host call). Skip-honest if dir missing.
eval-coding-skill-live-dir:
	cargo run -p vegapunk-cli -- --no-help eval --coding --live-dir evals/coding_agent/live

# System track: skip-honest unless VEGAPUNK_SYSTEM_LIVE=1 (live run).
eval-coding-system:
	env -u VEGAPUNK_SYSTEM_LIVE cargo test -p vegapunk coding_system -- --nocapture
	env -u VEGAPUNK_SYSTEM_LIVE cargo run -p vegapunk-cli -- --no-help --format json eval --coding --system

# System track live Grok CLI host (report-only). Isolated rocks; gold encode; host must hard-recall --pack.
# Dumps: VEGAPUNK_SYSTEM_DUMP=evals/coding_agent/system (gitignored). Optional EVAL_SYSTEM_LIMIT=N.
# Hashing unless VEGAPUNK_EMBED_URL is set (see eval-coding-system-live-http).
eval-coding-system-live:
	#!/usr/bin/env bash
	set -euo pipefail
	mkdir -p evals/coding_agent/system
	# Do not inherit a leftover Ollama URL into the hashing dump tree.
	env -u VEGAPUNK_EMBED_URL -u VEGAPUNK_EMBED_MODEL -u VEGAPUNK_EMBED_DIM \
		VEGAPUNK_SYSTEM_DUMP=evals/coding_agent/system \
		cargo run -p vegapunk-cli --features cli-llm -- --no-help --format json eval --coding --system --system-live

# Same system host with local Ollama BGE-384 (labeled http). Skip-honest if Ollama is down.
# Dumps: evals/coding_agent/system-http/ (gitignored). Does not overwrite the hashing dumps.
# Host binary is VEGAPUNK_LLM_BIN from the parent env, else grok (Rust default).
# Shebang: just 1.37 + set shell -c runs each recipe line as its own bash; exit 0 must be this script.
eval-coding-system-live-http:
	#!/usr/bin/env bash
	set -euo pipefail
	if ! curl -fsS -m 2 http://127.0.0.1:11434/api/tags >/dev/null 2>&1; then
		echo "SKIP eval-coding-system-live-http: Ollama not reachable at :11434"
		exit 0
	fi
	mkdir -p evals/coding_agent/system-http
	VEGAPUNK_SYSTEM_DUMP=evals/coding_agent/system-http \
	VEGAPUNK_EMBED_URL=http://127.0.0.1:11434/v1 \
	VEGAPUNK_EMBED_API_KEY=ollama \
	VEGAPUNK_EMBED_MODEL=qllama/bge-small-en-v1.5 \
		cargo run -p vegapunk-cli --features cli-llm -- --no-help --format json eval --coding --system --system-live

# Re-score system dumps (no host). Skip-honest if the dump dir is missing.
# Preview-era dumps (no pack_log) still score the 500-char host preview.
eval-coding-system-live-dir dir="evals/coding_agent/system":
	#!/usr/bin/env bash
	set -euo pipefail
	DIR="{{dir}}"
	if [ ! -d "$DIR" ]; then
		echo "SKIP eval-coding-system-live-dir: $DIR missing"
		exit 0
	fi
	cargo run -p vegapunk-cli -- --no-help --format json eval --coding --system-dir "$DIR"

# --- Hindsight local compare stack (optional; never part of just check) ---
# Plane default: LLM_PROVIDER=none, no API key. Full image = local BGE + MiniLM.
# Switch plane ↔ llm: just hindsight-down && just hindsight-up[-llm]
# Do not bind Mem0 OSS on :8888 at the same time.

hindsight-up:
	bash scripts/hindsight.sh up plane

hindsight-up-llm:
	bash scripts/hindsight.sh up llm

hindsight-down:
	bash scripts/hindsight.sh down

hindsight-reset:
	bash scripts/hindsight.sh reset

hindsight-status:
	bash scripts/hindsight.sh status

hindsight-wait:
	bash scripts/hindsight.sh wait

hindsight-logs:
	bash scripts/hindsight.sh logs

# Wait for local Hindsight, then run the tagged scoreboard against it
eval-compare-hindsight:
	bash scripts/hindsight.sh wait
	EVAL_HINDSIGHT_URL=http://127.0.0.1:8888 EVAL_HINDSIGHT_BANK=nomiso-eval just eval-compare

# Same as eval-compare-hindsight, plus HTTP embeds when VEGAPUNK_EMBED_URL + key exist
eval-compare-hindsight-http:
	bash scripts/hindsight.sh wait
	EVAL_HINDSIGHT_URL=http://127.0.0.1:8888 EVAL_HINDSIGHT_BANK=nomiso-eval \
		cargo test -p nomiso-eval --features http channel_scoreboard_http -- --nocapture

# --- Citation-grounded head-to-head (G22; evals/h2h-preregistration.md) ---
# nomiso-t4 vs local Hindsight; grok answering agent declares USED: [ids].
# Skip-honest parts: grok on PATH + EVAL_H2H_LIVE=1; hindsight skips when down.

# Hashing row (offline embedder) + hindsight plane.
eval-h2h:
	bash scripts/hindsight.sh wait
	EVAL_H2H_LIVE=1 EVAL_H2H_MODEL=grok-4.6 EVAL_H2H_EFFORT=low \
	EVAL_HINDSIGHT_URL=http://127.0.0.1:8888 EVAL_HINDSIGHT_BANK=nomiso-eval \
	EVAL_H2H_OUT=evals/h2h-hash.json \
		cargo test -p nomiso-eval --features http h2h::tests::h2h_live -- --nocapture

# HTTP-embed row (Ollama BGE-384 our side) + hindsight plane.
eval-h2h-http:
	bash scripts/hindsight.sh wait
	if ! curl -fsS -m 2 http://127.0.0.1:11434/api/tags >/dev/null 2>&1; then \
		echo "SKIP eval-h2h-http: Ollama not reachable at :11434"; exit 0; fi
	EVAL_H2H_LIVE=1 EVAL_H2H_MODEL=grok-4.6 EVAL_H2H_EFFORT=low \
	EVAL_HINDSIGHT_URL=http://127.0.0.1:8888 EVAL_HINDSIGHT_BANK=nomiso-eval \
	VEGAPUNK_EMBED_URL=http://127.0.0.1:11434/v1 \
	VEGAPUNK_EMBED_API_KEY=ollama \
	VEGAPUNK_EMBED_MODEL=qllama/bge-small-en-v1.5 \
	VEGAPUNK_EMBED_DIM=384 \
	EVAL_H2H_OUT=evals/h2h-http.json \
		cargo test -p nomiso-eval --features http h2h::tests::h2h_live -- --nocapture

# LLM row: hindsight retain uses its container-default (LLM) extraction —
# no chunks pin. Label lands as `hindsight-default` in the report.
eval-h2h-llm:
	bash scripts/hindsight.sh wait
	if ! curl -fsS -m 2 http://127.0.0.1:11434/api/tags >/dev/null 2>&1; then \
		echo "SKIP eval-h2h-llm: Ollama not reachable at :11434"; exit 0; fi
	EVAL_H2H_LIVE=1 EVAL_H2H_MODEL=grok-4.6 EVAL_H2H_EFFORT=low \
	EVAL_HINDSIGHT_URL=http://127.0.0.1:8888 EVAL_HINDSIGHT_BANK=nomiso-eval \
	EVAL_HINDSIGHT_EXTRACT=none \
	VEGAPUNK_EMBED_URL=http://127.0.0.1:11434/v1 \
	VEGAPUNK_EMBED_API_KEY=ollama \
	VEGAPUNK_EMBED_MODEL=qllama/bge-small-en-v1.5 \
	VEGAPUNK_EMBED_DIM=384 \
	EVAL_H2H_OUT=evals/h2h-llm.json \
		cargo test -p nomiso-eval --features http h2h::tests::h2h_live -- --nocapture

# Semantic foil: skip-honest if no embed URL+key; otherwise HTTP vs Hindsight
eval-semantic:
	bash scripts/eval-semantic.sh

# Local BGE-small (384-d) via Ollama — same family as Hindsight default embedder.
# Requires: `ollama pull qllama/bge-small-en-v1.5` and `ollama serve` on :11434.
eval-semantic-ollama:
	VEGAPUNK_EMBED_URL=http://127.0.0.1:11434/v1 \
	VEGAPUNK_EMBED_API_KEY=ollama \
	VEGAPUNK_EMBED_MODEL=qllama/bge-small-en-v1.5 \
	VEGAPUNK_EMBED_DIM=384 \
		just eval-semantic
