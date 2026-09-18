# Current state, gaps, and transition plan

Status: implementation evidence and ordered transition register.
Baseline: `3fcb456` on `main`, audited 2026-09-16 before this specification consolidation.
The normative target in documents 01–13 is not a claim that the baseline implements it.

## T1 implementation progress

Status: implemented and verified in the working tree against the T1 exit gate.
The audited baseline and historical results below remain unchanged historical evidence.

Implemented and tested in bounded slices:

- Near-duplicate proposals are review-only; automatic near-duplicate deletion is disabled.
- Product batches preserve committed prefixes, return `not_attempted` tail outcomes, and expose computed overall status. The deprecated `apply_ops_atomic` rejects before effects.
- Packing selects whole fitting cards and bounded `reference_only` locators. CLI `read --id` and product HTTP `/v1/read` provide full-content follow-up.
- Temporal predicates are applied inside retrieval channels; scope normalization is consistent across foundation operations.
- Cursors carry normalized query/lens context; text enumeration uses keyset ordering; count exhaustion returns an error rather than a partial exact integer.
- Product working-state omission of a version is create-only; CLI/MCP expose expected-version updates.
- Provider input/response validation, deadlines, bounded response bodies, lazy credential resolution, and explicit lexical search are implemented.
- CLI/HTTP/MCP use typed failure paths, including partial reports; a real product MCP stdio contract test is present.
- Keyed product puts store a canonical request identity and original acknowledgement transactionally; replay returns the original id/version, changed input is `idempotency_conflict`, erased or legacy receipts are `idempotency_unavailable`, and replay never invokes the embedding provider.
- Config resolution: explicit relative `VEGAPUNK_CONFIG` resolves against cwd; missing/non-file explicit config fails without fallback; `path_base = "config"` resolves relative paths under the config directory; ambiguous legacy relative paths outside cwd are refused; environment overrides apply before ambiguity checks; error output does not echo sensitive config source.
- Packaging: workspace path dependencies carry explicit versions and inherited repository metadata; coding-agent fixtures are embedded via `include_dir` with a byte-parity test and `scripts/sync-eval-fixtures.py` guard.

Lead review corrections also landed: relative config resolution, symlink-sensitive parent paths, override precedence, safe error formatting, serializer field counts, and bounded test-process shutdown.

T1 acceptance evidence (working tree, Rust/Cargo 1.98.0, Linux):

| Command/check | Result |
| --- | --- |
| `just check` (fixtures-check, fmt, workspace clippy `-D warnings`, plane/surface/product/tact tests) | Passed: 284 tests, 0 failed |
| `just foundation` (`just check` + smoke, smoke-durable, smoke-durable-mp, smoke-agent-durable, smoke-coding-loop) | Passed: durable local release bar |
| `cargo package --workspace --allow-dirty` | All 15 packages archived and compile-verified from packaged tarballs |
| `scripts/sync-eval-fixtures.py --check` | 26 byte-identical packaged fixtures |

Remaining honest limits: package verification used the crates.io index over the network (offline verification is cache-limited); the lockfile was moved off yanked `chacha20 0.10.1` to `0.10.2`; remote SurrealDB deployments, non-Linux platforms, and live provider quality remain unverified here; repo memory recall in this environment is still blocked by the missing embedding credential noted below.
T1 items not claimed complete: see the gap register — T2+ rows remain open, and several T1-labeled rows are improved rather than fully closed (for example MCP conformance beyond the stdio contract test, and deeper provider hardening under SEC-004/SEC-006).

## T2 implementation progress

Status: in progress. Nine bounded slices are implemented and verified in the working tree.

Slice 1 — required journal atomicity (WRITE-003, WRITE-010, part of REL-004):

- Every required belief event (`assert`, `supersede_close`, `supersede_assert`, `soft_forget`, `hard_erase`, `annotate`) now commits inside the same transaction as the canonical mutation effect — for `put`, keyed `put_idempotent`, `supersede`, `forget` (soft and hard), and `annotate`.
- Hard erase previously journaled a successful `hard_erase` before the `DELETE`; a failed delete now leaves no event at all.
- Supersede's stale-dependent edge updates (`derived_from_edge`) now commit in the same transaction as close+assert, instead of a post-commit best-effort call.
- Replay puts journal nothing; journal payloads record the committed revision.
- Verified by six new tests, including `journal_no_erase_event_when_hard_forget_conflicts`, `journal_failed_supersede_journals_nothing`, and `journal_supersede_commits_close_and_assert`.

Slice 2 — migration ledger and newer-schema guard (MIG-001, MIG-002):

- `migrate()` now checks store compatibility before any DDL: a missing or recognized older marker proceeds, a newer or unrecognizable marker is rejected with `Error::IncompatibleStore` (stable code `incompatible_store`), and embedding-dimension checks still run before mutation.
- New `schema/009_schema_ledger.surql` defines `schema_migration`; every embedded migration claims a ledger row keyed by stable name with a blake3 checksum. Matching applied rows are skipped; changed checksums are rejected; `failed` rows are reclaimed; stale `applying` claims are stealable after 120 s; concurrent migrators coordinate via create-claim conflicts with a 30 s bound.
- The `nomiso_meta:schema` marker advances only after all migrations complete; interrupted runs resume from ledger state rather than falsely advancing.
- Schema marker bumped to `0.3.4`; root and crate-embedded schema copies remain byte-identical (checked by `diff -r` and the packaging fixture guard).
- Verified by six new tests, including `migrate_rejects_newer_store_marker`, `migrate_rejects_drifted_migration_checksum`, and `migrate_concurrent_migrators_converge` (which caught and fixed an unrecognized "already exists" claim conflict).

T2 acceptance evidence (working tree, same toolchain as T1): `just check` passes at 346 tests, 0 failed, including all new journal, migration, relationship, embedding-identity, job-journal, worker-runtime, surface, and atomic write+job regressions.

Slice 3 — typed relationship API and bounded traversal (REL-001..004, part of RET-001):

- New `relationship` plane (schema `010_relationships.surql`, marker `0.3.5`): scoped, versioned edges with typed subject/object endpoints, registered predicate + registry version, evidence refs, epistemic status, half-open validity, producer metadata, and explicit `active`/`closed`/`stale`/`purged` state. Append-only `relationship_event` rows preserve the prior revision's state/evidence in the same transaction as every update (REL-002).
- Closed predicate registry with per-predicate endpoint-kind rules: `supports`, `derived_from`, `contradicts` (symmetric), `supersedes`, `mentions`, `depends_on`, `applies_to`, `observed_in`, `attempted`, `resolved_by`. Unknown predicates and ill-typed endpoints are rejected before mutation; `supersedes` is foundation-maintained and rejected on the public path.
- REL-001 enforcement: both endpoints (and every evidence reference) must exist and carry the exact owning scope before any mutation; pinned revisions may only reference existing versions of versioned kinds.
- Idempotent create: a unique `live_key` (scope|predicate|subject|revs|object) makes concurrent identical creates single-winner; identical re-put replays the same edge, divergent payload on a live identity returns `idempotency_conflict`. Non-active edges tombstone `live_key` so history never blocks re-assertion.
- Entities become first-class scoped records (`put_entity`/`get_entity`/`update_entity` with CAS), enabling `mentions`/`depends_on` endpoints. Legacy entities without `scope` fail closed as endpoints.
- `traverse` implements bounded BFS: depth, visited-node, edge, and deadline budgets; per-predicate and direction filters; visited dedup and cycle handling; per-node path provenance (edge-id chain); explicit `truncated` reasons that only mark a budget when real work was cut.
- REL-004 invalidation is atomic: superseding a memory marks touching `derived_from` edges `stale` (`endpoint_superseded`) in the same commit; hard erase marks referencing edges `purged` (`endpoint_erased`) in the same commit. Closed/stale/purged edges are excluded from traversal unless explicitly requested.
- Foundation trait + `NomisoClient` expose `put_entity`/`get_entity`/`update_entity`, `put_relationship`/`get_relationship`/`update_relationship`, `list_relationships`, and `traverse`; the `nomiso` facade re-exports the types.
- Verified by nine new tests covering registry rejection, missing/cross-scope endpoints, dedupe replay vs conflict, CAS + audit, atomic stale/purge hooks, traversal depth/direction/path provenance, cycles, budgets, state filters, and scope checks.

Slice 4 — embedding model/index identity and staged reindexing (MIG-004, MIG-005):

- New `nomiso-core/src/embedding.rs` types: `EmbeddingIdentity` (family, model, dimension, normalization, encoding, optional limitation), `GenerationStatus` (`building`/`active`/`retired`/`failed`), `SourceFrontier` (captured timestamp, generation, expected count), `EmbeddingGeneration`, `EmbeddingState`, `DeclareGenerationRequest`, `StagedEmbedding`. `PutRequest.embedding_identity` lets callers attest raw vectors; `MemoryRecord.embedding_generation` and `SearchHit.embedding_generation` report the generation used.
- Schema `011_embedding_identity.surql` (marker `0.3.6`, both copies byte-identical): `embedding_generation` and `embedding_vector` tables plus `memory.embedding_generation`.
- Bootstrap: a fresh store creates generation 1 (`unknown` identity, or the declared `StoreConfig.embedding_identity`); a store holding vectors with an unknown active generation refuses an unrelated configured identity — same-dimension model changes fail closed with `IncompatibleStore` instead of silently mixing vectors. `attest_embedding_identity` adopts an identity for legacy vectors only when the active generation is still unknown; claimed-identity writes are validated against the active identity and dimension.
- Staged reindex: `declare_embedding_generation` records the new identity plus a captured source frontier and expected memory count; `stage_embeddings` writes per-memory staged vectors under the target generation (dimension-, status-, and memory-existence-checked, idempotent); `activate_embedding_generation` refuses while staged coverage is incomplete, then atomically swaps memory vectors, stamps `embedding_generation`, activates the new generation, and retires the old. Hard erase deletes staged vectors for the erased memory in the same transaction.
- `Embedder::identity()` stamps auto-generated vectors with the producing identity; `HashingEmbedder` reports a precise identity, `HttpEmbedder` reports endpoint-derived identity with an explicit `limitation` (provider URL is not a stable model revision).
- A pre-existing idempotency race surfaced under parallel testing: a losing keyed put could observe the winner's committed memory before its slot read saw the committed receipt and falsely return `IdempotencyUnavailable`; `lookup_put_receipt` now re-reads the slot after observing the memory. A replay-consistency fix moves `embedding_identity` attachment ahead of the receipt lookup so writer and replayer compute the same request identity (caught by the cross-process CLI replay test).
- Verified by ten new tests covering fresh/declared bootstrap, same-dimension model mismatch, legacy attestation + enforcement, write stamping, staged validation, incomplete-activation refusal, atomic swap/retirement, generation metadata on reads and hits, and staged cleanup on hard erase.

Slice 5 — durable generic job journal (JOB-001..007 journal layer):

- New `nomiso-core/src/job.rs` types and `nomiso_job` table (schema `012_job_journal.surql`, marker `0.3.7`): registered `kind`, exact owning scope, pinned typed `inputs` (endpoint kind + id + optional revision), `composition` identity, bounded `payload`, `JobBudget` (`max_attempts`, `lease_ms`, `retry_backoff_ms`, `deadline_ms` → `expires_at`), attempt/fence/lease fields, bounded `history`, checkpoint/result/error, `replaced_by`, and terminal metadata.
- **JOB-001 durable acceptance + dedup:** `enqueue_job` validates shape/inputs and inserts the durable record in one transaction with input existence+scope+revision preflight; a unique `dedup_slot` (blake3 of scope|kind|sorted inputs|canonical composition|hint) makes identical live intents replay (`deduplicated: true`). Terminal states tombstone the slot so a cancelled/failed intent can be re-enqueued as a new job.
- **JOB-003 lease fencing:** `claim_job` scans eligible rows (pending past `not_before`, or leased past expiry) inside the worker's exact scope grant and kind allowlist, then CAS-updates one candidate guarded by the old fence — concurrent claimants are single-winner. `renew_job_lease`, `checkpoint_job`, `complete_job`, and `fail_job` all re-check `state='leased' AND fence AND owner` in the same statement; a stale holder resuming after lease loss gets `Error::LeaseLost` (`lease_lost`, HTTP 409).
- **JOB-004 revision-bound results:** `complete_job` revalidates every pinned input inside the completion transaction — versioned kinds must still be at the pinned revision and every input must still exist in scope; a moved or purged input throws `nomiso_stale_input` → `Error::StaleInput` (`stale_input`, HTTP 409), leaving the job leased for replanning. The journal never resurrects purged content.
- **JOB-005 bounded retries:** retryable failures within `max_attempts` return to `pending` with `not_before = now + backoff * 2^min(attempts,8)`; exhausted budget, non-retryable failure, or a past `expires_at` deadline are terminal `failed` with a reason. Claim scans terminal-fail past-deadline pending jobs rather than leasing them.
- **JOB-006 cancellation truthfulness:** `cancel_job` moves pending/leased to `cancelled` with a reason while preserving any committed checkpoint on the returned record — cancellation is not rollback. `supersede_job` records the replacement reference.
- **JOB-007 inspectability:** `list_jobs` returns `JobSummary` rows (state, attempts, fence, owner, error, `has_checkpoint`/`has_result` flags) with no payload/checkpoint/result bodies; `get_job` returns the full record. Two new typed errors: `LeaseLost`, `StaleInput`.
- Foundation trait + `NomisoClient` expose all ten ops; the `nomiso` facade re-exports the types.
- Verified by thirteen new tests covering durable enqueue + dedup replay, bad-input/missing-input rejection, concurrent single-winner claims, stale-fence rejection on every effect path, lease-expiry reacquisition with a new fence, retry→pending with bounded history, budget exhaustion, non-retryable terminal failure, cancel from both pending and leased (checkpoint preserved), supersede-with-replacement, scope/kind grant enforcement, stale-entity and purged-memory completion rejection, payload/checkpoint bounds, and deadline-expiry marking.

Slice 6 — worker runtime consuming the journal (JOB-002/003/006 execution layer):

- New `nomiso-service/src/worker.rs`: `Worker` is an in-process claim loop over the durable journal — claims eligible jobs inside a restricted scope/kind grant, dispatches each to a registered `JobExecutor` by kind (never arbitrary text), and lets the journal decide retry vs terminal within budget. `JobRun` = `Completed | Failed | Paused | Aborted`; `Paused` returns the job to `pending` promptly (graceful stop preserves durable checkpoints); `Aborted` means the lease was lost mid-execution and no further effect is attempted (JOB-003).
- Host-controlled lifecycle: constructing a `Worker` never starts work — `run()` executes until `stop_handle()` is set or `max_jobs`/`concurrency` bounds are met; Unix signals belong to a process adapter (JOB-006). `WorkerStats` reports claimed/completed/failed/paused/aborted/rejected counters.
- Built-in `ReindexExecutor` (`kind = "reindex"`): payload `{generation, scopes, batch}` — enumerates scoped memories via `list`, re-embeds in bounded batches, `stage_embeddings` per batch, and checkpoints `{scope_index, cursor, staged}` after each batch for crash-safe resume; then `activate_embedding_generation` publishes the generation atomically (MIG-005). All effects are idempotent — a retried attempt re-stages the same (generation, memory) rows and re-activates or reports success, never duplicates (JOB-002).
- `NomisoClient::embedder()` exposes the attached embedder for wiring the executor; `nomiso` facade re-exports the worker types.
- Verified by five new tests: end-to-end reindex (declare → claim → stage in 2-row batches → activate → succeeded; generation 2 active, generation 1 retired, all records stamped `embedding_generation = 2`), retryable failure→retry→success with attempt history, pause→pending with a durable checkpoint observed on resume, scope-grant exclusion (a job in `org/other` is never claimed by an `org/acme`-granted worker), and graceful-stop drain.

Slice 7 — relationship, embedding-admin, and job surfaces (CLI / HTTP / MCP):

- **CLI** (`vegapunk`): `entity-put|get|update`, `rel-put|update|list` (`--endpoint-ref` filter), `traverse`, `embed-state|attest|declare|activate`, `job-enqueue|get|list|claim|renew|checkpoint|complete|fail|cancel|supersede`, and `worker-run` (registers built-in `reindex` when an embedder is attached; `--once`, `--concurrency`, `--poll-ms`, `--max-jobs`). Structured JSON output and typed error codes preserved; `kind:id` endpoint syntax (bare ids default to `memory`).
- **Plane HTTP** (`nomiso-http`): `/v1/entities{,/get,/update}`, `/v1/relationships{,/update,/list}`, `/v1/traverse`, `/v1/embed/{state,attest,declare,stage,activate}`, `/v1/jobs{,/get,/list,/claim,/renew,/checkpoint,/complete,/fail,/cancel,/supersede}` — all Bearer-authed where a key is configured; `lease_lost`/`stale_input` map to 409, `incompatible_store` to 503.
- **Plane MCP** (`nomiso-mcp`): 22 new tools (`nomiso_put_entity` … `nomiso_supersede_job`) taking the same typed request bodies as the store API; tool count 10 → 32.
- **Product HTTP** (`vegapunk serve`): the same route set on the product surface; `domain_error` maps `lease_lost`/`stale_input` → 409.
- **Product MCP** (`vegapunk mcp`): 22 new `vegapunk_*` tools (`vegapunk_entity_put` … `vegapunk_job_supersede`) for agent/operator use — worker primitives are tools on the job journal, not model-callable task agents.
- Verified by new tests: plane HTTP relationship + job roundtrips (incl. summary payload-privacy, 409 on stale fence, foreign-scope claim isolation); plane MCP job lifecycle with typed `lease_lost`; product HTTP job+relationship+embed-state roundtrip; CLI contract covering entity→rel→traverse→embed-state→job lifecycle with typed `lease_lost` and summary privacy. One real bug found and fixed: the `--endpoint` filter flag collided with the global `VEGAPUNK_ENDPOINT` arg id and poisoned store connection — renamed to `--endpoint-ref`.

Slice 8 — atomic canonical write + job intents (JOB-001 commit-together):

- **`JobIntent`** (`nomiso-core/src/job.rs`): a durable job declaration that rides a canonical write — `kind`, typed `inputs`, `self_input` (pins the record created by the write at revision 1), `composition`, bounded `payload`, required `JobBudget`, `dedup_hint`. Scope is inherited from the write, so an intent can never target a different partition. `JobIntent::resolve` produces the `EnqueueJobRequest` committed inside the write transaction.
- **`put_with_jobs` / `supersede_with_jobs`**: validate the canonical write and every intent, resolve self-inputs to the Rust-generated record key, pre-check live dedup holders, then commit memory + belief events + job rows in **one transaction** — an invalid intent (dangling input, out-of-scope reference, bad revision) rolls the write back; a live identical intent dedups without failing the write. Job rows are durable before acceptance is reported — no best-effort post-commit enqueue, no separate outbox store.
- **Keyed replay**: the put-receipt `request_identity` now covers canonical intents — an identical replay returns the original write + job acknowledgements (self-inputs resolve to the receipt's key so dedup matches the first commit); changed intents on the same key are `IdempotencyConflict`. `lookup_put_receipt_with_intents` lets the service skip the embedding provider entirely on replay.
- Store internals: `create_memory_inner` accepts a caller-assigned record key plus extra SQL fragments/binds inside the same transaction; `job_txn_fragment` emits per-intent input existence/scope/revision checks and the durable `CREATE nomiso_job` with unique bind names; `nomiso_bad_input` throws are mapped before generic conflict handling so intent validation surfaces as `InvalidOp` (HTTP 400), not a version `Conflict`.
- **Service + surfaces**: `NomisoClient::put_with_jobs`/`supersede_with_jobs` (receipt lookup before any provider call); plane HTTP `POST /v1/put_with_jobs`, `POST /v1/supersede_with_jobs`; plane MCP `nomiso_put_with_jobs`, `nomiso_supersede_with_jobs` (35 tools after slice 9 added `nomiso_search_detailed`). Product surfaces intentionally do not expose raw puts — Vegapunk writes remain `apply-ops` WriterOps.
- Verified by six new store tests (atomic commit, bad-intent rollback for put and supersede, self-input revision pinning, live dedup, keyed replay + intent conflict) plus an HTTP test covering atomic success, bad-intent rollback, and supersede-with-jobs end to end. `nomiso-mcp` dev-deps now enable `embedded-mem` so its tests run standalone.

Slice 9 — graph candidate expansion into retrieval (RET-001, REL-003, T5 experiment):

- **`GraphExpand`** request (`SearchQuery.graph_expand`, opt-in only): `predicates` allowlist, `direction`, `max_depth`, `max_visited`, `max_edges`, `max_candidates`, `deadline_ms`. Default queries and `graph_enrich` behavior are unchanged.
- **Store expansion** (`expand_candidates` in `surreal_store.rs`): direct lexical/vector/hybrid hits become BFS seeds; edges are scanned in keyset order over the `relationship` table restricted to `state = active`, the exact/prefix scope, the valid-time lens, and the predicate allowlist — bounded by depth/visited/edge/candidate/deadline *before* ranking. Neighboring memory rows are fetched with the same scope/temporal/category filters as the direct channel, merged by identity (a record already returned directly keeps its direct signals and gains `expansion` provenance; a record found via multiple paths merges them).
- **Honest signals, not endorsement**: expanded hits carry `signals.expanded = true`, a `rank_fallback` score (never BM25/vector relevance), and `expansion = {seed, depth, via_predicates}` provenance. `SearchStats.expansion` reports seed/edge/candidate counts, elapsed ms, and truncation reasons (`depth`, `visited`, `edges`, `candidates`, `deadline`, `seed_keys`) — no-neighbors vs truncated vs direct-search-failure stay distinguishable; expansion failure propagates rather than degrading silently.
- Service/facade/surfaces: `NomisoClient::search_detailed` returns `SearchOutcome { hits, stats }`; plane HTTP `POST /v1/search_detailed` and MCP `nomiso_search_detailed` expose the cost report to remote operators; `RecallOptions::graph_expand`/`HardRecallOptions::graph_expand` and `vegapunk hard-recall --expand-graph` let product recall opt in per call (expanded hits carry provenance into the pack path).
- Verified by six new store tests: direct-hit provenance merge, unmatched-neighbor surfacing, predicate filter + depth bound, scope + closed-edge exclusion, prefix-scope descendant expansion, and candidate-cap truncation.

Slice 9 evaluation evidence (`nomiso-eval::graph_expand`, fixture `graph_expand_v1`, 17 docs, k=6, hashing embedder — deterministic, no live provider): on graph-only probes (gold reachable only via edges), direct/hybrid/iterative-requery all scored **recall 0**; `expanded` scored **recall 1.0** at depth 1 and depth 2 with expansion-provenance hits only on edge-reachable docs (lexical trap and distractors never expanded). Cost at fixture scale: ~48ms vs ~20ms direct (7 edges scanned, no resource-bound truncation). On the lexical-baseline probe, expansion did not displace the direct hit.

Real-corpus evidence (`nomiso-eval::graph_expand_corpus`): 173 section docs mined from `docs/spec/*.md`, 38 organic `derived_from`/`depends_on` edges from requirement-ID references and doc links, 10 preregistered probes (9 graph-only), k=6. Hashing dim-8 row: expanded graph-only recall **0.89** vs 0 for every non-graph strategy; http `bge-small` 384 (local Ollama) row: expanded **1.0** vs 0. Noise cost +3–7 slots total; latency ~2.5–3× direct. Two findings recorded in `evals/README.md`: a degenerate embedder collapses the hybrid channel (hashing dim-8 → hybrid recall 0 on lexical probes where direct scores 1.0, and `expanded` inherits it since it seeds from hybrid), and the candidate cap legitimately fires on dense real neighborhoods with correct truncation reporting. **Decision: `graph_expand` remains opt-in as API default** (real ~3× latency, wins accrue only on graph-shaped queries) — but the opt-in is now evidence-backed rather than fixture-only, and expansion is recommended when semantic embeddings are configured.

Provider lifecycle (OPS-001) is implemented and tested at the `Embedder` surface: `ready()` probes (`GET {base}/models`, bounded), `embed_cancellable` with `CancellationToken` racing in-flight reqwest futures, `Error::Cancelled` distinct from deadline errors, and classified provider error strings that never echo URLs/keys/transport internals. `NomisoClient::provider_status()` surfaces it over the admin API.

Evidence relocation + staged backup/restore (OPS-004/005) landed: `FsBlobStore` confines all paths to the configured root (rejects `..`, absolute escapes, symlinks resolving out) and verifies blake3 content addresses on read — `S3BlobStore::get` verifies on read too; `nomiso_blob::relocate` copies+verifies CAS objects between backends idempotently; `nomiso_memory::artifact::relocate_artifacts` lists artifact rows, verifies destination bytes, and rebinds each row via a guarded update on the pre-copy location (concurrent writers can't be clobbered; failures leave rows untouched and are reported per-artifact). `nomiso-snapshot.sh` is a staged restore — extract → verify digests/file-set → validate CAS layout + symlink/path confinement → activate by atomic rename — with a manifest frontier (snapshot id, file/blob counts, per-table row counts via `store-stats`), post-activation row-count recheck, and `FRONTIER.json` markers + `verify` for mixed-generation detection. Self-test covers tamper, non-CAS payload, symlink members, FORCE semantics, and staged-dir leak.

## T3 implementation progress

**Toolkit extraction (`crates/nomiso-memory`) — landed.** The reusable memory mechanics moved out of `crates/vegapunk` into a product-free toolkit crate:

- **Moved**: `error`, `types` (incl. `ApplyOpsReport`, `CompactionIngest`/`Report`), `recall` (`RecallOptions`, `recall`, `EnumerateOptions`), `reader` (`HardRecallOptions`, `hard_recall`, `hard_recall_pack`, `pack_context`, `QueryRewriter`), `writer` (`WriterOp`, `preflight_ops`, `apply_ops`, `MemoryWriter`, `RuleWriter`, `write_episode`, `supersede_remember`), `remember`, `sleep` (`sleep_pass`, proposal application helpers), `checkpoint`, `artifact`, `llm` (BYOM `LlmCompletion` port + parsers), `cli_writer`, `trace_emit`.
- **Policy seam**: new `MemoryPolicy` value type carries the six knobs mechanics consult; every public entry point takes `impl Into<MemoryPolicy>` so products bring their own policy type. Vegapunk's `ProfilePolicy` converts via `From` — zero product call-site churn.
- **Product stays product**: `Profile`/`ProfilePolicy` (soft-inject, working-state restore, card limits), `cli_llm` process backends, eval runners, and the `Vegapunk` facade (`ingest_compaction`, trace-correlated flows) remain in `crates/vegapunk`, which re-exports the moved items so `vegapunk::writer::apply_ops` paths are source-compatible.
- **Exit gate**: `tests/standalone.rs` composes the toolkit end-to-end — typed writes → recall → pack → checkpoint → sleep — with only `nomiso-core`/`nomiso-service`/`nomiso-store` + `nomiso-memory` on the dependency path (per ARCH-008 the toolkit never depends on the `nomiso` facade). `nomiso-memory` is wired into `just check`.

Open in this milestone: `tact-on-nomiso` still talks to the foundation directly rather than the toolkit; the T4 controller is not started.

## Verified starting point

The workspace contains a substantial foundation, a higher-level Vegapunk library, binaries, and a Tact-shaped consumer.
It now contains `nomiso-memory` (T3 toolkit, see above). It does not yet contain the generic controller, versioned context manifests, or a durable generic worker beyond the in-process `Worker` runtime.

| Area | Baseline evidence | Important limit |
| --- | --- | --- |
| Foundation operations | [Core operation types](../../crates/nomiso-core/src/ops.rs), [store](../../crates/nomiso-store/src/surreal_store.rs) | Legacy contracts remain evolving |
| Supersession | Conditional close, successor creation, lineage linking, journal events, and stale-edge updates in one transaction | Journal applies to assertion mutations; a durable generic job journal is still absent |
| Retrieval | Surreal BM25/HNSW plus in-engine RRF | Graph enrich attaches stored links; it does not expand candidates |
| Evidence | Artifact/span/derivation APIs and filesystem CAS | Integrity, relocation, and deletion contracts need hardening |
| Product | [Vegapunk library](../../crates/vegapunk/src/lib.rs), profiles, writers, multipass recall, packs | Mechanics extracted to `nomiso-memory`; Vegapunk keeps profile policy + composition |
| Surfaces | CLI, product HTTP/MCP, plane HTTP/MCP | Parity and error contracts are incomplete |
| Evaluation | [Plane harness](../../crates/nomiso-eval/src/lib.rs), skill/system runners | Current system fact-hit is not answer correctness |
| Alternative consumer | [Tact example](../../examples/tact-on-nomiso/README.md) | Uses foundation; future toolkit reuse remains to be proved |

## Audit verification evidence

The audit session preceding this documentation rewrite observed:

| Command/check | Result |
| --- | --- |
| `just check` | Passed: formatting, workspace Clippy with denied warnings, configured plane/surface/product/example tests |
| `just smoke` | Passed, isolated ephemeral flow |
| `just smoke-durable` | Passed, embedded Rocks roundtrip |
| `just smoke-durable-mp` | Passed, separate processes |
| `just smoke-agent-durable` | Passed, CLI encode then recall across processes |
| `just smoke-coding-loop` | Passed, isolated pack/trace/candidate/CAS/compaction/apply/snapshot/restore flow |
| `cargo check -p vegapunk-cli --features cli-llm` | Passed |
| `cargo package -p nomiso --offline --no-verify` | Failed: path dependency `nomiso-blob` lacks a version requirement |
| `cargo package -p vegapunk-cli --offline --no-verify` | Failed: path dependency `nomiso` lacks a version requirement |
| `cargo package -p nomiso-core --offline --no-verify` | Archive produced; this did not compile-verify or publish it |

The observed toolchain was Rust/Cargo 1.98.0 and Just 1.55.1 on Linux.
This does not prove the declared Rust 1.89 minimum, other operating systems, remote database behavior, or live model quality.
`cargo package --list` passed even for packages that failed actual archive creation, so listing is not a packaging gate.
Live external-model/provider comparisons were not rerun for this audit.

Four isolated black-box probes also completed:

1. Opposite signed-artifact requirements produced a dry-run near-duplicate proposal with Jaccard 1.00. No deletion was applied.
2. A put followed by supersede of a missing prior returned a committed prefix and an error outcome, but process exit code zero.
3. An accepted roughly 5 KiB memory was retrieved but produced an empty pack with budget-too-small status.
4. A nonexistent explicit `VEGAPUNK_CONFIG` fell back to another configuration and exited successfully.

These probes observed behavior; they were not committed as permanent regression tests in the baseline.
The transition must first turn them into maintained tests.

Repo memory recall/working-state use in this audit environment was blocked by the configured HTTP embedder's missing credential environment variable.
No credential files were read and no store/embedding mode was silently changed to bypass the issue.
This is both an environment limitation and an example of unnecessary provider coupling for metadata operations.

## Gap register

Evidence labels: **R** = reproduced in the audit; **S** = confirmed in source, not independently stress-tested here; **T** = target capability absent or not demonstrated.
A gap is closed only with implementation and verification references, not by changing this table's wording.

| ID | Gap and evidence | Owning target | Priority/step |
| --- | --- | --- | --- |
| G01 | Improved: near-dup proposals are review-only (never applied); pairs with mechanical contradiction signals (negation asymmetry, differing numeric values) are now flagged `possible_conflict` with a "review as conflict, never merge" reason. Semantic slot changes without such markers still require reviewer judgment | MEM-007 | T1 |
| G02 | Resolved by design: `apply_ops_atomic` is deprecated and rejects **before any effect** (test: `deprecated_atomic_rejected_before_supersede_effects`); prefix-preserving `apply_ops` with `not_attempted` tails is the documented semantic — no compensating path exists to drift | WRITE-007 | T1 |
| G03 | Improved: CLI `apply-ops` exits nonzero on `!report.is_ok()`; product HTTP maps the first non-`not_attempted` per-op code to a typed status and returns the full outcome report; MCP surfaces carry typed per-op codes. Full MCP transport conformance remains G21 | WRITE-002, API-005, API-006, API-007 | T1 |
| G04 | Resolved: `RememberInput.idempotency_key` (and writer `put`/`supersede` ops) thread to `PutRequest.idempotency_key`; `client.put` does the receipt lookup before any provider call so replay returns the original ack | WRITE-005 | T1 |
| G05 | R: long first hit can empty the context pack | CTX-005 | T1 |
| G06 | R/S: missing explicit config falls back; paths depend on cwd; scope normalization needs consistent binding; state operations attach providers | API-003, API-004 | T1 |
| G07 | S: product cursor not consumable; score-order cursor mismatch affects text counts | RET-006, RET-007 | T1 |
| G08 | Resolved: as_of/known_as_of/sys_as_of/category/scope predicates are pushed into every search SQL branch before LIMIT; the remaining post-filter re-reads the exact hit ids (by-id verification, not a bounded scan) so eligible rows cannot be hidden | RET-008 | T1 |
| G09 | Resolved: `put_working_state` takes `expected_version` (None = create-only); CLI/MCP expose expected-version updates; second unversioned write conflicts (test: `working_state_second_none_conflicts_then_cas`) | DATA-010, WRITE-008 | T1 |
| G10 | Improved: ledger with checksums, newer/unknown-marker rejection, dim-check-before-DDL (T2 slice 2), and embedding identity/generation stamping (T2 slice 4) landed | MIG-001, MIG-002, MIG-004 | T2 |
| G11 | Improved: generations record family/model/dimension/normalization/encoding, staged reindex with source frontier + explicit activation (T2 slice 4), a `reindex` worker executor that builds+activates generations (T2 slice 6), and CLI/HTTP/MCP admin surfaces (T2 slice 7) landed | MIG-004, MIG-005 | T2 |
| G12 | Improved: assertion-mutation journal events and stale-edge updates commit atomically (T2 slice 1); durable job journal (T2 slice 5), consuming worker runtime with the `reindex` executor (T2 slice 6), and `put_with_jobs`/`supersede_with_jobs` committing canonical writes and required `JobIntent`s in one transaction with self-input revision pinning and keyed-replay intent identity (T2 slice 8) landed — the JOB-001 durable-acceptance path is now atomic | WRITE-003, WRITE-010, REL-004, JOB-001..007 | T2 |
| G13 | Improved: `nomiso-snapshot.sh` is staged restore — flock quiescence probe, `nomiso-snapshot-v2` manifest with declared frontier (snapshot id, file/blob counts+bytes, per-table row counts from `store-stats`), sha256 verification with extra-file detection, extract→validate→atomic-rename activation that never half-swaps, CAS layout + symlink/path confinement at staging, per-component `FRONTIER.json` markers + `verify` for mixed-generation detection, post-activation row-count recheck; `MemoryStore::table_counts()` is the frontier primitive, `rebind_artifact_location` is guarded, `nomiso_memory::relocate_artifacts` drives CAS migration with per-artifact reporting; self-test in `just check`. Remote-backend backup and `b3sum`-less content verification remain open | OPS-004, OPS-005 | T2 |
| G14 | Improved: HTTP embedder has deadlines/bounded bodies/redacted Debug (tested); OPS-001 lifecycle surface added — `Embedder::ready()` inspectable probe (`GET /models`, short deadline, bounded body, classified safe detail) and `Embedder::embed_cancellable` (in-flight abort via `CancellationToken`, `Error::Cancelled` distinct from deadline); `NomisoClient::provider_status` + `POST /v1/embed/provider_status` expose it to operators. Transport errors no longer echo addresses into error strings. `FsBlobStore::get` verifies blake3 content addresses on CAS reads | SEC-004, SEC-006, OPS-001, OPS-002 | T1–T2 |
| G15 | Improved: workspace at 0.2.0; crates.io check (2026-02): all `nomiso*` names + `nomisod`/`vegapunk-cli`/`tact-on-nomiso` unclaimed — the single collision is `vegapunk` itself (unrelated published GraphRAG crate), so the library crate needs a publish-time rename (e.g. `nomiso-vegapunk`) or stays path-only; per-crate READMEs complete; `cargo package --workspace --allow-dirty --offline` verifies all 15 crates compile from packaged tarballs via `just package-check` (isolated target dir + stale-extraction purge). Real `cargo publish` against the live index remains untested | PKG-001, PKG-002 | T1/T6 |
| G16 | Improved: `nomiso-memory` toolkit crate holds the reusable mechanics behind a `MemoryPolicy` seam; Vegapunk keeps product policy/composition and re-exports for source compat; standalone composition test proves the crate works without the product | ARCH-004, PROD-007 | T3 |
| G17 | Implemented: `nomiso-memory::context` — typed `PrepareContextRequest` (task/scope/inventory/global budget/effort plan/temporal lenses/degradation), `ContextProposal` with content-derived `proposal_id`, ordered blocks with per-candidate exclusion reasons and provenance, labeled token accounting (strict provider-token mode refused without a tokenizer), rendered text derived from blocks only, durable `pack` trace event binding the selection; `record_insertion` verifies the host's inserted subset against the recorded proposal, replays identical acks, conflicts on divergent ones; `Evaluator` distinguishes host/model/execution outcomes (CTX-010). CLI `prepare-context`/`record-insertion` + HTTP `/v1/prepare_context`, `/v1/record_insertion`, `/v1/trace_outcome`, `/v1/working_state` | CTX-001..CTX-010 | T4 |
| G18 | Improved: typed relationships, traversal, and opt-in graph candidate expansion landed (T2 slices 3 + 9); expansion is evaluation-gated, association is not endorsement | REL-001, REL-003, RET-001 | T2/T5 |
| G19 | T: evidence-grounded procedure lifecycle and transfer evaluation | MEM-006 | T5 |
| G20 | T: durable generic jobs, fencing, restartable derived processing | JOB-001, JOB-002, JOB-003, JOB-004 | T5 |
| G21 | Resolved: `cli_contract::mcp_stdio_typed_errors_and_tools` drives the real stdio transport — initialize handshake, tools/list with inputSchema validity for every advertised tool, tools/call typed results (isError + structured codes incl. partial-report payload), unknown-tool protocol error, `-32601` on unknown methods, malformed-frame resilience, clean EOF shutdown | API-007 | T1/T6 |
| G22 | Improved: `nomiso-eval::h2h` adds citation-grounded scoring — the answering agent declares `USED: [block_ids]`; the scorer verifies declared ⊆ *actually inserted* set (Nomiso: the `record_insertion` ack; foils: retrieved items — asymmetry labeled), then checks gold assertion against used-block text: grounded `fact_hit`, `leak` (gold asserted with zero used-block support), `citation_fraud` (unpresented ids), `abstain_ok`; latency/token/cost columns recorded. Head-to-head harness runs the real T4 loop (`prepare_context` → host subset → `record_insertion`) vs a Hindsight channel over the frozen coding corpus with preregistration in `evals/h2h-preregistration.md`; fixture + mock + live-gated tests landed, labeled rows in `evals/` | TEST-001, TEST-002, TEST-004 | Parallel evaluation work |
| G23 | Improved: thin Python SDK (`sdk/python/nomiso_vegapunk`, stdlib-only, typed errors, idempotent-safe retry) with a live-server contract test; reference harness loop (`examples/reference-harness/harness.py`) covering task-state → events → proposal → verified insertion ack → attributed outcome (API-009); `just feature-check` proves advertised feature rows compile independently (PKG-003); `docs/release-matrix.md` records verified OS/toolchain/engine rows honestly and `scripts/publish-order.sh` computes dep-first publish order (PKG-004); MSRV corrected to 1.97 after measuring real dep floors. TypeScript SDK and non-Linux release archives remain open | API-008, API-009, PKG-003, PKG-004 | T6 |
| G24 | S/T: exact historical revision replay and derived-view lineage are incomplete | DATA-002, DATA-007, DATA-009 | T2/T4 |

Additional test targets include malformed database response handling, sensitive-evidence admission, purge propagation, bounded archive extraction, and non-loopback serving controls.
They are not certified merely because no exploit or production failure was reproduced during this audit.

## Historical evaluation evidence

Former docs reported a local BGE-small 384-dimensional semantic control, live skill extraction, and full-pack system rows.
The evidence files remain in `evals/`, not in this specification tree:

- [Comparison record](../../evals/compare/last-miss.md).
- [Coding pack and runner guide](../../evals/coding_agent/README.md).
- [Evaluation guide](../../evals/README.md).

Some artifacts are maintainer-local/gitignored and may be absent from a fresh clone; they are not required specification inputs or release proof.
At the documented historical checkpoints, the skill report quoted precision 0.72 and recall 1.00, and the HTTP full-pack system report quoted fact-hit 30/30 with no sentence-match leaks.
These are historical author-reported measurements under the recorded protocols, not independently certified results from this consolidation.

The system scorer checks expected text against pack plus answer, uses permissive overlap, and can mark a run passed independently of quality.
Temporal/multi-hop exclusions and preview-era differences must remain visible.
Hindsight retain measurements included provider failures and use a different metric from extraction precision/recall.
Do not blend those rows, use them as evidence of answer correctness, or use skipped multi-hop cases as proof that graph retrieval has no value.

## Ordered transition

| Step | Deliverable | Exit gate |
| --- | --- | --- |
| T0 Specification | Canonical ownership, data/operation contracts, acceptance map, current-state register | Whole-spec review, complete source disposition, valid links, line cap |
| T1 Dependable baseline | Maintained regressions, safe writes/config/packing/pagination/errors, early packaging fixes | Relevant regressions and current foundation gate pass; known failures reported honestly |
| T2 Foundation primitives | Revision/evidence integrity, relationships, required transactions/journal, compatibility and recovery | Isolated upgrade, crash/concurrency, evidence and restore tests |
| T3 Reusable toolkit | Extract coherent mechanics into `nomiso-memory`; keep Vegapunk defaults separate | Independent Rust composition and isolated dependency-feature tests |
| T4 Controller | Typed events/proposals, retrieval plans, budgets, context manifests and acknowledgments | Direct Rust end-to-end workflows and replay/authority tests |
| T5 Capability experiments | Procedures, graph expansion, adaptive selection, optional background work as separate slices | Hard conformance plus preregistered workload/cost comparisons; explicit promotion decisions |
| T6 Product release | Thin adapters/SDKs, real MCP, clean-room install, supported deployment matrix | Packaged artifacts work outside checkout; documented capabilities match evidence |

Evaluation design and scorer-correctness work run alongside T1–T4, not after all algorithms are implemented.
Packaging hygiene for existing crates can proceed independently where it does not prematurely freeze new APIs.
Each slice is reviewed before the next dependent slice starts.

## First implementation batch after specification approval

1. Convert the four observed black-box failures into maintained regressions.
2. Correct consolidation and batch recovery semantics without reorganizing all crates simultaneously.
3. Fix config/provider coupling, pack assembly, pagination/counts, and error propagation in bounded slices.
4. Expose safe idempotency and working-state guards through the product.
5. Repair publishable dependency/version/resource packaging.
6. Review the complete baseline before beginning the richer schema/controller transition.

When extracting code, separate mechanical moves from behavior changes where possible.
Do not implement graph extraction, worker scheduling, a schema rewrite, and new default context policy in one unreviewable change.
No actual-store reset is part of the transition.

## Source disposition

The following are retired source paths, listed for historical traceability rather than as live links.
Their useful contracts were distilled into the specified owners; source history remains in Git.

| Retired document | Retained destination and deliberate changes |
| --- | --- |
| `docs/README.md` | 00 index; replace fragmented authority with one specification tree |
| `docs/spec.md` | 02–05, 10–13; retain operation/scope/time/provider principles, remove stale workspace/API sketches and unsupported completion claims |
| `docs/repo-layout.md` | 02; retain crates/bins/examples organization, add toolkit/composition direction |
| `docs/cognitive-stack.md` | 01–03, 06–09; retain memory taxonomy, evidence, procedural learning, host authority |
| `docs/nomiso-hybrid.md` | 05 and 14; preserve actual BM25/HNSW/RRF behavior and graph-attach limitation; allow composable alternatives |
| `docs/vegapunk.md` | 09 and 14; replace ownership of all high-level mechanics with reusable toolkit plus product composition |
| `docs/vegapunk-surfaces.md` | 07, 09, 10; preserve explicit baseline injection and skills, remove permanent transport/default algorithm dogma |
| `docs/vegapunk-agent-recipe.md` | 10 and 11; retain current durable workflow, remove unsafe overwrite/restore shortcuts |
| `docs/surface-parity.md` | 04, 10, 14; preserve current partial surfaces and non-ACID batch warning; specify stronger target errors/transactions |
| `docs/capability-matrix.md` | 01 and 14; retain relevant research lessons and gaps, not unverified competitor feature/score tables |
| `docs/roadmap.md` | This document; replace completed MVP sequence with T0–T6 |
| `docs/eval-tracks.md` | 13 and historical evidence references here; retain split tracks and comparable labels, strengthen scoring honesty |
| `docs/spectron.md` | 01 and 03; retain three-clock reasoning without unsupported vendor implementation assertions |
| `docs/tact-on-nomiso.md` | 09 and example README; preserve alternative policy, limits, and upgrade caveat |
| `docs/handoff/latest-state.md` | This baseline/gap register; retain useful evidence and constraints, remove stale host paths, service assumptions, and prior-session push authorization |

## Decisions superseded or clarified

| Earlier posture | Current architectural decision |
| --- | --- |
| Nomiso means only storage plane | Nomiso is foundation plus reusable memory toolkit; foundation stays model-free |
| All higher-level policy code belongs to Vegapunk | Reusable policies/mechanics belong to toolkit; Vegapunk selects a tested recipe |
| Graph/CE/HyDE must never start until the old scored rows miss | Investigate against a justified workload; require evidence for default promotion; no automatic feature expansion |
| Only manually requested recall can ever be useful | Explicit recall remains baseline; authorized bounded lifecycle proposals are permitted experiments |
| In-engine RRF is the only admissible composition | Keep optimized baseline; expose primitives and typed alternative policies |
| `apply_ops_atomic` is an adequate permanent public name | Explicit prefix versus real atomic-group contracts; no fictitious compensation guarantee |
| Small fixture success proves product quality | Separate mechanics, extraction, actual answer/task quality, grounding, harm, and cost |

The following remain: Surreal-only structured substrate, Rust-library-first behavior, no model inside foundation physics, no blind prompt dump, host-owned execution, scope not auth, no repository Pi auto-recall extension, no mixed external memory store, and local Justfile verification.

## Completion reporting

Every completed implementation slice records changed behavior, tests/commands and outcomes, compatibility impact, remaining gaps, and the next bounded step.
Do not mark a capability implemented because its requirement exists in this directory.
The specification-consolidation task changes documentation and references only, except any separately identified and verified preliminary correction.
At its conclusion, report the final spec tree/line count, review results, and any unverified runtime claims without manufacturing a new feature-completion result.
