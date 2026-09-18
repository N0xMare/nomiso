# Durable background processing

Status: the durable job journal (records, dedup, fencing, checkpoints, revision-bound completion, bounded retries, cancellation, inspection) is implemented at the foundation layer — schema `012_job_journal.surql` and `MemoryStore`'s job ops. A library-level worker runtime consumes it: `nomiso_service::worker` (`Worker`, `JobExecutor`, `JobRun`, `WorkerConfig`) provides an in-process claim loop with per-kind executor registration, durable checkpoints, scope/kind-restricted claims, graceful stop, and the built-in `reindex` executor (staged embedding reindex → activate). `vegapunk worker-run` is the process adapter; Unix signal handling remains the adapter's job, not the library's. Job ops are exposed on CLI, HTTP (`/v1/jobs/*`), and MCP (`nomiso_*`/`vegapunk_job_*`) for operators and remote workers. Required job acceptance is atomic with the canonical write: `put_with_jobs`/`supersede_with_jobs` commit the write and its declared `JobIntent`s in one transaction (scope inherited, `self_input` pins the created record at revision 1, live identical intents dedup, invalid intents roll the write back, keyed replay carries intent identity) — the JOB-001 "commit together" path, no separate outbox store. Plane HTTP exposes `/v1/put_with_jobs` and `/v1/supersede_with_jobs`; plane MCP adds `nomiso_put_with_jobs`/`nomiso_supersede_with_jobs`.
The reusable execution machinery belongs in Nomiso's memory toolkit.
Vegapunk selects which jobs run and their default budgets.

## Scope

Initial jobs may derive summaries, propose procedures, build supported relationships, reprocess stale views, or rebuild derived indexes.
A worker is not a general shell runner or another unrestricted task agent.
Foreground use remains supported without workers.

## Job contract

| Field | Meaning |
| --- | --- |
| Job ID | Stable identity assigned at enqueue |
| Scope/access | Owning partition and restricted grant for execution |
| Kind/schema version | Registered workflow, not executable arbitrary text |
| Input references | Exact evidence revisions or a captured source frontier |
| Composition identity | Producer/policy/provider configuration for this job |
| Deduplication key | Kind, scoped input identity/frontier, and composition identity |
| State | Pending, leased, succeeded, failed, cancelled, or superseded |
| Attempt and lease | Attempt number, fencing token, owner, expiry |
| Checkpoint | Bounded durable progress and external request/operation identities |
| Budget | Maximum attempts, runtime, provider work, input/output sizes |
| Result | Mutation receipts, readiness information, and safe error details |

Job records are toolkit-owned representations persisted through foundation operations.
Do not introduce a separate database or let workers write around Nomiso's commit boundary.

## Acknowledgment and enqueue

**JOB-001 — Durable acceptance.** Reporting an event as accepted for background processing MUST mean its permitted input reference and required job intent are durable. If the job is necessary to fulfill a promise made by a canonical write, enqueue intent and that promise must commit together or use a recoverable outbox contract.

An acknowledgment identifies what is durable now and what is pending.
Optional enrichment does not delay an otherwise complete required foreground mutation unless the composition explicitly requests it.
Queue failure must not be hidden behind a successful all-work-complete response.

## State transitions

| From | Event | To |
| --- | --- | --- |
| Pending | Authorized lease acquisition | Leased |
| Leased | Checkpoint with valid fence | Leased |
| Leased | Commit final effects and completion | Succeeded |
| Leased | Retryable failure within budget | Pending, with attempt history |
| Leased | Terminal failure or exhausted budget | Failed |
| Pending/leased | Authorized cancellation | Cancelled, with commit-state reconciliation |
| Pending/leased | Inputs replaced by a newer desired job | Superseded, with replacement reference |
| Leased | Lease expires | Eligible for reacquisition with a new fence |

**JOB-002 — At-least-once execution, idempotent effects.** Workers MUST assume execution may repeat. Stable per-effect operation identities and durable receipts prevent duplicate canonical mutations. Do not claim exactly-once external execution.

**JOB-003 — Lease fencing.** Lease acquisition and renewal MUST be compare-and-swap operations. Every canonical effect, checkpoint, and finalization checks the current active job state and fencing token in the same transaction as that effect. A stale or cancelled lease cannot commit new effects even if the old worker resumes after a pause. Only the current lease holder may advance job state.

External providers may receive duplicate requests after ambiguous failures unless they support idempotency.
Their repeated cost is measured separately from duplicate canonical effects.

## Input consistency

**JOB-004 — Revision-bound results.** A worker MUST validate that its input/prior revisions still satisfy the proposal at commit. A valid lease does not authorize overwriting foreground changes. Stale results are rejected, reviewed, or replanned as a new input/composition identity.

A summary of frontier A remains a summary of A, even if newer source material exists.
A job may publish that bounded view if the capability allows it, but it cannot call it a summary of the current corpus.
A purged input cannot be reintroduced from a cached job payload.
Permission restrictions are checked again before publishing effects; revoked access cancels or rejects the work.

## Checkpointing

A checkpoint records the completed input range/reference set, proposed outputs or artifact references, and operation identities already committed.
It is not a raw model conversation dump.
Checkpoint size and retention are bounded.
Partial provider output is not automatically a valid extraction result.
A restart resumes from a verified checkpoint or recomputes with the same effect identities.
Persist a validated proposal and its stable per-effect identities before applying it. On recovery, replay that proposal/receipts rather than rerun a nondeterministic extractor and silently assign new IDs to changed output. Re-extraction that changes semantic input/output is a new explicitly planned attempt, not an idempotent replay.

**JOB-005 — Bounded retries and resources.** Every job MUST have finite retry, deadline, provider-call, payload, and graph-expansion limits. Backoff distinguishes transient provider/storage errors from invalid input and permission failures. Retries cannot silently exceed the original resource grant.

Concurrency is configured per store/provider/workload and must not starve required foreground operations.
No numeric default is declared optimal by this specification; a supported composition publishes measured defaults.

## Cancellation and shutdown

**JOB-006 — Cancellation truthfulness.** Cancellation MUST stop new work and reconcile any in-flight committed effects. A cancelled job may have committed checkpoints/effects; the response identifies them. Cancellation is not rollback.

Graceful shutdown stops acquiring jobs, propagates cancellation/deadlines, and persists useful checkpoints.
Lease expiry allows another worker to continue after process loss.
The host controls worker lifecycle explicitly; constructing a library handle never starts workers automatically.
Unix signal handling belongs in a process adapter, while cancellation/checkpoint semantics belong in the library.

## Observability and operator controls

**JOB-007 — Inspectable lifecycle.** Operators MUST be able to inspect pending work, attempts, input/composition identities, readiness, terminal errors, and committed receipts without reading raw sensitive payloads by default.

Retrying terminal work requires an explicit operation that preserves or intentionally changes its job/effect identities.
Administrative cancellation and retry are not exposed as unrestricted model tools.
A failed enrichment is visible on readiness diagnostics; it must not disappear merely because ordinary lexical recall still works.

## Background-work safety examples

- Process dies after committing a procedure but before recording job success: receipt replay prevents a duplicate procedure.
- Old lease holder resumes after another worker took over: stale fence rejects completion.
- Foreground user supersedes a referenced claim: worker cannot close or replace the new claim using its old expected version.
- Source evidence is purged: a queued summary must not republish the erased content.
- Provider is unavailable: job records a retryable failure and consumes only its allowed attempts.

## Acceptance

See `JOB` in [13](13-verification-and-evaluation.md).
Worker support is not complete until kill/restart, fence-race, cancellation, and privacy-invalidation tests pass using isolated stores.
