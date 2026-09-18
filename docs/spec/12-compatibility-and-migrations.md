# Compatibility, migrations, indexes, and packaging

Status: normative transition and release requirements.
Current schema marker is `0.3.7`; package versions at the audited baseline are `0.1.0`.
These identify different things and must not be conflated.

## Compatibility dimensions

| Dimension | Identifies |
| --- | --- |
| Rust/package version | Source/API and dependency release |
| Transport contract version | Request/response semantics and serialization |
| Representation schema | Structured claim/procedure/event/job payload |
| Store schema version | Tables, fields, indexes, and migration state |
| Canonical encoding version | Digest/idempotency/revision encoding |
| Embedding identity | Provider/model revision, dimension, normalization, generation |
| Composition identity | Selected policies/components and effective configuration |
| Backup format version | Archive/manifest and restore requirements |

**MIG-001 — Check before mutation.** Opening an existing store MUST validate compatibility before applying DDL or updating version markers. A newer/unsupported schema is rejected or opened in explicitly supported read-only mode; never silently relabeled as older.

**MIG-002 — Migration ledger.** Applied migrations MUST have stable identities and checksums, completion state, and relevant pre/postconditions. Concurrent migrators must coordinate. Interrupted non-transactional work must be resumable or explicitly recoverable without falsely advancing the schema marker.

The packaged schema is the executable input to migration.
`crates/nomiso-schema/schema/` is the single source of truth for migrations (compile-time embedded, packaged with the crate).
Do not rely on swallowing errors containing phrases such as already-exists to establish compatibility.

## Existing data

**MIG-003 — Preserve legacy identity and uncertainty.** Migration MUST retain existing assertion IDs, content, owning scopes, and supersession history. Missing evidence, historical versions, or embedding identities remain explicitly unknown. Do not synthesize complete revision history from a current row.

A migration may capture the current legacy state as the first fully reconstructible target revision.
Earlier unavailable snapshots are marked legacy-unavailable, not reconstructed by guessing.
Lineage roots can be derived only from verified same-scope supersession chains; ambiguous/broken chains are reported for repair.
Legacy `entity_links` strings are not automatically trusted typed edges.
Legacy categories do not imply epistemic verification.

Migration planning must include counts/integrity checks appropriate to the intended transformation and a rollback/recovery strategy.
No automatic destructive repair of the real store is permitted merely to satisfy the new schema.
Representative fixture stores are used before touching operator data.

## Embeddings and derived indexes

**MIG-004 — Model identity, not just dimension.** A target vector index MUST record its embedding family/model revision, dimension, normalization/encoding, and generation. Same-dimension model changes cannot silently mix incompatible vectors.

An unknown legacy model requires explicit operator attestation or a new re-embedding generation.
An endpoint URL alone is not a stable model revision.
If a provider cannot identify immutable model weights, record the available version and the limitation; do not promise exact cross-run equivalence.

**MIG-005 — Staged reindex.** Re-embedding/reindexing MUST build a new generation from a declared source frontier, validate it, and activate it explicitly. Queries and jobs identify the generation they used. Incomplete generations are not silently treated as complete.

Canonical evidence is independent of derived vectors, summaries, and graph projections.
Old generations may remain for rollback until explicit retention cleanup.
Changing provider configuration is not authorization to rewrite all memories or spend an unbounded inference budget.

## API evolution

**MIG-006 — Versioned semantic changes.** Changes to error status, batch atomicity, time defaults, path resolution, cursor format, output envelopes, or idempotency semantics MUST be explicit compatibility changes. Existing endpoints cannot silently reinterpret old requests as new contracts.

The legacy `nomiso.op.v1` surface is evolving and has not passed a stable-public-ABI gate.
Before public stabilization, correct misleading semantics rather than preserving them indefinitely.
Where practical, keep deprecated Rust wrappers and explicit legacy transport modes for a documented transition period.
Unsupported features return typed errors; they do not quietly emulate weaker semantics.
Unknown fields follow the chosen version's schema rules: reject mutation fields the implementation cannot understand; permit declared extension objects only within their limits.

## T1 pre-release compatibility changes

The in-progress T1 implementation is not a frozen stable-ABI release. The following changes need explicit adoption by callers:

- Product working-state writes without an expected version are create-only. Updating an existing slot requires its current version. The foundation request adds `create_only`; legacy foundation upsert remains available when false.
- List cursors now carry query and effective-time context. Old two-field cursors are rejected; callers restart enumeration rather than adapting a cursor to different filters.
- Prefix batches preserve committed writes and include explicit `not_attempted` outcomes. The deprecated `apply_ops_atomic` symbol rejects before effects instead of performing compensating deletion.
- CLI partial failures exit nonzero while preserving their JSON report. MCP operation failures use `isError`; HTTP distinguishes domain error classes.
- Pack cards may be `reference_only`, with `original_bytes` describing omitted content. Such a card is not the missing fact; use the full-record read surface before relying on it.
- Explicit empty embedding vectors are rejected rather than treated as a request for automatic embedding.
- Rust consumers with exhaustive error matches, custom store implementations, or literal construction of changed public structs must adapt to the new types and `MemoryStore::limits()` contract.

The current implementation register records the remaining review fixes and final-gate status. This list is not evidence that keyed-put receipts, generic revision snapshots, or the complete migration ledger have been implemented.

## Configuration migration

The target resolves file-relative paths relative to the selected config file.
Current Vegapunk resolves relevant relative paths from the process cwd.
A migration must display old and new resolved locations and require an explicit choice when they differ; it cannot quietly create a second memory bank.
Credential references are migrated as references, never copied into plaintext config.
An explicit missing config file is an error under the target, not a fallback path.

## Rust-first packaging

**PKG-001 — Publishable dependency graph.** Internal published dependencies MUST include compatible version requirements, correct feature propagation, and package metadata. A path-only workspace build is not evidence that crates can be packaged or installed.

**PKG-002 — Runtime-independent artifacts.** Installed libraries/binaries MUST NOT require compile-time source-tree paths for runtime fixtures, skills, templates, or schemas. Package required resources or accept explicit runtime paths. Evaluation-only resources belong behind maintainer features/tools, not in the minimal runtime closure.

**PKG-003 — Tested feature closure.** Verify minimal foundation, toolkit baseline, Vegapunk baseline, optional transports/providers, and supported engines independently. A successful all-workspace build does not prove no-default-features or individual-crate usability.

**PKG-004 — Honest release matrix.** Publish supported OS/toolchain/engine combinations and test the advertised minimum Rust version. Release archives/checksums and clean-room installation must be verified before claiming polished distribution.

Current technology choices to preserve unless separately justified: Rust edition 2021, Tokio, jiff timestamps, UUID v7 identities, Axum for HTTP, official rmcp for MCP, Apache-2.0 licensing.
The current lockfile uses SurrealDB 3.2.4 and rmcp 3.1.1; this is a baseline observation, not permanent compatibility with all future releases.
Do not relax security controls, use unbounded latest-version resolution, or bypass hooks to make packaging pass.

## Migration rollout sequence

1. Inventory actual schema/index/config state without exposing secrets.
2. Verify a recoverable backup and determine whether writers must stop.
3. Produce an explicit migration plan with required compatibility versions.
4. Validate the plan on representative isolated old stores.
5. Acquire migration ownership and apply resumable/transactional steps.
6. Verify canonical identities, scope boundaries, revision/evidence links, and readiness.
7. Activate compatible indexes/configuration explicitly.
8. Retain rollback material according to policy and report any unavailable history.

## Acceptance

See `MIG` and `PKG` in [13](13-verification-and-evaluation.md), including older/newer store refusal, migration interruption, same-dimension model drift, packaged execution, and minimum-toolchain checks.
