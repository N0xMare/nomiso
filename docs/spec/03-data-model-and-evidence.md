# Data model, evidence, identity, and time

Status: normative target. Legacy migration rules are in [12](12-compatibility-and-migrations.md).

## Identity model

Do not conflate assertion identity, optimistic version, lineage, and content identity.

| Identity | Meaning |
| --- | --- |
| `MemoryId` | Opaque identifier of one assertion; superseding content creates a new assertion ID |
| `version` | Monotonic optimistic token for state changes to that assertion, including closure and metadata changes |
| `RevisionRef` | Exact assertion ID plus version and canonical revision digest |
| `lineage_id` | Root assertion ID for a supersession chain; not a semantic deduplication key |
| `ArtifactId` | Scoped metadata record for bytes addressed by content digest |
| `RelationshipId` | Opaque identity for one relationship assertion, with its own version |
| `StoreRevision` | Opaque snapshot/commit-frontier token from one store instance; not a global timestamp |

**DATA-001 — Identity separation.** Content similarity MUST NOT determine assertion identity or authorize supersession. A new claim requires a new ID unless it is an idempotent replay of the same committed operation.

**DATA-002 — Revision resolution.** A target `RevisionRef` MUST resolve to the exact canonical state that was referenced, or explicitly report unavailable/erased/legacy-unavailable. Resolving an old reference to the current version is forbidden.

The target stores immutable revision snapshots or an equivalently reconstructible journal for committed changes.
The existing mutable row can remain a current-state projection.
The storage design is not required to replay arbitrary SQL or expose database MVCC internals.
Target digest encoding is identified as `nomiso.canonical-json.v1`: UTF-8 JSON canonicalized according to RFC 8785, hashed with BLAKE3, represented as lowercase hexadecimal with a separately recorded algorithm/encoding identifier. Canonicalization is a new target contract, not the current serde serialization guarantee.
Canonical payloads reject duplicate object keys, nonfinite numbers, and values outside the canonical encoding's supported numeric domain. Schema fields requiring exact 64-bit integers, including versions and counters, use decimal strings in new versioned wire/digest projections; legacy numeric JSON remains a separately versioned compatibility shape. Arbitrary structured integers needing greater precision must use schema-declared strings.
Normalize semantic defaults according to the request contract before hashing; omitted server-generated values retain explicit default markers for request digests as defined in document 04. Do not normalize text casing, whitespace, or Unicode content as if it were semantically equivalent.
A revision digest covers its canonical assertion state, excluding the digest field itself, physical storage locators, and mutable derived-index/usage telemetry. Artifact content digests cover raw bytes, not canonical JSON. Request, revision, and artifact digests are distinct domains and cannot be substituted.
Pin the encoding version and verify cross-language golden vectors before public use.

## Memory record contract

| Field | Target semantics |
| --- | --- |
| `id`, `version`, `lineage_id` | Identity and concurrency fields above |
| `scope` | Owning normalized scope; immutable within an assertion |
| `category` | Semantic, episodic, identity, procedural, uncertainty, or legacy trace classification |
| `representation` | Claim, observation, procedure, summary, entity description, or namespaced extension |
| `content.text` | Human-readable assertion or view; bounded UTF-8 |
| `content.attrs` | Bounded structured payload with an explicit schema identifier/version for toolkit representations |
| `epistemic_status` | Reported, inferred, verified, disputed, or unknown |
| `support` | Revision-bound evidence references and derivation metadata |
| `valid_from`, `valid_until` | Half-open applicability interval in represented world time |
| `known_at` | When the input was known according to the supplied source; not proof of commit time |
| `sys_created`, `sys_updated`, `sys_closed` | Store-assigned audit clocks |
| `confidence` | Optional finite value in [0,1], with producer identity; not truth or authorization |
| `supersedes`, `superseded_by` | Explicit lineage references |
| `stale` | Derived-state invalidation marker with reason and source revision, not a replacement truth judgment |
| `provenance` | Origin, source/event references, producer/model/policy versions, correlation identifiers |
| `embedding_refs` | Derived representation/index generation references; not authoritative evidence |

The target schema may add fields without changing the legacy JSON names used by compatibility endpoints.
Representation schema and category are separate: a procedural record may be inferred or verified.
The presence of a category must not imply verification or general applicability.

**DATA-003 — Evidence status.** Inferred or reported content MUST NOT become verified merely through persistence, summarization, retrieval, or repetition. Verification requires a recorded check and its scope of applicability.

**DATA-004 — Immutable assertion content.** Content changes use a new assertion and explicit revision relationship. Annotation may change bounded metadata but MUST preserve old revision resolution and cannot forge original evidence or audit time.

Assertion payload includes semantic `content.attrs`; changing procedure steps, applicability, or other meaning-bearing attributes requires a new assertion, not an annotation loophole. Metadata revision may change explicitly allowed support/status fields with recorded justification. Scan/use counters belong to a separate usage stream. New IDs and legacy aliases must not be inferred from content hashes.

**DATA-005 — No invented legacy metadata.** Missing legacy source, model, validity, or verification information MUST remain unknown unless an operator supplies a recorded migration attestation. Absence is not equivalent to a trusted default.

## Evidence and artifacts

An artifact contains bytes; a memory is an interpretation or reusable unit that may cite bytes.
Artifact metadata contains scope, digest algorithm, digest, length, media type, locator, source, and creation information.
CAS locators are logical references resolved by the configured backend, not arbitrary caller-controlled filesystem paths.
A copied or restored store can resolve the same content digest under a new backend root.

A span references an artifact and a half-open range with unit `byte`, `char`, or `line`.
Byte and Unicode-scalar offsets are zero-based; line ranges are zero-based logical lines.
`start <= end` is required, and the range must fit the referenced artifact when validated.
Readers may request a bounded excerpt rather than load the whole artifact.
Normalization of line endings or text encoding requires a new artifact identity or an explicitly versioned interpretation.

**DATA-006 — Evidence integrity.** Retrieved bytes MUST be verified against their recorded digest when integrity is required. Metadata registration cannot make missing or corrupt bytes appear valid. Unsupported media/range interpretations return typed errors.

**DATA-007 — Recoverable derivation.** A derived memory MUST reference supporting artifact spans or exact memory revisions, or explicitly state that evidence is unavailable. Its producer, transformation version, and input references are recorded independently of its human-readable text.

An observation can be useful evidence even when no reusable claim is admitted.
Retention of source evidence is policy-controlled; this specification does not mandate raw transcript archival.
Immutable means not silently rewritten while retained, not exempt from authorized deletion.

## Three clocks and two historical operations

| Clock | Question |
| --- | --- |
| Valid time | For which world/task interval is this assertion applicable? |
| Known time | Had this source information been learned by the requested cutoff? |
| System time | Which recorded version existed at the store cutoff? |

**DATA-008 — Half-open intervals.** Valid intervals use `[valid_from, valid_until)`, with absent end meaning open. Equal endpoints denote an empty interval; reversed endpoints are rejected after defaults are applied. Store time is assigned by the store, not trusted from a model.

**DATA-009 — Distinct history semantics.** Eligibility filtering and historical-state reconstruction MUST be separate documented operations. `known_as_of` filters source knowledge time; it does not by itself reconstruct what the system once believed. Exact historic record state requires revision/snapshot reads.

Legacy search defaults `as_of` to now, even when a known/system lens is supplied.
That behavior must remain explicit on compatibility endpoints.
A target snapshot query specifies both the store frontier and valid-time lens; omitted valid time is resolved once at request start and returned in the result metadata.
A later retroactive correction must not rewrite the result of an earlier captured snapshot.

Logical source revision/commit applicability is separate from timestamps.
A procedure tied to a repository revision or dependency version needs explicit applicability metadata; text mentioning a version is not a reliable index predicate.

## Snapshot consistency

A `StoreRevision` is issued by a named store instance and identifies one coherent visibility frontier. It must not be reconstructed by using wall-clock time as if that proved commit order. Consistent multi-page enumeration pins that frontier for all pages. Exact revision reads remain possible independently of current-query defaults while those revisions are retained.
Historical search must declare whether its index generation can answer at the requested frontier. An implementation unable to provide the requested snapshot/index consistency returns unsupported or explicit bounded/partial status; it must not advertise snapshot-consistent search based only on filtering a current index by timestamps.

## Working state

Working state is a mutable, versioned slot, not an endorsed long-term claim.
It can contain goals, plans, blockers, file references, progress, and pending verification.
Its owning scope and slot name are explicit; task/session isolation must not depend on one global `coding-wm` slot.

**DATA-010 — Working-state separation.** Restoring a state slot MUST NOT silently promote its contents into durable factual memory or insert it into the model context. Promotion and insertion are separate authorized operations.

Target slot writes use explicit create-if-absent or compare-and-swap modes.
Unconditional overwrite is reserved for an explicitly privileged administrative operation, not an accidental omitted version.

## Procedures and summaries

A procedure's structured payload includes:

- Task class and applicability predicates.
- Preconditions and required permissions/resources.
- Actionable steps at an appropriate abstraction level.
- Verification steps and expected observable results.
- Failure cases and conditions for stopping or escalating.
- Supporting successful/failed episode references.
- Inferred versus verified portions.
- Freshness and invalidation conditions.

A summary identifies the exact source set and source frontier it summarizes.
A changed source marks affected derived views stale; it does not automatically prove a replacement summary.
Summaries, graph projections, and embeddings are derived representations and may be rebuilt without altering canonical evidence.

## Extensions

**DATA-011 — Bounded extensibility.** Toolkit-defined representations MUST use versioned schemas, size limits, and reserved-field validation. Arbitrary attributes cannot override scope, identity, time, permission, or mutation semantics.

Start with the listed representations; a universal ontology is not required.
Unknown extension payloads may be preserved as opaque data only under a declared compatibility mode, never executed or promoted as understood instructions.

## Acceptance

The `DATA` family in [13](13-verification-and-evaluation.md) covers exact revision resolution, evidence integrity, temporal semantics, state isolation, and legacy uncertainty.
