# Mutations, transactions, idempotency, and failures

Status: normative target. Legacy batch behavior is cataloged in [14](14-current-state-and-transition.md).

## Commit boundary

All manual, model-assisted, and worker mutations pass through the same foundation validation and commit boundary.
Models propose typed operations; no model-returned SQL or shell commands are executed as memory operations.

**WRITE-001 — Validate before effects.** Validate operation shape, limits, normalized scope, effective permissions, interval defaults, and supplied vectors before provider calls or database writes. Store-dependent validation is repeated against authoritative state at commit.

**WRITE-002 — Explicit outcomes.** Every mutation result MUST distinguish committed, rejected, partial, and indeterminate outcomes. A transport interruption after possible commit cannot be reported as a confirmed rollback.

**WRITE-003 — Required atomicity.** An assertion mutation, its required lineage/relationship changes, required belief journal event, and idempotency receipt MUST commit together when the operation promises atomicity. Optional usage telemetry is not part of that guarantee and its failure is separately visible.

## Operation families

| Operation | Preconditions | Successful result |
| --- | --- | --- |
| Put assertion | Valid payload, authorized owning scope, valid embedding identity | New ID, version 1, exact revision reference, commit receipt |
| Supersede | Trusted prior ID, exact expected version, same owning scope, open predecessor | Closed prior, new assertion, lineage links, journal, receipt in one transaction |
| Annotate | Authorized metadata fields, expected version | New metadata revision; immutable content preserved |
| Soft forget | Authorized ID/version and admissible close instant | One-shot closure; retained historical revision |
| Purge | Explicit administrative authorization and declared deletion scope | Tracked deletion result including remaining retained artifacts/backups |
| Put working state | Explicit create or expected version | Updated slot and new optimistic version |
| Relate/revise relation | Authorized endpoints, endpoint revisions, valid relation schema | Versioned edge and support links |
| Apply prefix batch | All operations structurally valid before commit | Per-operation outcomes and stopped-at index |
| Commit atomic group | Bounded supported operation set, all validations satisfied | All required canonical changes or none |

Atomic groups contain only declared foundation operations; they cannot include model calls, arbitrary user code, or distributed external side effects.
A capability declaration states whether atomic groups are supported and their limits.
Unsupported atomicity must fail before mutation, not fall back to sequential apply.

## Supersession

**WRITE-004 — Guarded supersession.** Supersession MUST use explicit prior ID and expected version. The authoritative guard is inside the transaction. Concurrent attempts against the same open predecessor have at most one committed successor.

The new assertion inherits the lineage root, not the predecessor's identity.
Scope cannot change through supersession.
The default close instant and successor valid start are the same captured instant.
The target baseline rejects a supplied successor start unequal to the close instant; discontinuous corrections require a separately specified temporal-correction operation.
A closed predecessor cannot be reopened or silently superseded again.
An expired/closed record may be read historically but is not an open candidate for supersession.
A discovered semantic conflict without a trusted prior becomes a disputed/uncertainty proposal or no-op, not an invented predecessor.

## Idempotency

**WRITE-005 — Durable replay.** Product writes and canonical foundation mutations MUST accept an idempotency key. Its namespace includes store identity, owning scope, stable authenticated principal/authority domain where applicable (not a rotating credential or grant token), operation family, and key. Successful replay returns the original committed receipt rather than performing another mutation.

The receipt stores a canonical request digest and contract version.
The digest covers semantic caller input, including expected versions and policy identity where relevant; server-generated timestamps and IDs are represented by stable default markers before commit.
The same key with a different request digest returns `idempotency_conflict` before provider calls or writes.
Concurrent matching requests have one committed effect and compatible receipts.
Completed replay should not require a provider to be online.
Current permissions are rechecked before disclosing a receipt or result; permission revocation does not create a new idempotency namespace. The baseline retains receipts or minimal replay tombstones for the store-instance lifetime unless an explicit purge policy removes them. Any bounded-retention mode must use an explicit replay epoch/horizon and reject expired-epoch keys, rather than treat an unrecognized expired key as a new operation.
If required receipt history has expired, the server must say replay protection is unavailable; it must not claim an old operation is safe to replay.
Authorized purge can redact receipt payloads while retaining a minimal tombstone where policy permits. A purged object must not be accidentally recreated by replay.

## Batch modes

**WRITE-006 — Honest prefix mode.** Prefix apply MUST validate all structurally checkable operations before committing any. On the first runtime failure it stops, returns committed-prefix receipts, reports the failing operation, and marks remaining operations not attempted.

A prefix result is not atomic even if all operations happened to succeed.
Each operation has a stable operation identity; retry resumes or replays by identity, not by blindly re-extracting and applying the whole episode.
The overall result reports `partial` if some mutations committed and another failed.

**WRITE-007 — No fictitious rollback.** Compensating recovery MUST NOT be named or represented as an atomic transaction. It cannot silently soft-forget a successor while leaving its predecessor closed and call the batch restored. Compensation is an explicit, separately journaled operation with its own authorization and outcomes.

Target product workflows use prefix mode with honest reporting or a supported atomic group.
The existing `apply_ops_atomic` helper requires deprecation/replacement; it does not establish the target atomic-group contract.
A failed checkpoint after successfully committed memory is a separate checkpoint failure, not a reason to repeat committed writes.

## Concurrency and cancellation

**WRITE-008 — Authoritative compare-and-swap.** Version guards MUST be checked in the mutation transaction/update, not only in a prior read. Conflict errors preserve expected and observed versions when disclosure is authorized.

**WRITE-009 — Cancellation semantics.** Cancellation accepted before transaction submission prevents that transaction from starting. Once commit is in flight, the result may be committed or indeterminate until reconciled; cancellation is not a rollback guarantee. After commit, cancellation cannot undo the receipt. A caller that loses the response queries/replays the receipt. Retried transient storage conflicts must remain bounded and preserve the same operation identity.

Metadata updates that do not affect content or applicability may use a separate telemetry stream instead of bumping canonical version.
They must not overwrite canonical attributes through an unguarded read-modify-write.
Workers use both job fencing and expected record versions; a job lease alone does not authorize overwriting new memory.

## Canonical error contract

Target errors are typed in Rust and have stable serialized codes.

| Code | Meaning | Retry posture |
| --- | --- | --- |
| `invalid_request` | Invalid shape, interval, category, or incompatible fields | Correct input |
| `limit_exceeded` | Input or declared work limit exceeded | Reduce or explicitly increase permitted budget |
| `scope_denied` / `permission_denied` | Scope or grant restriction | Do not retry with broader access automatically |
| `not_found` | Authorized lookup has no target | Reconcile input |
| `conflict` | Expected revision no longer current | Re-read and decide; no blind overwrite |
| `idempotency_conflict` | Key reused for different input | Use correct original input or a new intentional operation |
| `embedding_mismatch` | Wrong model/index identity, dimension, or vector shape | Repair configuration/reindex explicitly |
| `unsupported` | Requested capability/contract unavailable | Negotiate supported behavior |
| `provider_unavailable` | External model/embedding failure | Bounded retry only when safe |
| `deadline_exceeded` / `cancelled` | Work stopped or expired | Inspect commit state before retry |
| `store_unavailable` | Storage operation could not complete | Receipt reconciliation and bounded retry |
| `integrity_error` | Corrupt bytes, invalid stored shape, broken required relation | Fail closed and investigate |
| `incompatible_store` | Schema/index compatibility failure | Explicit upgrade or supported binary |

An error includes operation/request identity, stage, safe message, retry classification, and commit state where relevant.
Internal SQL, credentials, raw model responses, and arbitrary provider error bodies are not public error messages.
A batch contains operation-level errors plus an overall status.

## Journaling

**WRITE-010 — Mutation history accuracy.** Required belief events MUST describe committed effects, not attempted effects. Failed hard deletion must not produce a successful erase event. The journal stores exact affected revisions and the operation receipt identity.

Prepare/attempt diagnostics may be recorded separately and clearly labeled.
Optional search/use traces must not be confused with canonical belief history.
Derived-view invalidation caused by a revision is committed or durably queued with that revision; a lost best-effort callback is not sufficient.

## Example failure sequences

- A put commits, its response is lost, and the caller retries with the same key: return the same receipt and ID.
- Prefix operation 0 commits, operation 1 conflicts: operation 0 remains, operation 2 is not attempted, overall status is partial.
- An atomic group creates a claim and required relation but relation validation fails: neither canonical effect commits.
- A worker extracts against revision 2 while a user creates revision 3: the worker receives a conflict and must replan rather than overwrite revision 3.
- A soft-forgotten memory is queried by exact historical revision: it remains available unless explicitly purged.

## Acceptance

See the `WRITE` acceptance family in [13](13-verification-and-evaluation.md), including fault injection at commit/receipt boundaries.
