# Security, privacy, providers, and operations

Status: normative target. This specification is not a security certification.

## Trust boundaries

| Boundary | Required treatment |
| --- | --- |
| Harness/operator → library | Explicit granted operations/scopes/resources; local embedding may use a documented trusted-owner grant |
| Transport → library | Authenticate caller, derive restrictions, then call the same library validation |
| Evidence → model | Untrusted attributed data, not policy or execution authority |
| Model → memory | Typed proposals subject to admission and validation |
| Memory → context | Revision-bound evidence under host-controlled insertion |
| Worker → canonical state | Leased/fenced execution plus ordinary authorization and revision guards |
| Blob locator → filesystem/network | Restricted backend resolution, not arbitrary path/URL access |

## Scope and permission

**SEC-001 — Scope is not authentication.** Scope paths MUST NOT be treated as credentials or tenant authorization. Exact matching is the default filter; prefix inclusion is explicit. Remote callers receive a grant that independently restricts allowed scopes and operations.

Target scope grammar preserves the current convention: slash-separated nonempty segments using ASCII letters, digits, underscore, hyphen, or dot; no leading/trailing slash or embedded whitespace.
Trim outer whitespace once using the scope parser, then use that normalized value consistently in every query and write. Internal whitespace remains invalid. Do not validate one normalized string and bind a different raw string.
The current 512-byte scope bound remains a compatibility limit until explicitly versioned.
A scope string is never a filesystem path.

**SEC-002 — Enforcement below adapters.** Reads, queries, graph paths, evidence references, task state, receipts, and writes MUST enforce supplied restrictions in library code. A cross-scope relationship or reference cannot be used to bypass the grant.

Cross-scope access denial must not reveal unnecessary foreign content, identifiers, or counts.
Session and turn identifiers are correlation only.
In-process trusted ownership must be explicit in the builder; it is not a remotely selectable flag.

## Serving

**SEC-003 — Safe exposure.** The baseline service MUST bind to loopback by default. Non-loopback serving requires authentication or an explicit, conspicuous insecure-development override. TLS/reverse-proxy trust and forwarded identity are configured explicitly.

Bearer secrets are compared safely and never returned through status or debug formatting.
Authentication, input limits, and concurrency limits precede expensive parsing/provider work where practical.
A single API key authorizes one configured principal; it is not a full multi-tenant control plane.

## Sensitive content and memory poisoning

**SEC-004 — Admission restrictions.** Secrets and prohibited source data MUST be excluded before durable retention and external inference. Best-effort pattern detection is a defense-in-depth measure, not proof that data is safe. Hosts must be able to disable evidence retention and remote providers entirely.

**SEC-005 — No authority laundering.** Retrieved content, source instructions, model output, and inferred relationships MUST NOT change permissions, prompt authority, tool configuration, or retention rules. Endorsement of a procedure does not grant permission to perform its actions.

A memory writer should receive only the source material and capabilities needed for the extraction task.
Vendor CLI adapters are maintainer tools with separate risk/permission boundaries, not default memory providers.
Generic memory inference must not silently enable unrestricted agent tools.

## Resource controls

**SEC-006 — Bounded processing.** Enforce input bytes, structured payload size/depth, batch operations, vector dimensions, graph visits, evidence excerpt size, trace/job payload size, provider responses, concurrency, and deadlines. Limits apply to Rust callers as well as transports.

Current example limits such as 8 KiB memory text, 4 KiB attrs, or 32 search hits are implementation defaults, not evidence that every workload needs those exact numbers.
A supported profile publishes effective limits and tested behavior at boundaries.
Resource exhaustion must not silently produce a complete-success response.

## Model and embedding providers

**OPS-001 — Explicit provider lifecycle.** Provider implementations MUST support deadlines, cancellation, bounded responses, safe error mapping, and inspectable readiness. Metadata operations do not initialize providers unnecessarily.

Embedding validation checks expected output cardinality, unique/in-range indexes, dimension, finite values, and model/index identity.
Duplicate response indexes are not acceptable simply because the final vector array is nonempty.
No silent downgrade from a configured semantic model to hashing is permitted.
Provider retry behavior distinguishes safe reads/inference from possibly committed mutations and counts every attempt against the budget.
Provider response bodies and model traces are not blindly copied into logs or public errors.

## CAS and artifact access

**OPS-002 — Restricted content addressing.** Filesystem CAS MUST resolve logical content references within the configured root, enforce symlink/path escape restrictions, and verify expected content identity. A raw `file://` locator from untrusted input cannot authorize arbitrary file reads.

Blob bytes are installed durably before metadata references are committed.
Metadata failure may leave an unreferenced blob; orphan cleanup requires a safe retention window and reference analysis, not immediate deletion after any failed request.
Concurrent same-content writes converge on one verified object.
A pre-existing corrupt final object must not be accepted solely because the path exists.
S3-compatible implementations need signed-request contract tests, bounded I/O, and explicit support limits; local filesystem tests do not certify S3 behavior.

## Deletion and retention

**OPS-003 — Honest deletion scope.** Soft forget, current-row deletion, derived-data cleanup, CAS garbage collection, and backup expiry MUST be separate declared effects. A successful row delete does not mean forensic erasure of traces, artifacts, caches, model-provider logs, or backups.

A purge plan identifies affected canonical revisions, relationships, derived views, jobs, receipts, and artifact references.
Shared blobs are deleted only when no permitted retained reference requires them, subject to retention policy.
Queued/cached derived work must not republish erased content.
Retention policy defines whether minimal receipt/audit tombstones may remain.
Administrative purge requires explicit authorization and an inspectable result, including residual backup/provider limitations.

## Backup and restore

**OPS-004 — Consistent backup.** A supported backup MUST capture canonical state, revision/journal state, required job/receipt state, manifests, and referenced CAS data under a declared consistent frontier. An offline filesystem copy requires exclusive store quiescence, not merely a warning to stop writers.

A backup manifest includes format/schema version, store identity/frontier, component/index identities, file/content digests, completeness, and required restore conditions.
Partial or unverifiable backups must not be labeled complete.
Derived indexes may be omitted only if the manifest declares them rebuildable and restore marks their readiness accordingly.

**OPS-005 — Safe restore.** Restore MUST validate archive paths, manifests, compatibility, digests, all destinations, and permissions before replacing any destination. Reject path traversal and symlink escapes. Stage a restore, then activate it with a documented recovery plan; failure must not leave an unreported mixed data/blob generation.

Relocated restores resolve artifact content through the new backend root rather than preserved machine-specific absolute paths.
Do not accept an arbitrary `force` flag as permission to ignore integrity or archive-kind checks.
Actual destructive replacement requires operator confirmation for that destination.

`scripts/nomiso-snapshot.sh` now implements the embedded-engine subset of OPS-004/005: quiescence is enforced by a live-writer flock probe (fail-closed where flock exists), the manifest (`kind: nomiso-snapshot-v2`) carries a declared frontier — snapshot id, file/blob counts and bytes, and per-table row counts captured from `vegapunk store-stats` while the store is quiesced — plus sha256 digests of every archived file. Restore extracts to an adjacent `<dest>.staged.<id>` dir, validates digests + file set + CAS object layout + symlink/path safety *before* any destination is touched, then activates by atomic rename (replaced dirs are moved aside, never deleted). Per-component `FRONTIER.json` markers and `verify` make mixed data/blob generations detectable, and declared table counts are rechecked against the live store post-activation. Remaining honest limits: filesystem copy semantics (not a remote-backend backup story), frontier capture requires the stats command to reach the same store being snapshotted, and CAS object-level content verification runs only when `b3sum` is installed.

## Observability

**OPS-006 — Useful, privacy-aware diagnostics.** Expose operation stage, safe error code, commit state, latency, budget consumption, candidate counts, index readiness, job status, and composition identity. Default logs MUST NOT contain secrets, raw transcripts, unrestricted provider bodies, or full recalled content.

Payload retention for exact replay is separately authorized and bounded.
Where content is unavailable due to retention/deletion, replay reports that limitation rather than fabricating evidence.
Metrics distinguish ingestion, retrieval, foreground model use, background model use, and storage work.

## Operational support envelope

A release identifies supported engines, server versions, OS/toolchain combinations, authentication modes, and tested sizes/concurrency.
Embedded durability and remote query correctness require separate evidence.
Known engine planner fallbacks are diagnostics requiring investigation, not automatically evidence of a failed channel or permission bypass.

## Acceptance

See `SEC` and `OPS` in [13](13-verification-and-evaluation.md): grants, poisoning cases, provider faults, CAS corruption, archive validation, and restart/restore tests.
