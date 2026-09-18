---
name: vegapunk
description: Durable Nomiso memory via the Vegapunk AXI CLI. Use before project-specific claims (locks, decisions, error lessons) and when persisting a fact. Triggers - hard-recall, apply-ops, remember, working-state, format gate, Surreal-only.
---

# Vegapunk (Pi)

You do **not** write SurrealQL. You do **not** dump the memory bank. Call the CLI with `bash`.

Prefer `./target/debug/vegapunk` from the repo root. If missing: `cargo build -p vegapunk-cli`. Flags: `--no-help --format json`. Pass `--session-id` / `--turn-id` when `PI_SESSION_ID` is set.

## Recall (Layer 2)

Before answering with a repo-specific fact:

```bash
./target/debug/vegapunk --no-help --format json hard-recall --query "<keywords>" --pack
```

- Inject **only** `pack.block`. Cite memory ids. A `reference_only` card requires `read --id <id>` before relying on its omitted content.
- Then `trace-inject --trace-id <trace_id> --id memory:…` and `trace-outcome --trace-id <id> --outcome helped|harmed|skipped`.
- Empty / abstain: rewrite the query once (error codes, names). Still empty → say you do not know. Do not invent.

## Write

```bash
./target/debug/vegapunk --no-help --format json apply-ops --json '[{"op":"put","scope":"<scope>","text":"<atomic fact>","category":"semantic"}]'
```

- One atomic fact per put. Scope from `vegapunk.toml` / `VEGAPUNK_SCOPE` unless the episode names another.
- No `prior_id` unless the episode or `candidates` returned a real id. Empty prior → noop.
- Two conflicting claims, no prior → `category: "uncertainty"`. Never pick a winner.
- Secrets, raw chat, plans → noop.

Before supersede: `candidates --query "…"`.

## Working-state

```bash
./target/debug/vegapunk --no-help --format json working-state
./target/debug/vegapunk --no-help --format json working-state --put-json '{"goal":"…","file":"…"}'
```

Coding profile restore-on. Not soft-inject. Not a standing header. A write without `--expected-version` is create-only; get the current version before updating an existing slot.

## Never

- Per-turn memory dump or a Pi extension that recalls in `before_agent_start`
- Mixing Mem0 / Hindsight-pi into this project
- Treating hashing embeddings as a Hindsight semantic score
