# Interfaces, configuration, and integration

Status: target contracts plus an explicitly labeled current operational recipe.
The recipe below does not imply implementation of the future controller API.

## Semantic API first

**API-001 — Shared library behavior.** Rust, CLI, HTTP, MCP, and SDK calls for the same operation MUST reach the same domain workflow. Adapters handle transport/configuration/presentation, not independent retention, ranking, mutation, or context-selection policies.

Target requests identify contract version, operation/request identity, owning scope/access, operation-specific input, and applicable budgets.
Mutating requests carry idempotency identity and expected revisions as required by document 04.
Responses identify status, typed data, operation receipts, safe errors, warnings/degradation, effective composition, and correlation.

The existing `nomiso.op.v1` name denotes an evolving legacy operation family, not a frozen public ABI.
New envelopes and controller operations require explicit contract versioning and negotiation; do not silently wrap old response shapes on existing endpoints.
Rust semantic types are authoritative; generated transport schemas must be checked against them.
Type names in specification tables are design contracts, not claims that current bindings export those names.

## Rust interfaces

Foundation clients expose records, mutations, evidence, relationships, queries, and recovery/admin capabilities through appropriate restricted interfaces.
Toolkit clients expose composition, processing, context proposals, and worker lifecycle.
Vegapunk exposes the supported default workflow facade.

**API-002 — Usable library lifecycle.** Cancellation, deadlines, provider identity, permission restrictions, and optional worker startup MUST be available in Rust, not only as CLI flags. Constructors validate configuration; side-effecting connection and worker start remain explicit.

Minimal dependency features must not activate a model provider, remote evaluator, or server.
Examples must compile against packaged crates before an installable API is advertised.

## Configuration

**API-003 — Deterministic resolution.** Configuration MUST have documented precedence: explicit caller/CLI values, environment overrides, selected file, then built-in defaults. An explicitly requested nonexistent or invalid file is an error, not a signal to select another store.
Target file discovery is explicit config selection first (CLI path or `VEGAPUNK_CONFIG`), then `./vegapunk.toml`, then the supported user config location, then built-ins. A selected file's parse/permission failure does not fall through to another file. CLI/environment overrides carry provenance in resolved diagnostics. Environment-supplied relative paths resolve against invocation cwd; config-supplied relative paths resolve against the config directory.

For target file-based configuration, relative storage/blob paths resolve against the configuration file's directory.
Explicit relative command-line paths resolve against invocation cwd and are displayed as resolved paths in diagnostics.
Library builders use explicit supplied paths; they do not search the host filesystem for configuration.
Any change from current cwd-relative behavior requires migration/warning rules in document 12.

The target configuration includes endpoint, namespace/database, secure credential references, profile/composition identity, scope defaults, blob root, embedding identity/index generation, budgets, and worker settings.
Credentials are read through explicit environment/provider references, never printed in resolved configuration.
Scope defaults fill omitted input; they cannot expand the effective access grant.

**API-004 — Provider-independent administration.** Status, working-state reads, metadata reads, receipt lookup, and suitable offline administration MUST NOT require embedding/chat credentials merely because a provider is configured.

A doctor/status result separates store health, compatibility, provider readiness, capability availability, and pending derived work.
Successful store health is not proof that semantic recall or background processing is ready.

## CLI contract

**API-005 — Machine-safe CLI.** Machine mode MUST emit a stable JSON payload on stdout, diagnostics on stderr, and a nonzero exit for rejected, partial, indeterminate, or failed operations. Structured partial receipts remain available even with a nonzero exit.

Successful empty/no-op outcomes are explicit and may exit zero.
Argument errors exit nonzero without opening or mutating storage where avoidable.
Target exit classes are success `0`, operation failure/partial `1`, and usage/configuration validation error `2`; compatibility changes are documented before release.
TOON/human-friendly output may remain optional, but JSON is the canonical SDK/automation contract.
Formatting choices must not change semantic fields or hide failure state.

Admin operations require explicit flags/authorization and are not suggested as automatic recovery for an ordinary agent failure.
No interactive credential prompt is required for normal automated operation.

## HTTP contract

**API-006 — Typed HTTP errors.** HTTP MUST map validation to 400, unauthorized authentication to 401, denied authorized access to 403, authorized missing targets to 404, version/idempotency conflicts to 409, oversized payloads to 413, and provider/store/deadline failures to appropriate 5xx responses. Responses retain stable domain codes and safe commit-state information.

Authentication and body/resource limits must be enforced before expensive processing.
A partial batch is not indistinguishable from complete success; the versioned API must expose partial status and failure HTTP semantics without discarding receipts.
Target failure mapping preserves the first failed operation's class for prefix batches (for example 409 for conflict), with overall `partial` in the body and all prior receipts. Upstream malformed responses map to 502, provider/store unavailability to 503, dependency deadline to 504, and internal invariant failures to 500. No adapter or SDK blindly retries the entire batch based on a 5xx status.
The baseline supports local single-principal serving; no multi-tenant authorization claim follows from one bearer key.
The service hosts library workflows, not a general model-completion endpoint.

## MCP contract

**API-007 — Protocol correctness.** MCP adapters MUST use proper tool-error results for operation failures, valid generated input schemas, and protocol-safe stdout. Initialization, tool discovery, calls, errors, and shutdown require real transport tests.

Product and foundation catalogs remain distinguishable.
Offer a small default agent-facing tool group and optional deeper/admin groups; the full library need not be injected as tool definitions on every turn.
Tool metadata must accurately describe read-only versus mutating behavior.
No raw SQL tool is required for memory use.

## SDKs and harness integration

**API-008 — Thin clients.** Python/TypeScript SDKs MUST reuse the versioned server contract and typed errors. They cannot implement independent conflict resolution, retention, or ranking. Automatic retries are limited to safe receipt/idempotency semantics.

**API-009 — Mechanical host bookkeeping.** A reference harness integration MUST demonstrate event submission, task-state handling, context proposal selection, actual-insertion acknowledgment, and observed outcomes. It must not require the main model to invent correlation IDs or claim an insertion it cannot observe.

SDK context helpers may return/render selected blocks, but actual placement remains host-controlled.
A long-lived service should own a shared embedded store; short CLI processes must not contend with that owner for the same Rocks files.

## Current surface inventory

Current means audited through the T4/T6 milestone, not target parity.

| Capability | Rust | Vegapunk CLI/MCP | Product HTTP | Plane MCP/HTTP |
| --- | --- | --- | --- | --- |
| Put/remember, supersede, search/recall | Present | Present | Present | Present |
| Hard recall and pack | Present | Present | Present | Not a foundation operation |
| Writer-op apply | Present | Present | Present | Raw foundation mutations instead |
| Prepare-context proposal + manifest | Present | `prepare-context` | `POST /v1/prepare_context` | Not a foundation operation |
| Verified insertion acknowledgment | Present | `record-insertion` | `POST /v1/record_insertion` | Not a foundation operation |
| List/count | Present | Present, product cursor gap | Absent | MCP present, HTTP absent |
| Working state | Present | Present | `POST /v1/working_state` | MCP present, HTTP absent |
| Belief events | Present | CLI present | Absent | MCP present, HTTP absent |
| Trace inventory/injection/outcomes | Present | Present | `POST /v1/trace_outcome` (attributed) | Primarily library/product tools |
| Candidates, artifacts, compaction | Present | Present | Absent | Primarily library/product tools |
| Sleep dry-run/apply | Present | Present, unsafe near-dup heuristic | Dry-run only | Not a foundation operation |
| Generic controller/worker/graph retrieval | Primitives + opt-in `graph_expand` eval | Opt-in effort plans | Primitives only | Target primitives only |

Current product MCP has 42 tools; plane MCP has 35.
Current plane HTTP exposes put, supersede, search, read, and forget plus health.
Current product HTTP exposes remember, supersede, apply_ops, recall, hard_recall, checkpoint, sleep, prepare_context, record_insertion, trace_outcome, and working_state plus health.
The current binaries do not ship a complete OpenAPI surface simply because OpenAPI dependencies appear in workspace configuration.

## Current durable recipe

This recipe uses the current binary and existing flags. It is intended for a new, isolated working directory, not to overwrite this checkout's configured store.
Build from the checkout with `cargo build -p vegapunk-cli`, then invoke the resulting absolute binary path from the chosen directory.
The default build includes embedded Rocks support.

Set up a new config with `vegapunk init` without `--force`.
The generated example uses durable Rocks, dim 32, and a default local scope; hashing is the offline lexical baseline.
Check `vegapunk --no-help --format json status` and confirm the endpoint is durable before storing real work.

Before using this recipe, review inherited `VEGAPUNK_*` overrides; an endpoint/scope/provider override takes precedence over the new file. Do not execute the commands until status confirms the intended isolated endpoint and scope. The following current commands assume the binary is on PATH and that intended configuration has been selected:

```bash
vegapunk --no-help --format json encode --text "Use the project test command before proposing a change."
vegapunk --no-help --format json hard-recall --query "project test command" --pack
vegapunk --no-help --format json candidates --query "project test command"
vegapunk --no-help --format json working-state
```

After actual insertion, pass the returned top-level `trace_id` and selected memory IDs to `trace-inject`, then report an observed outcome with `trace-outcome`.
Before supersede, use a real candidate ID and version.
Use `apply-ops` for host-extracted WriterOp arrays; inspect every outcome because current partial batches can still exit zero.
Use `ingest-compaction` only for already-extracted durable lines, not raw discarded chat.
Do not use current `sleep --apply` unattended until the contradiction regression is fixed.

Current embedding modes:

| Label | Configuration |
| --- | --- |
| Hashing | No `embed_url`, default hashing enabled; lexical proxy |
| HTTP | `embed_url`, model, matching dimension, explicitly provisioned API-key environment variable |
| None | No `embed_url` and `--no-hash-embed`; lexical-only search |

Current HTTP provider configuration still requires a key variable even for a local compatible server.
The repo's configured memory commands fail when that variable is absent; do not solve this by silently switching the existing store's embedding mode.
Never change a store's dimension/model merely to get a command to run.

For shared use, choose a tested remote Surreal endpoint or let one service own the embedded store.
Current Vegapunk config lacks some foundation namespace/database/auth controls; this is a transition gap, not a reason to deploy an unauthenticated remote database.

For isolated verification use existing `just smoke-agent-durable` and `just smoke-coding-loop` recipes; see document 13.
For actual backups, follow document 11's limitations and stop writers; no destructive restore command is part of this quickstart.

## Acceptance

See `API` in [13](13-verification-and-evaluation.md), including packaged execution, parser behavior, real MCP calls, and adapter parity.
