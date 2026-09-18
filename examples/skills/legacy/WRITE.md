# WRITE skill (reference)

> Historical plane playbook — **not installable**. Product writer: [`../vegapunk-writer/SKILL.md`](../vegapunk-writer/SKILL.md).

Use a frontier model **outside** Nomiso to turn a transcript into structured ops.

## Goal

Given conversation/tool context, emit a JSON array of `put` / `supersede` ops. Prefer **precision** over recall.

## Output contract (`nomiso.op.v1`)

```json
[
  {
    "op": "put",
    "scope": "org/acme/user/alice",
    "category": "semantic",
    "content": {
      "text": "Alice prefers TypeScript for agent tooling.",
      "attrs": { "preference": "typescript" }
    },
    "confidence": 0.9,
    "provenance": { "source": "session:abc", "kind": "user_statement" }
  }
]
```

For updates, call `nomiso_read` / `nomiso_search` first, then:

```json
{
  "op": "supersede",
  "prior_id": "memory:…",
  "expected_version": 1,
  "new": { "scope": "…", "category": "semantic", "content": { "text": "…" } }
}
```

## Rules

1. Never invent IDs — use `prior_id` from search/read for supersede.
2. One atomic fact per op when possible.
3. Do not store secrets (tokens, passwords).
4. Call `nomiso_put` / `nomiso_supersede` tools; Nomiso does not call models.
