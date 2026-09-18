# Head-to-head preregistration (frozen before live rows)

Date frozen: 2026-02. Harness: `nomiso-eval::h2h` (`EVAL_H2H_LIVE=1`).
This file fixes corpus, probes, channels, metrics, and acceptance labels
**before** any live answering-agent output was observed.

## Corpus

Bundled coding-agent scenarios (`nomiso-eval` fixtures, frozen JSON). Gold
facts = `gold_memories[].text` addressed by `probes[].expect_keys_in_top`.
`EVAL_H2H_LIMIT` may truncate scenario count for smoke runs; labeled rows
must state the limit used.

## Scored probe classes

Scored: `exact`, `paraphrase`, `hybrid`, `knowledge_update`, `abstain`,
`scope_isolation`, `uncertainty` (where fixtures carry them).
Skipped (declared, not dropped silently): `temporal`, `multi_hop`,
`skip_if_no_graph`, and any probe with `as_of`/`known_as_of`/`sys_as_of`.

## Channels

- `nomiso-t4` — real T4 loop per probe: `prepare_context` (budget
  max_tokens=1200, max_blocks=6, max_candidates=20) → host inserts the full
  selected subset → `record_insertion` verifies → presented = acknowledged
  blocks only.
- `hindsight` — local container. Presented = top-k recalled items
  (`budget=mid`, k=probe.limit). Foreign APIs have no insertion-ack
  protocol: citations verify against *retrieved* items, not an acknowledged
  subset. This asymmetry is reported, not hidden.
  Mode note: the running container is the `nomiso-hindsight-retain` image
  (llm-capable, OpenRouter `:free` configured), but the eval adapter pins
  `retain_extraction_mode=chunks` on every eval bank (verified via
  `GET /v1/default/banks/{bank}/config` → `overrides`), so retain never
  calls the LLM — ingest is plane-equivalent on both rows. A fresh
  `latest` pull on 2026-09-18 fails at boot (HF model download bug), so
  the rows below run the 6-day-old pinned container image `84ab276b8f50`.

## Answering agent

`grok` CLI headless: `--output-format plain --system-prompt-override
<rules> --always-approve --no-subagents --no-plan --verbatim -p <question>`
plus `--model grok-4.6 --effort low` (EVAL_H2H_MODEL/EVAL_H2H_EFFORT
override; label recorded in report). Same prompt for every channel:
evidence blocks `[id] text`, must emit `USED: [ids]` or `ABSTAIN`.

## Metrics (per system, per scored probe)

- `fact_hit` — answer asserts gold AND ≥1 declared-used block supports it.
- `leak` — answer asserts gold with zero declared-used support
  (parametric/unsupported).
- `citation_fraud` — declared id ∉ presented set (nomiso: ∉ acked subset).
- `abstain_ok` — abstain probes must abstain without asserting gold or
  forbidden text.
- `forbidden_asserted` — answer asserts a `must_not_include_keys` fact
  *without* asserting gold (stale claim / cross-scope leak); fails the row.
  Keyless negative probes (`must_not_include_keys` only) pass iff nothing
  forbidden is asserted — abstaining is correct isolation.
- Cost: presented block count, est tokens (chars/4), search_ms p50/p95,
  answer calls.

## Amendment 1 (2026-09-18, post first hash-row run)

The first live run exposed a scorer gap: `must_not_include_keys` was read
nowhere, so keyless scope-isolation probes could never pass and a
forbidden-text assertion would go unflagged. `forbidden_asserted` was added
(row field + per-system count + fail condition) and keyless probes now pass
on correct non-assertion. Both systems' p07 misses in that run were correct
abstentions mislabeled as failures — the row is rerun under the amended
scorer. This amendment was made before inspecting any system-level
conclusion beyond the misses list itself.

## Amendment 2 (2026-09-18, post second hash-row run)

The second run showed `forbidden_asserted` firing on knowledge-update
probes where the answer asserted the *current* fact — supersede fixtures
deliberately pair near-duplicate texts (v2 literally contains "Port 9191
is no longer the default"), so text-overlap cannot separate "asserted the
stale fact" from "asserted the corrected fact that lexically overlaps it".
`forbidden_asserted` is now conservative: it fires only when the answer
matches a forbidden text **without** asserting gold (a stale claim or
cross-scope leak with no gold cover). Gold-asserting rows are never
flagged. Consequence: same-scope stale-claim detection on near-duplicate
KU pairs is unreliable by construction and is not claimed; cross-scope
leaks with no gold assertion still flag (hindsight-chunks did fire on
`p07b_no_tact_leak` in run 2 — a real off-scope answer). Row rerun under
the amended scorer.

## Acceptance labels (frozen)

1. `nomiso-t4.citation_fraud_rows == 0` — structural property of the
   verified-ack path (fraud is impossible downstream).
2. `nomiso-t4.fact_hit ≥ hindsight.fact_hit` on the identical scored set —
   parity-or-better claim. If the numbers say otherwise, they are reported.
3. `nomiso-t4.leak ≤ hindsight.leak`.
4. `abstain_ok == 100%` per system; misses recorded.
5. All rows publish whatever they measure — labels gate the *claim
   wording*, never the report.

## Rows

| label | nomiso embedder | hindsight retain | answering |
|---|---|---|---|
| h2h-hash | hashing-8 (offline) | chunks (plane-equivalent) | grok-4.6 low |
| h2h-http | Ollama bge-small 384 | chunks (plane-equivalent) | grok-4.6 low |
| h2h-llm | Ollama bge-small 384 | llm extraction (OpenRouter :free) | grok-4.6 low |

Each row names its exact embedder/mode/model — never blended.
