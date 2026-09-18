# HARD_RECALL skill (reference)

> Historical plane playbook — **not installable**. Product reader: [`../vegapunk-reader/SKILL.md`](../vegapunk-reader/SKILL.md).

Multi-step search when the first hybrid pass is weak.

## Procedure

1. Call `nomiso_search` with the user question (exact scope).
2. If results are weak/empty:
   - Reformulate query (synonyms, entity names, error codes).
   - Retry with `scope_match: "prefix"` if hierarchical include is intended.
   - Optionally supply a query embedding from your embedder (Nomiso does not embed).
3. `nomiso_read` top ids for full content + provenance.
4. Synthesize an answer **in the harness**, citing Nomiso ids.

## Rules

- Core stays read-only and filter-enforcing.
- Do not dump entire scopes into the prompt.
- Prefer ≤K cards (default limit 8).
