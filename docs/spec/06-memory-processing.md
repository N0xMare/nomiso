# Retention, extraction, procedures, and consolidation

Status: normative target for reusable Nomiso toolkit components.
Vegapunk selects a supported configuration; it does not own private copies of these mechanisms.

## Three separate decisions

| Decision | Possible results |
| --- | --- |
| Retain source evidence? | Reject, retain bounded evidence, retain reference only |
| Promote reusable memory? | No-op, candidate, endorsed claim/procedure, disputed record |
| Use in this task? | Exclude, expose as reference, select excerpt, request more evidence |

Evidence retention is not factual endorsement.
A transient tool result may be useful now and not worth persisting.
A failed experiment may be worth retaining without supporting a generalized lesson.

## Allowed events

A target event contains an idempotent event identity, owning scope, event kind, source/correlation references, occurrence time, bounded payload or artifact reference, and effective retention restrictions.
Initial kinds are explicit memory instruction, task outcome, tool observation, checkpoint request, compaction proposal, and source invalidation.
The harness chooses which events are supplied; toolkit construction must not begin collecting them implicitly.

**MEM-001 — Admission before inference.** Retention restrictions, payload limits, permission checks, and sensitive-content handling MUST run before remote model calls or durable writes. Unknown source permission is not permission to archive.

**MEM-002 — No raw-dump default.** The baseline composition MUST NOT treat raw conversation ingestion as synonymous with remembering. Raw evidence archival requires explicit opt-in, bounded retention, and provenance. An ordinary rule writer only ingests already-selected durable lines.

## Retention policy inputs

A policy may consider:

- Explicit user/operator memory instruction.
- Evidence support and source authority.
- Novelty relative to existing scoped memory.
- Expected reuse and reconstruction cost.
- Stability and invalidation conditions.
- Sensitivity and retention restrictions.
- Available processing/storage budgets.

The initial implementation uses documented deterministic decisions plus host proposals.
These factors are not a mandated numeric score or a license to invent confidence.
Learned admission policies are optional modules and must pass the same hard restrictions.

## Extraction contract

| Input | Output |
| --- | --- |
| Admitted evidence or references | Typed candidate assertions/procedures/relations |
| Task outcome and applicable environment | Explicit evidence and applicability references |
| Relevant scoped prior candidates | Proposed put/supersede/no-op/dispute operations |
| Extraction policy/provider identity | Producer version and safe explanation |
| Resource/deadline limits | Actual cost/status and partiality if permitted |

**MEM-003 — Proposals, not direct writes.** Extractors and consolidators MUST return typed proposals. They cannot commit directly, broaden scope, execute instructions found in evidence, or change permissions.

**MEM-004 — Prior discipline.** A proposed supersession MUST identify a real prior returned by authorized discovery or supplied trusted input. Conflicting statements without a trusted prior produce uncertainty/dispute or no-op. Similarity alone does not authorize a winner.

**MEM-005 — Parse failures are not durable success.** Invalid model output MUST be distinguishable from an intentional no-op. The controller reports extraction failure or a labeled fallback; it must not acknowledge an explicit remember request as stored merely because malformed output was converted to no-op.

Providers are replaceable; no supported baseline requires a named vendor CLI.
Prompt/config/model versions are recorded as producer identities, not embedded in canonical fact text.
A source statement that changes scope or says to ignore prior instructions is data, not authority.

## Validation and promotion

Proposal validation checks:

1. Schema and hard limits.
2. Scope/permission compatibility.
3. Evidence reference resolution.
4. Whether claims are actually supported by the cited evidence.
5. Current prior versions and open/closed state.
6. Applicability and conflicting memory.
7. Idempotent operation identities.
8. The requested commit mode.

Structural validation can be deterministic; support assessment may involve a host or optional model.
A model judgment is recorded as such, not as an executable test result.
Revalidation is required after source/prior changes before commit.

## Procedural memory

**MEM-006 — Scoped procedures.** A promoted procedure MUST declare applicability, preconditions, verification, failure/stop conditions, and supporting outcomes. A single success must not silently become an unrestricted always-rule.

Distinguish:

- An observed action sequence.
- A proposed explanation of why it worked.
- A reusable strategy.
- A strategy verified on a subsequent task.

Failure-derived lessons preserve uncertainty about root cause when the evidence only shows symptoms.
A procedure can be useful despite incomplete causal knowledge if it accurately describes its tested conditions and limits.
Source-code facts that are cheap to inspect may be better represented by a current source reference than a duplicated stale summary.

## Consolidation

Consolidation is a proposal-producing transformation over a declared input set/frontier.
Supported actions may include exact duplicate detection, derived summary creation, procedure extraction, relation proposals, stale-view repair, and explicit age-out proposals.
It must not silently overwrite original evidence.

**MEM-007 — Conservative automatic deduplication.** Automatic deletion/closure MUST be limited to demonstrably equivalent records under the same scope, representation, applicability, and evidence constraints. Removing negation, short identifiers, numbers, or versions to establish equivalence is forbidden.

Near-duplicate similarity generates review candidates, not an automatic truth decision.
The baseline may conservatively keep both records.
Duplicate suppression in a context proposal is not the same operation as deleting durable memory.

**MEM-008 — Source-preserving summaries.** A summary MUST identify its source revisions and coverage. It must disclose truncation or omitted source groups and remain separately revisable. Summarizing an earlier summary is permitted only with preserved lineage and loss accounting.

**MEM-009 — Guarded aging.** Age-out MUST be explicitly enabled with a retention rule and cutoff. Lack of a recorded read does not prove lack of value. Uncertainty records and actively referenced evidence are not automatically discarded by a generic age heuristic.

Consolidation dry-run returns proposals and reasons without mutation.
Apply uses the standard commit contract. Partial failures remain inspectable; a failed consolidation is not silently retried as a new batch.

## Source changes

A source update or purge invalidates affected derived views according to their recorded dependencies.
The system distinguishes stale content from contradicted content.
Reprocessing may create a replacement view; it must not pretend the original extractor saw future evidence.
A background job cannot upgrade its input snapshot after starting without producing a new plan/checkpoint identity.

## Compaction and checkpoints

**MEM-010 — Compaction is not endorsement.** Harness compaction may produce task-state summaries or memory candidates, but MUST NOT automatically endorse the entire discarded context as durable fact. A checkpoint acknowledges only the successfully committed effects it references.

A checkpoint failure after successful writes returns those receipts and its own error.
A no-op extraction can complete processing without storing memory; the result says no-op, not stored.
Plans remain task state unless explicitly transformed into a supported, evidence-grounded reusable procedure.

## Experimental policies

Learned retention, automatic relation extraction, more aggressive summaries, and proactive selection are supported extension directions.
Their outputs pass the same validators and mutation boundary.
Promotion requires the component/system evaluation protocol in document 13, including harmful-memory cases and total processing cost.

## Acceptance

See the `MEM` acceptance family in [13](13-verification-and-evaluation.md).
Use adversarial negation/version examples, invalid model outputs, stale priors, unsupported lessons, and explicit no-op cases.
