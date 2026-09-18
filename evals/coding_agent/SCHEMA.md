# Coding-agent scenario schema

**Status:** locked for `sota_coding_v2`  
**Source of truth:** `evals/coding_agent/scenarios/*.json`  
**Compiler:** `nomiso-eval::coding_agent_suite`

One shared **scenario corpus**. Each product applies its native remember/read. Plane ingest uses `gold_memories` + `probes` only. `transcript` + `gold_ops` are the skill-track payload.

## File

```json
{
  "id": "p01_toolchain_cli_lang",
  "pattern": "p01_toolchain",
  "scope": "org/eval/coding/nomiso",
  "origin": "exemplar",
  "notes": "optional",
  "transcript": [{ "role": "user|assistant|tool", "text": "…" }],
  "gold_ops": [{ "op": "put|supersede|noop|forget", "…": "…" }],
  "gold_memories": [{ "key": "p01_rust_cli", "scope": "…", "text": "…", "category": "semantic", "with_embedding": true }],
  "probes": [{ "name": "p01_exact_rust_cli", "query": "…", "scope": "…", "tag": "exact", "expect_keys_in_top": ["p01_rust_cli"] }]
}
```

Unknown fields are rejected (`deny_unknown_fields` on the scenario envelope).

## Rules

| Rule | Why |
|---|---|
| `pattern` ∈ P1–P20 list | Locked families |
| Gold `key` unique across the **pack** | No compile-time prefix |
| `put` must not have `prior_id` | No invented priors |
| Skill `gold_ops` text must be transcript-supported | Plane `gold_memories` may keep retrieval keys the episode never said |
| `supersede.prior_id` is a gold **key** in the same scenario | Skill later resolves to `memory:` ids |
| Probe `expect_keys` / `must_not` exist in the pack | Fail closed |
| `expect_abstain` ⇒ empty `expect_keys` | Honest empty |
| `tag: multi_hop` ⇒ `skip_if_no_graph: true` | Graph not bound |
| `replace_group` on version pairs | Hindsight `document_id` replace for knowledge_update |
| At least one `origin: exemplar` per P1–P20 | Hand-checked coverage |

## Origins

- `exemplar` — hand-checked against the pattern lock  
- `generated` — LLM fill under this schema (same validator)

## Tracks

| Field | Plane (A) | Skill (B) | System (C) |
|---|---|---|---|
| `gold_memories` | gold put / foreign chunks | — | — |
| `gold_ops` | ignored | extract target | — |
| `transcript` | ignored | extract input | retain input later |
| `probes` | hit@k / leak / abstain | — | later QA |

Do not blend scores. Distractor scale (1k/10k rows) is a later labeled run, not this pack.
