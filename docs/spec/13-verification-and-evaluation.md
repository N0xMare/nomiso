# Verification, evaluation, and release evidence

Status: normative target protocol, with a separately labeled current gate.
This document specifies acceptance and measurement; it does not claim those future tests already exist.

## Separate kinds of proof

| Track | Inputs | Proves | Does not prove |
| --- | --- | --- | --- |
| Foundation conformance | Structured operations and controlled stores | Mutation/query/recovery invariants | Model extraction or useful agent behavior |
| Memory-policy evaluation | Allowed evidence, proposals, priors, outcomes | Supported retention, revision, procedures, selection | Complete agent success from retrieval alone |
| Agent-system evaluation | Fixed tasks, host/model/tools, memory snapshots | Task outcomes, grounding, repeated mistakes, efficiency | Universal optimality or every deployment mode |

**TEST-001 — Separate tracks.** Reports MUST keep foundation, memory-policy, and agent-system results distinct. A single blended memory quality percentage is not a valid release claim.

**TEST-002 — Honest run status.** Report runner status, availability, scored cases, missing cases, skipped cases, and quality results separately. A skipped provider test is not a pass for that provider; a successful runner is not a quality pass.

**TEST-003 — Reproducible inputs.** Each experiment MUST identify source/task snapshots, contract/composition/index/model versions, budgets, scorer version, environment, and repetition/selection protocol. Future outcomes must not enter earlier memory snapshots.

**TEST-004 — No fixture retuning to hide misses.** Gold labels, expected outcomes, sampled tasks, and thresholds MUST NOT be changed after seeing a candidate result without declaring a new experiment. Correcting an erroneous fixture requires rationale and preserved prior results.

**TEST-005 — Full cost accounting.** Measure foreground and background inference, ingestion, indexing, retrieval, retries, tool calls, and relevant storage work separately. Moving work off the foreground path is not automatically a total-cost reduction.

**TEST-006 — Evidence before promotion.** A module MUST pass its hard conformance tests and a preregistered workload comparison before becoming a default. A new model, ranking recipe, or budget creates a new evaluated composition.

## Requirement-to-acceptance map

Each identifier below is owned and defined in its corresponding specification document.
The test families describe target tests; the transition register identifies which are currently implemented.
One case may exercise several requirements, but the recorded result must identify its coverage.

| Requirements | Required acceptance family |
| --- | --- |
| GOV-001, GOV-002, GOV-003, GOV-004, GOV-005 | Spec authority/status, unique ownership, change traceability, current-state evidence, tree/line/link checks |
| PUR-001, PUR-002, PUR-003 | Distinguish observed outcomes, retained evidence, endorsed claims, and inserted context; repetition does not upgrade truth |
| PUR-004, PUR-005, PUR-006, PUR-007, PUR-008 | Independent foundation/toolkit/product use; model-free operation; host context authority; experimental promotion controls |
| ARCH-001, ARCH-002, ARCH-003, ARCH-004 | Direct Rust use, dependency-cycle checks, provider-independent metadata, alternative composition |
| ARCH-005, ARCH-006, ARCH-007, ARCH-008 | Isolated feature builds, invariant parity across adapters, constructor side-effect tests, module-boundary review |
| DATA-001, DATA-002, DATA-004, DATA-005 | Assertion/revision/lineage separation, exact historical resolution, immutable content, legacy-unavailable behavior |
| DATA-003, DATA-006, DATA-007 | Epistemic status, corrupt/missing evidence, digest/range validation, derivation lineage |
| DATA-008, DATA-009 | Half-open boundaries, retroactive corrections, known-time filtering versus exact snapshot reads |
| DATA-010, DATA-011 | State-slot isolation/CAS, no implicit promotion, extension-schema bounds and reserved-field rejection |
| WRITE-001, WRITE-002, WRITE-003 | Preflight before effects, explicit commit state, required-effect transaction fault injection |
| WRITE-004, WRITE-008 | Competing supersedes, stale expected versions, one-shot closure, authoritative CAS |
| WRITE-005, WRITE-006, WRITE-007 | Concurrent replay, mismatched key payload, prefix stop/not-attempted results, no false rollback |
| WRITE-009, WRITE-010 | Cancellation around commit, receipt reconciliation, failed erase does not claim success |
| REL-001, REL-002, REL-003, REL-004 | Cross-scope endpoints, versioned edges, cycles/limits, scoped invalidation |
| RET-001, RET-002, RET-003, RET-004 | Explicit channels, filtered candidates, fixed query frontier, score/provenance labeling |
| RET-005, RET-006, RET-007, RET-008 | Malformed store responses, stable pagination/counts, filtered top-k refill/completeness |
| MEM-001, MEM-002, MEM-003, MEM-004, MEM-005 | Admission restrictions, no raw-dump default, typed proposals, real priors, invalid-output versus no-op |
| MEM-006, MEM-007, MEM-008, MEM-009, MEM-010 | Procedure applicability, contradictory duplicates, summary coverage, guarded aging, compaction/checkpoint truth |
| CTX-001, CTX-002, CTX-003, CTX-004, CTX-005 | Workflow states, shared commits, total budgets, selection reasons, long/qualified evidence |
| CTX-006, CTX-007, CTX-008, CTX-009, CTX-010 | Manifest/render fidelity, host authority, poisoning resistance, actual-insertion subset, attributed outcomes |
| JOB-001, JOB-002, JOB-003, JOB-004 | Enqueue durability, duplicate execution, stale fence, foreground-update conflict |
| JOB-005, JOB-006, JOB-007 | Retry/deadline bounds, cancellation receipts, inspectable restart state |
| PROD-001, PROD-002, PROD-003, PROD-004 | Shared components, simple builder, immutable recipe identity, conservative defaults |
| PROD-005, PROD-006, PROD-007, PROD-008 | Experimental labels, host-owned execution, independent consumer, supported-surface declarations |
| API-001, API-002, API-003, API-004 | Library/adapter parity, lifecycle control, missing config/path behavior, metadata without providers |
| API-005, API-006, API-007, API-008, API-009 | CLI exit/envelope, HTTP status, real MCP lifecycle/errors, SDK replay, harness acknowledgments |
| SEC-001, SEC-002, SEC-003, SEC-004, SEC-005, SEC-006 | Permission versus scope, cross-surface enforcement, serving exposure, sensitive input, poisoning, resource exhaustion |
| OPS-001, OPS-002, OPS-003 | Provider failures/malformed vectors, CAS corruption/escape, declared deletion scope and worker resurrection prevention |
| OPS-004, OPS-005, OPS-006 | Quiescent/snapshot backup, invalid archive/mixed generation restore, privacy-aware diagnostics |
| MIG-001, MIG-002, MIG-003 | Newer schema refusal, interrupted/concurrent migration, legacy identity and uncertainty |
| MIG-004, MIG-005, MIG-006 | Same-dimension model drift, staged index activation, explicit semantic version changes |
| PKG-001, PKG-002, PKG-003, PKG-004 | Actual packages, source-tree-independent execution, feature closure, minimum-toolchain/platform evidence |
| TEST-001, TEST-002, TEST-003, TEST-004, TEST-005, TEST-006 | Track/status separation, run manifests, holdout and scorer history, cost accounting, promotion review |

## Core regression scenarios

### Mutation and history

- Concurrent supersession of one open prior: exactly one committed successor; loser reports conflict.
- Failed atomic group after preparing an edge: no half-linked canonical state.
- Commit succeeds and response is lost: replay returns the same receipt/IDs without another provider call when the receipt is sufficient.
- Same idempotency key, different payload: explicit conflict and no new effect.
- Prefix put succeeds, next supersede fails, third op exists: first remains, second errors, third is explicitly not attempted.
- Supersede succeeds then later batch work fails: no hidden compensation that leaves both predecessor and successor invalid-now.
- Old revision read after annotation/closure: exact captured state or explicit legacy-unavailable, never the latest version masquerading as old.
- Scheduled validity, equal bounds, reversed bounds, late-known source, and retroactive correction: each time axis follows its stated contract.
- Hard erase conflicts: no successful erase journal entry.

### Retrieval and context

- Exact scope A excludes A/B and unrelated scopes; prefix A includes only authorized descendants.
- Graph path enters an unauthorized or foreign-scope node: no content/path leak.
- More than one page of text-filtered results with differing ranks: no duplicate/omitted inventory or false exact count.
- High-ranked recent candidates fail a historical filter while eligible old candidates exist: refill or explicit incomplete result.
- First memory exceeds context budget while later material fits: useful bounded proposal or explicit insufficiency, not accidental global empty output.
- Negation/version/identifier appears near an excerpt boundary: do not present a condition-free or inverted assertion.
- Context inventory already contains a revision: selection records deduplication rather than repeating it blindly.
- Manifest blocks and rendering: selected IDs, versions, excerpts, ordering, and actual insertion agree.
- Empty pack, provider failure, and budget exhaustion remain distinct statuses.

### Processing and workers

- "Must use signed artifacts" versus "must not use signed artifacts": not automatic equivalent duplicates.
- Dependency versions and short numeric identifiers differ: similarity does not authorize deletion.
- A failed test run offers several plausible causes: no inferred cause promoted as verified without discriminating evidence.
- A model emits invalid JSON or fabricated prior: extraction/validation fails distinctly from an intentional no-op.
- A plan in compaction: task-state candidate, not automatically an established memory.
- Kill worker after effect commit but before job completion: no duplicate effect on resume.
- Expire lease and resume old worker: stale fence cannot finalize.
- Purge source while a derivation job is pending: no republishing erased content.

### Providers, storage, and interfaces

- Embedding output has duplicate/out-of-range indexes, wrong cardinality/dimension, or nonfinite values: reject.
- Same-dimension different model/index identity: reject or explicitly stage a new generation.
- Missing explicit config: fail before opening another bank.
- Config stored outside cwd: target path-resolution rules remain deterministic.
- Metadata/state read with provider unavailable: succeed if storage is healthy and the operation needs no provider.
- CAS final object exists with corrupt bytes: integrity failure rather than existence-based success.
- Restore archive contains traversal/symlink escape, incompatible manifest, or invalid destination: reject before replacement.
- Restore to a new root with the original root unavailable: resolve artifact metadata and verify bytes through the restored backend.
- MCP initialize/list/call/error/shutdown: test the actual protocol, not a manually maintained name array.
- Installed binary runs outside the source tree: no missing baked-in fixture paths.

## Foundation metrics

Define the eligible scored query set before running the experiment.
For a query with nonempty expected revision/key set E and returned top-k set R:

- All-required hit@k: `1` if `E` is a subset of `R`, otherwise `0`.
- Recall@k: `|E intersect R| / |E|`.
- Forbidden-result violation: `1` if any declared forbidden eligible identifier appears in the returned results, otherwise `0`.

Queries with empty E are not counted as perfect positive-recall cases.
Abstention and negative/isolation queries have their own denominators.
Ranked relevance scores are not compared numerically across engines or ranking recipes.
Approximate-result completeness and degraded-channel status are reported separately.

Knowledge-update tests must say whether they exercise a real revision operation or merely ingest pre-authored closed/open intervals.
Both are useful, but the latter does not prove mutation correctness.
Graph cases remain scored in graph-capability experiments even when a candidate cannot answer them; they cannot disappear from the denominator because the graph failed.

## Policy/extraction metrics

Report separately:

- Structural validity of proposals.
- Supported retained claims versus unsupported additions.
- Coverage of expected durable information.
- Correct prior selection and version handling.
- Correct uncertainty/no-op behavior.
- Sensitive/prohibited retention violations.
- Procedure applicability and verified transfer.

Define matching units and one-to-one matching before scoring precision/recall.
One vague output must not receive credit for multiple distinct expected facts through loose shared-word overlap.
A no-op is evaluated against the case's expected admission behavior; it is not automatically a successful extraction.
Gold-apply tests establish mechanics, not a writer's ability to extract the gold.

## Agent outcome and grounding protocol

The target system scorer separates five dimensions:

1. Retrieval coverage: supporting evidence reached the proposal.
2. Insertion coverage: supporting evidence actually entered the host context.
3. Answer/task correctness: the final answer or executed task met its acceptance contract.
4. Grounding: material factual claims are supported by resolvable citations/evidence.
5. Harm: contradicted, unauthorized, or inapplicable memory influenced the answer/action.

A positive factual answer passes only if all required answer units are correct, required qualifications are preserved, and no disallowed contradiction is asserted.
A correct pack with a missing or wrong answer does not pass answer correctness.
A single matching word or substring is not a semantic correctness test.
For abstention cases, the host must explicitly and appropriately decline the unsupported answer; an empty pack, an absent answer, or the word ABSTAIN inside a contradictory answer is insufficient.
Required recall discipline is measured from execution/insertion records, not model self-report.

Citation checks resolve the referenced revision/span and test whether it supports the actual claim, including polarity, entity, applicability, and time.
A syntactically valid citation to irrelevant evidence fails grounding.
Forbidden-memory leakage is checked independently of expected-text overlap; a contradiction does not become safe because old and new claims share words.
For semantically defined forbidden claims, the adjudication rubric must include paraphrases rather than only exact sentence matching.

Use deterministic task checks when possible, such as successful tests and explicitly verified state changes in an isolated task environment.
When a model judge is needed, freeze its model/version, rubric, inputs, and uncertainty handling; validate it against reviewed positive and negative cases.
Report uncertain/adjudication-required cases separately rather than silently forcing them to pass.
Do not place held-out outcomes or judge explanations into memory accessible to the evaluated run.

## Baselines and promotion

Compare a proposed capability against relevant strong baselines:

- No durable memory.
- Simple notes/files plus search.
- Conservative direct lexical/vector/hybrid memory.
- Iterative host-driven retrieval.
- Fuller context where feasible under a declared cost budget.

Hold model, tools, permissions, tasks, and source evidence comparable.
For a graph experiment, include relational questions and ordinary factual lookups.
For procedures, test transfer to a later different task and harmful overgeneralization.
For context policies, compare downstream success and total cost rather than packing density alone.

Before evaluation, register the primary endpoint, permitted regressions, hard safety constraints, cost budget, sample/repetition design, and uncertainty analysis.
No fixed percentage threshold in this specification is a universal quality target.
Hard correctness/permission/integrity violations block promotion regardless of average task score.
Report per-task paired results and uncertainty appropriate to the sample; small suites do not justify sweeping claims.
An independent review checks both the implementation and the scorer before default promotion.

## Current local gate

These commands exist at the audited baseline; they are not the complete target acceptance suite:

```bash
just check
just smoke
just smoke-durable
just smoke-durable-mp
just smoke-agent-durable
just smoke-coding-loop
```

`just foundation` runs the check and smoke ingredients together.
`just check` covers formatting, workspace Clippy, configured library/tests for plane, surfaces, Vegapunk, and Tact.
The pre-push hook runs `just check`, not the complete durable release bar.
Local Justfile-based verification remains the project convention; this specification does not introduce GitHub Actions.

Run the narrowest relevant tests during an implementation slice, then the broad gate at a milestone.
A documentation/template-reference-only change does not require re-running every live evaluation.
For this specification consolidation, relevant runtime verification is the CLI example-TOML parser test plus formatting/diff hygiene because only a documentation path in the template changed.
No runtime acceptance claim follows from the specs being written.

## Existing evaluation compatibility

The current fixtures and runners remain outside `docs/` and are not retuned by this consolidation.

| Existing track | Interpretation to preserve |
| --- | --- |
| Plane gold ingest | Retrieval over provided facts; hashing is lexical-only |
| Skill gold apply | Operation-application gate, not live extraction quality |
| RuleWriter report | Line-ingestion baseline, not semantic extraction |
| Live skill | Model extraction under its recorded protocol; report-only |
| Current system | Gold encode plus host hard-recall; fact-hit checks pack plus answer, not answer correctness alone |
| Citation-grounded h2h (`nomiso-eval::h2h`) | Answerer declares `USED: [block_ids]`, verified against the *actually inserted* set (Nomiso: `record_insertion` ack; foreign channels: retrieved items — the asymmetry is labeled). Scores grounded fact-hit / leak / citation-fraud / abstain / forbidden-assertion plus latency and estimated tokens on the frozen coding corpus; preregistration in `evals/h2h-preregistration.md` |

Historical system reports may set `passed` for runner success and contain full-pack versus preview-era differences.
The target protocol above is stronger; new results need a new scorer version and must not be blended with those rows.
Foreign Hindsight/Mem0/Zep comparisons require comparable ingest, embedding, budgets, and available services.
Hashing-versus-semantic comparisons cannot establish semantic superiority.
Do not switch or reset a live comparison service merely to run the local gate.
Optional live checks require explicitly configured, isolated services and credentials; unavailable services are reported as skipped.

## Specification hygiene checks

Verify all of the following after consolidation:

1. Exactly the intended Markdown specifications remain under `docs/spec/`; no documentation files remain elsewhere inside `docs/`.
2. Total line count is at most 25,000.
3. Every local Markdown link and fragment resolves, including inbound links in edited READMEs/skills/config references.
4. Every bold requirement definition is unique and every defined requirement appears in the acceptance map.
5. Every reference to a requirement resolves to one definition.
6. All 15 retired documents have an explicit disposition in document 14.
7. Current-state claims are tied to the audited baseline; future APIs are labeled target.
8. No secrets, fabricated benchmark results, runtime logs, or stale private-path dependencies are copied into the specifications.
9. No frozen evaluator inputs or scoring logic were changed during documentation cleanup.
10. The complete diff is reviewed; Markdown links alone do not establish semantic consistency.
