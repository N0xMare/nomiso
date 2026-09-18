# Nomiso / Vegapunk (this repo)

Durable memory is **Vegapunk on Nomiso**, not Pi session JSONL and not Grok `/memory`.

Load skill `vegapunk` (`.pi/skills/vegapunk/SKILL.md`) when you need project facts, locks, or to persist a decision.

- **Recall:** `./target/debug/vegapunk --no-help hard-recall --query "…" --pack`. Use only `pack.block`. Then `trace-inject` / `trace-outcome` on the pack `trace_id`.
- **Write:** extract `WriterOp[]` → `apply-ops`. Never invent `prior_id`. Secrets → `noop`. Conflict without a trusted prior → `category: uncertainty`.
- **Working-state:** get/put when the goal or file changes. That is restore-on, not prompt inject.
- **Do not** add a Pi `before_agent_start` recall hook, Mem0, or Hindsight-pi in this repo.

Binary: `./target/debug/vegapunk` (`cargo build -p vegapunk-cli`) or `vegapunk` on PATH. Config: `./vegapunk.toml` if present, else CLI/env. Context boundary: `docs/spec/07-controller-and-context.md#context-boundary`. Canonical architecture: `docs/spec/00-index.md` (target requirements, not a claim that all features are implemented).
