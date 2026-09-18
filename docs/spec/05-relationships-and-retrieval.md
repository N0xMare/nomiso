# Relationships and retrieval primitives

Status: normative target. Graph traversal, graph enrich, and opt-in graph candidate expansion (`graph_expand` on `SearchQuery`) are implemented; expansion remains a T5 experiment gated on evaluation, not a default.

## Relationships are assertions

| Field | Target contract |
| --- | --- |
| Identity | Opaque relationship ID and optimistic version |
| Scope | One owning scope; baseline endpoints must be in that same scope |
| Subject/object | Typed endpoint references, optionally pinned to exact revisions |
| Predicate | Registered, versioned relation type, not executable query text |
| Evidence | Supporting artifact spans or memory revisions |
| Epistemic status | Reported, inferred, verified, disputed, or unknown |
| Validity | Half-open world interval plus store audit clocks |
| Producer | Source/extractor/policy version |
| State | Active, closed, stale, or purged, with explicit reason |

Initial relation families: `supports`, `derived_from`, `contradicts`, `supersedes`, `mentions`, `depends_on`, `applies_to`, `observed_in`, `attempted`, and `resolved_by`.
A relation registry defines allowed endpoint kinds, direction, required metadata, and whether closure/invalidation propagates.
An inferred `resolved_by` or causal explanation is not equivalent to a verified causal relationship.

### Initial relationship signatures

| Predicate | Direction and endpoint kinds | Revision/invalidation behavior |
| --- | --- | --- |
| `supports` | Evidence span or assertion -> claim/procedure | Pin source revision; source changes mark support for review |
| `derived_from` | Derived assertion/view -> evidence span or assertion | Pin every derivation input; changes mark the derived view stale |
| `contradicts` | Assertion <-> assertion | Symmetric claim about incompatibility; preserve both references, no automatic winner |
| `supersedes` | Successor assertion -> predecessor assertion | Foundation-maintained lineage, never a free-form model edge |
| `mentions` | Observation/assertion -> entity | Source-revision-bound mention; alias changes cannot rewrite historical identity |
| `depends_on` | Entity/resource -> entity/resource | Evidence-backed dependency with applicable versions/time |
| `applies_to` | Procedure/claim -> entity/resource | Applicability assertion, not an execution permission |
| `observed_in` | Observation/incident -> evidence span | Exact supporting artifact/span |
| `attempted` | Episode/incident -> procedure/action-observation | Records an attempt, not successful verification |
| `resolved_by` | Incident -> repair/procedure assertion | Distinguish observed recovery from inferred causation |

Entity/resource records have scoped opaque identity, versioned descriptions, namespaced kinds, and sourced aliases. Entity creation/read and alias proposals use typed foundation operations. Name similarity never merges identities automatically. Support and derivation edges require exact revision/span references; live entity references are permitted only for relation types explicitly defining their invalidation behavior. Unknown predicate schemas are rejected before mutation.

**REL-001 — Scoped integrity.** Relationship creation and traversal MUST enforce endpoint existence, owning scope, effective permissions, and relation schema. The baseline rejects cross-scope edges; future federated edges require a separate explicit authorization design.

**REL-002 — Versioned relationships.** Edge changes MUST preserve prior revision resolution and evidence. Entity merging or alias resolution must not silently reconnect historical evidence to a different identity.

**REL-003 — Bounded traversal.** Traversal MUST accept and enforce depth, visited-node, edge, candidate, and time limits. It deduplicates visits, handles cycles, and returns truncation/completeness metadata and supporting paths.

**REL-004 — Evidence-sensitive invalidation.** A changed source MUST mark dependent derived relationships/views stale or durably schedule that action. Invalidation must remain scope-bounded and must not silently assert a new relationship.

## Foundation query contract

A target retrieval request identifies:

- Store/snapshot frontier and normalized scope filter.
- Permission grant supplied by the host or transport boundary.
- Query text, structured keys, or explicitly identified vectors.
- Temporal and representation/category filters.
- Candidate source/mode and index generation.
- Result, candidate, traversal, and deadline budgets.
- Optional continuation token for operations that support it.

**RET-001 — Explicit channels.** Callers MUST be able to select lexical, vector, structured, and supported graph primitives independently. Attaching an embedder must not force a vector call for an explicitly lexical request. The optimized hybrid convenience operation remains available.

**RET-002 — Filter correctness.** Scope/permission and applicability filters MUST constrain eligible results in every channel. Post-filtering cannot leak forbidden candidates. If bounded approximate retrieval cannot fill the requested eligible set, report partial completeness rather than claiming an exhaustive answer.

**RET-003 — Query identity.** Effective filters, resolved time defaults, snapshot/frontier, index generation, and retrieval mode MUST be returned or referenced in diagnostics. A continuation token cannot silently change those parameters.

Completeness is explicit: `complete_for_contract`, `bounded_approximate`, `partial`, or `failed`. Filling k slots in HNSW does not prove exact nearest-neighbor completeness. Exhaustive enumeration/count and approximate ranked retrieval expose different guarantees. Snapshot/index mismatch or exhausted filtered refill cannot be hidden by a nonempty result.
Each `CandidateBatch` carries `completeness` and an optional `incomplete_reason`; a composed retrieval result carries its own `completeness` plus `channels[]` entries with source identity, status, and completeness. Aggregate completeness is never stronger than the guarantees required by the requested plan: a missing required channel fails the request, while an allowed missing optional channel yields partial status. Approximate channels retain their bounded-approximate label even when the requested number of candidates is returned.

No model-generated text becomes raw SurrealQL.
Bind request values; whitelist any structural query choices such as predicate or index names.
No query silently scans an entire corpus to emulate vector search in a production path advertised as indexed.

## Candidate contract

| Field | Meaning |
| --- | --- |
| Revision reference | Exact content/state selected, not only a mutable ID |
| Representation/category | What kind of material this is |
| Scope and validity | Eligibility metadata available to downstream selection |
| Excerpt/reference | Bounded preview plus full-evidence retrieval mechanism |
| Channel/rank | Which candidate source found it and at which rank |
| Score/score kind | Engine score, distance-derived score, rank fallback, or policy score |
| Evidence paths | Relationship paths or provenance supporting discovery |
| Staleness/conflicts | Known invalidation or dispute indicators |
| Cost | Actual measured cost or explicitly labeled estimate |

**RET-004 — Score honesty.** Raw BM25, vector distance, RRF rank mass, reranker relevance, and model confidence MUST NOT be presented as calibrated substitutes. Zero/missing engine scores and rank fallback remain distinguishable. Channel participation is not proof of relevance or truth.

**RET-005 — Decoding integrity.** Invalid database response shapes MUST produce an integrity/compatibility error, not silently become empty results, invented categories, epoch timestamps, or default versions.

## Baseline retrieval

Current foundation search uses Surreal FULLTEXT/BM25 and HNSW, with in-engine `search::rrf` when both channels participate.
The target retains this optimized baseline while permitting other typed compositions above it.
Pure Rust fusion is permitted for reusable policies; its use is not an architectural violation.

A candidate-generation failure is distinct from a valid empty result.
A missing optional channel may yield a partial response only when the request/composition allows degradation and the missing channel is disclosed.
A required channel failure terminates the retrieval workflow with a typed error.

Hashing embeddings are lexical proxies, not semantic paraphrase evidence.
Vector requests require matching index/model identity, dimension, finite values, and supported normalization.
Different embedding generations must not be mixed in one query unless a policy explicitly treats them as separate candidate sources.

## Graph expansion policy

1. Select seed entities, memories, or evidence references under the same access filter.
2. Expand only allowed relation types and temporal windows.
3. Track visited identities and path provenance.
4. Bound expansion before ranking, not just final output.
5. Merge duplicate candidates while retaining discovery paths.
6. Rank/fuse without converting association into factual endorsement.
7. Preserve a direct-search fallback and report expansion cost/truncation.

Deterministic code/dependency relationships are preferred for the initial coding experiment.
Model-extracted associative links are a separate capability with separate quality gates.
Not every multi-hop question requires graph storage; iterative direct retrieval remains a comparison baseline.

## Listing, counts, and cursors

**RET-006 — Stable pagination.** Enumeration MUST use a stable ordering and a cursor containing all ordering keys plus query/snapshot identity. A score-ordered result cannot advance with only timestamp/ID keys. Invalid or incompatible cursors return errors.

The baseline target inventory order is `valid_from DESC, id DESC` under a fixed request frontier.
A ranked search need not support pagination unless it supplies a stable ranking/snapshot contract.
Text-filtered enumeration must either use inventory ordering or a fully specified rank cursor; it must not reuse an incompatible cursor.

**RET-007 — Honest counts.** Count MUST use complete enumeration or a proven aggregate under the same filters/frontier. A safety cap produces an explicit incomplete/lower-bound result, never a silently exact integer.

**RET-008 — Temporal completeness.** Known/system/valid filters MUST be applied before limiting where supported, or retrieval must refill within its declared budget. Exhausted refill reports incompleteness. Increasing an over-fetch constant is not a proof of correctness.

A `ReadRequest` by ID may explicitly request historical/closed records, unlike default valid-now search.
The API must distinguish missing, forbidden, erased, and unavailable historical references without violating disclosure policy.

## Candidate discovery for updates

`find_candidates` proposes eligible priors; it does not decide supersession.
Its target results identify whether the assertion is open for mutation and include the required optimistic version.
A valid-now assertion with a scheduled end is not necessarily an open predecessor.
The mutation transaction always rechecks eligibility.

## Graph non-goals for the baseline

- Unbounded recursive walks.
- Cross-tenant traversal by following an edge.
- A graph writer required for ordinary memory use.
- Mandatory community summarization on every query.
- Automatic causal inference from chronological adjacency.

## Acceptance

See `REL` and `RET` in [13](13-verification-and-evaluation.md): filtering before limits, cursor stability, cycle/budget handling, source integrity, and channel labeling.
