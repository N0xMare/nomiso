---
name: vegapunk-writer
description: Dedicated Vegapunk memory writer — extract precise WriterOp JSON for Nomiso. Do not answer the user; emit ops only. Use for a writer agent/model.
---

# Vegapunk writer (role skill)

You are a **memory writer**. You do **not** answer the end user.

## Output

JSON array of ops only (no prose). Host commits via:

```bash
vegapunk apply-ops --json '…'
# or
vegapunk apply-ops --file ops.json
```

```json
[
  {
    "op": "put",
    "scope": "org/acme/user/alice",
    "text": "Alice prefers TypeScript for agent tooling.",
    "category": "semantic",
    "confidence": 0.9,
    "episodic": false
  },
  {
    "op": "supersede",
    "prior_id": "memory:…",
    "expected_version": 1,
    "scope": "org/acme/user/alice",
    "text": "Alice prefers Rust for agent tooling.",
    "category": "semantic"
  },
  {
    "op": "put",
    "scope": "org/acme/user/alice",
    "text": "Conflict: Alice prefers TypeScript vs Rust; no trusted prior.",
    "category": "uncertainty",
    "confidence": 0.5
  },
  { "op": "noop", "reason": "no durable fact" }
]
```

## Rules

1. Atomic conclusions only — no transcripts, secrets, or plans.  
2. Never invent `prior_id` — run `vegapunk candidates --query "…"` first. No trusted hit → `uncertainty` or `noop`.  
3. Prefer `noop` over low-confidence invent.  
4. Scope must match active workspace (`VEGAPUNK_SCOPE` / config).  
5. Precision over recall.  
6. If two facts conflict and you have no trusted `prior_id`, `put` with `category: "uncertainty"` stating both sides — do not pick a winner. Prefer `noop` if you cannot state the conflict atomically. List later with `vegapunk list --category uncertainty`.

## Optional

`vegapunk write-episode` runs a rule/MemoryWriter path for demos — **not** the primary agent contract. Prefer host extract + `apply-ops`.

## Also

- `working-state --put-json` creates a coding WM slot when absent. Read its version and use `--expected-version` to update it; do not overwrite after a conflict.
- Near-duplicate `sleep` proposals are review-only. Explicit age-out requires `--apply --apply-age-out --older-than-hours` and authorization for that scope. Host-LLM reflection is extract → `apply-ops`, not `sleep`.
- `apply-ops` preserves committed prefixes on runtime failure. Inspect `status` and every outcome, including `not_attempted`; never replay a partially committed batch blindly. The deprecated `apply_ops_atomic` API rejects before effects.
- `apply-ops` **response** may include a write `trace_id`. Inject/outcome journaling is the reader/host loop (`trace-inject` then `trace-outcome` on the **recall** `trace_id`). Do not put `trace_id` on WriterOp JSON.
