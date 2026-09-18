---
name: vegapunk
description: Full dual-plane Vegapunk memory product on Nomiso — when to write, hard-recall+pack, checkpoint, supersede. Use for a generalist coding agent that owns both encode and recall.
---

# Vegapunk (monolith skill)

You use **Vegapunk** for durable agent memory. Storage is Nomiso/Surreal. You never write SurrealQL.

## Tools

Prefer the **AXI CLI** `vegapunk` (shell). Alternates: Rust lib, thin product MCP. `vegapunk serve` HTTP is a **subset** (no candidates / store-artifact / ingest-compaction / sleep apply).

```bash
vegapunk status
vegapunk encode --text "…"
vegapunk hard-recall --query "…" --pack
vegapunk apply-ops --file ops.json
vegapunk checkpoint --summary "…"
vegapunk candidates --query "…"
vegapunk supersede --prior-id memory:… --expected-version N --text "…"
vegapunk ingest-compaction --file discarded.txt --summary "…"
vegapunk store-artifact --file ./log.txt
vegapunk trace-inject --trace-id … --id memory:…
vegapunk trace-outcome --trace-id … --outcome helped
vegapunk traces-by-memory --id memory:…
```

Stdout defaults to **TOON** (token-efficient). Nested/long payloads may be compact JSON. Use `--format full` for pretty JSON.

## When to write

- Durable **preferences, decisions, constraints, error lessons** → encode or apply-ops  
- Ephemeral chat, secrets, raw transcripts → **do not** store. `ingest-compaction` is for **already-extracted durable lines**, not a chat dump.
- Prefer **host extract → `apply-ops`** (JSON `WriterOp[]`) over inventing free-form text dumps  
- Two facts conflict and no trusted `prior_id` → `put` `category: "uncertainty"` (or `noop`); never invent a winner. Query with `list --category uncertainty`.  

## When to recall

- Before answering with user/project-specific facts  
- Call `hard-recall --pack`  
- If pack non-empty: inject **only** `pack.block` and cite memory ids  
- Then `vegapunk trace-inject --trace-id <id> --id <memory-id>…` (host-reported; never auto-injected)  
- After the turn: `vegapunk trace-outcome --trace-id <id> --outcome helped|harmed|skipped`  
- If abstained: rewrite once (error codes, entities); then abstain — do not invent  

## Scope

Resolution: `--scope` > `VEGAPUNK_SCOPE` > `vegapunk.toml` `default_scope`.  
Never leak across projects.

## Inject (three layers)

**Layer 1 — standing header (off, not implemented).** Not `working-state` restore. Not `soft_inject`.

**Layer 2 — turn retrieval (shipped).** Before project-specific facts: `hard-recall --pack`. Inject **only** `pack.block` (not the full hit list / scope). Then `trace-inject` / `trace-outcome`.

**Layer 3 — reserved soft-inject (off).** Capped semantic dump. Never dump a scope. Deep tools (`candidates`, `as_of`, uncertainty list, traces) are never injected.

See `docs/spec/07-controller-and-context.md#context-boundary`.

## Roles

For dual-model setups use dedicated skills:

- `vegapunk-writer` — encode only  
- `vegapunk-reader` — hard-recall / pack only  

## Config

`vegapunk init` writes `vegapunk.toml` (endpoint, profile, embed_dim, default_scope).

## Also

- `working-state` get/put — coding profile restore-on (not soft-inject). A write without a version is create-only; read the slot and supply `--expected-version` for an update.
- `sleep` is dry-run by default. Near-duplicate proposals are review-only and are never automatically applied. Explicit age-out requires `--apply --apply-age-out --older-than-hours`; review the scope and proposals before authorizing it.
- After inject: **must** `trace-inject` then `trace-outcome` (same `trace_id`). Audit with `traces-by-memory --id …`.
- Before supersede: `candidates --query` for open-validity prior_id+version. No trusted prior → uncertainty.
- About to compact context: extract durable lines first, then `ingest-compaction --file discarded.txt --summary "…"`. Checkpoint runs only if every apply op succeeds. Default RuleWriter puts each non-empty line.
- Evidence bytes: `store-artifact --file` (CAS then metadata). Offline backup: `just snapshot` (stop writers first; do not mix CLI + long-lived `mcp`/`serve` on the same `rocksdb://`).
- `sleep` provides deterministic review proposals and explicitly enabled age-out — **not** in-product LLM reflection. Host-LLM reflection = extract `WriterOp[]` → `apply-ops`.
- `apply-ops` may preserve a committed prefix and fail later. Inspect `status` and every outcome, including `not_attempted`; never replay the whole batch blindly.
- A `reference_only` pack card omits the fact itself. Use `read --id <id>` (or MCP history) before relying on that content.
- If `embed_url` is set in `vegapunk.toml` / `--embed-url`, encode/recall/candidates are semantic; otherwise hashing or BM25 only.
- `hard-recall --min-score` is a **plane-score** floor; default `0.0` does **not** abstain.
- Temporal lenses: `--as-of` / `--known-as-of` / `--sys-as-of` (RFC3339) on `recall`, `hard-recall`, `list`, and `count`.
- After a pack/apply, `vegapunk traces` lists `trace_id`s in scope; then `get-trace` / `trace-inject` / `trace-outcome`. Filter with `--session-id` / `--turn-id`.
- `list` / `count --category uncertainty` inventories unresolved conflicts.
