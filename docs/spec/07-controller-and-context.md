# Controller workflows and the context boundary

Status: implemented baseline — `nomiso-memory::context` provides the typed
prepare-context request → `ContextProposal` + `SelectionManifest` contract,
and `record_insertion` provides the verified idempotent insertion ack.
`hard-recall --pack` remains the convenience compatibility path; the
controller contract is the target surface for harness integration.

## Controller responsibilities

The reusable controller coordinates typed components with explicit budgets, permissions, and receipts.
It does not become the task agent, execute arbitrary retrieved instructions, or own the harness's prompt buffer.
Vegapunk selects the component recipe and exposes convenient workflows.

## Target workflow contracts

The names below are target semantic operations, not commands already exported by the current CLI.

| Operation | Request | Result |
| --- | --- | --- |
| Observe | Allowed event, retention restrictions, event identity | Rejected, no-op, durably accepted, or completed receipt |
| Propose memory | Admitted evidence, relevant priors, producer configuration | Validated candidates or reasoned rejection |
| Apply proposal | Proposal identity, expected revisions, commit mode, idempotency key | Canonical mutation receipts and partial/failed status |
| Prepare context | Task/query, access, current inventory, budgets, temporal lens | Context proposal and selection manifest |
| Read evidence | Revision/artifact references and bounded excerpt request | Exact evidence or typed unavailable result |
| Record insertion | Proposal identity and actual selected blocks | Idempotent insertion acknowledgment |
| Record outcome | Operation/task identity, observed result, evaluator identity | Attributed outcome record |
| Consolidate | Scope/source frontier, policy, dry-run/apply mode | Proposal or explicit committed outcomes |

## Prepare-context request

| Field | Meaning |
| --- | --- |
| Request identity | Correlates calls and cancellation; not authority |
| Task description/query | Bounded statement of the information need |
| Active entities/resources | Optional explicit task references, never an implicit scope grant |
| Access context | Authorized scope set and operation/resource restrictions |
| Temporal lens | Valid/known/system constraints with defaults resolved once |
| Context inventory | Already-present revision/excerpt references, not necessarily the full prompt |
| Budget | Token allowance, retrieval/provider/latency limits, expansion bounds |
| Effort | Named bounded plan such as direct or expanded, resolved by composition |
| Representation preferences | Facts, procedures, evidence, summaries, or a permitted mixture |
| Degradation policy | Required versus optional components and failure behavior |

A host may omit inventory and receive conservative deduplication against only the candidates available to the controller.
The controller must disclose that it did not inspect the complete prompt rather than claim global context optimality.
Permissions and resource ceilings are hard constraints; relevance and usefulness are policy judgments.

## Write workflow

```text
allowed event → admission → optional evidence retention
  → extraction/proposal → prior and evidence validation
  → commit boundary → receipt → optional derived jobs
```

**CTX-001 — Explicit workflow states.** Results MUST distinguish admission, extraction, validation, committed effects, pending derived work, and failure. Durable acceptance of an event is not a claim that all extracted memories are ready.

**CTX-002 — Shared commit boundary.** The controller MUST apply every proposed mutation through document 04, including background and consolidation proposals. It cannot compensate for a failed operation by inventing a fresh operation identity.

Foreground explicit remember requests complete their required durable writes before reporting stored.
Optional embeddings/views may be pending only if the response identifies readiness and the composition permits lexical or evidence-only availability.

## Read workflow

```text
task + budget + inventory → bounded plan → candidate sources
  → optional graph/iterative expansion → ranking and selection
  → evidence-preserving excerpts → context proposal
```

**CTX-003 — Budgeted execution.** The controller MUST enforce limits across the whole workflow, including retries, graph expansion, model calls, and assembly. Per-component limits cannot collectively exceed the caller's global ceiling.

**CTX-004 — Selection beyond relevance.** Selection MUST expose reasons for inclusion/exclusion and consider mandatory eligibility before ranking. The baseline handles duplicate content, stale/disputed items, and available context inventory explicitly; it must not equate high similarity with safe applicability.

**CTX-005 — Oversized evidence.** A large first-ranked memory MUST NOT force an otherwise usable proposal to be empty when permitted shorter excerpts or later candidates fit. Excerpts preserve provenance and disclose omissions; an unsupported truncation must not invert or remove a condition/negation without warning.

The baseline prefers source excerpts over generated summaries for exact constraints.
If the budget cannot preserve a necessary qualifier, return a reference/follow-up need instead of presenting an incomplete assertion as sufficient.
Token accounting uses the selected tokenizer when available. An approximation is labeled, carries its method, and cannot be described as exact provider tokens.
A strict provider-token ceiling requires a compatible tokenizer or a validated conservative upper bound. Without one, the controller either refuses strict-token mode or uses an explicitly estimated-budget mode selected by the caller. Rendering overhead counts toward the budget. Provider-cost estimates and external billing are distinguished; the system reserves bounded work before dispatch and reconciles measured usage afterward.

## Context proposal and manifest

| Field | Target meaning |
| --- | --- |
| Proposal ID | Identity of one immutable selection result |
| Request/composition identity | Effective policy and request used to build it |
| Store/index frontier | Evidence and retrieval generation boundaries |
| Status | Ready, empty, insufficient evidence, budget exhausted, partial, or failed |
| Blocks | Ordered excerpts or structured context items with exact source references |
| Selection manifest | Candidate revision references, selected spans/digests, reasons, ordering, budget usage |
| Unresolved issues | Contradictions, missing evidence, stale candidates, optional-channel failures |
| Follow-up | Bounded suggested reads/retrieval; not automatically executed outside remaining budget |
| Rendered text | Optional convenience rendering of the same selected blocks |

**CTX-006 — Manifest fidelity.** Structured blocks, rendered text, and recorded selections MUST represent the same content. A renderer must not silently add unsupported claims or omit required qualifiers. Exact replay uses captured revisions/excerpts and composition identity.

A search trace may record candidates not selected for context.
That trace must not be mislabeled as actual injection.
Retaining full candidate text for diagnostics requires the privacy policy in document 11; fixture replay can use synthetic evidence and fully retained snapshots.

## Context boundary

**CTX-007 — Harness authority.** A context proposal MUST NOT directly modify the harness prompt, system policy, tools, or permissions. The harness decides placement and actual insertion under its own trust rules.

**CTX-008 — Evidence is not instruction authority.** Retrieved text remains attributed evidence. A memory that instructs the agent to change tools, reveal data, or ignore policy cannot gain authority by being ranked or placed in a pack. Instructions intentionally endorsed by the host remain distinguished from retrieved content.

**CTX-009 — Actual insertion acknowledgment.** The harness MUST be able to record which proposal blocks/revisions it actually inserted, including subset selection and truncation. The acknowledgment is idempotent and must not fabricate an insertion for a merely returned pack.

Compatibility with the current product:

- `hard-recall --pack` returns a convenience `pack.block` for explicit host insertion.
- The CLI trace ID is the top-level `trace_id`, not `pack.trace_id`.
- After actual insertion, use `trace-inject`; outcomes use `trace-outcome`.
- Working-state restoration is a separate operation and is not semantic memory injection.
- The former Layer 1 standing header remains unimplemented/off at the baseline.
- The former Layer 3 soft-inject setting remains reserved/off at the baseline.
- No repository-local per-turn recall/dump hook is introduced by this specification.

An optional future lifecycle policy may prepare context at explicit host events such as task start or a relevant source change.
It uses the same proposal, budget, permission, and acknowledgment contracts.
This is not permission to dump a scope every turn or install a hook without host authorization.

## Outcomes and feedback

**CTX-010 — Attributed outcomes.** Outcome records MUST distinguish observed execution results, host feedback, and model judgments. They identify the task/run and relevant proposal/operation. A helped/harmed label alone is not causal proof of memory utility.

Negative feedback may trigger review or an experiment; it must not automatically erase evidence.
A missing acknowledgment/outcome means unknown, not skipped, harmed, or helped.
A controller must not learn from later outcomes when replaying an earlier task snapshot.

## Failure behavior

- Permission or integrity failure: fail closed, no context proposal represented as trustworthy.
- Empty eligible evidence: explicit empty/insufficient result, not a provider error.
- Optional channel failure: return partial only when allowed, with the missing channel named.
- Required provider failure: typed failure, not an empty factual answer.
- Budget exhaustion: return selected material plus exhaustion status only if the contract permits partial context.
- Cancellation after writes: return/reconcile committed receipts; do not claim rollback.

## Acceptance

See `CTX` in [13](13-verification-and-evaluation.md): exact rendering, long-card behavior, permission boundaries, actual-insertion records, and budget enforcement.
