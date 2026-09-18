# Changelog

All notable changes to Nomiso/Vegapunk. Format: [Keep a Changelog](https://keepachangelog.com/).
This project is pre-release (`0.x`); minor bumps may include breaking changes.

## [Unreleased]

### Added — T4 controller contract (spec 07)

- `nomiso-memory::context`: `PrepareContextRequest` → `ContextProposal` with a
  durable `SelectionManifest` — typed task/scope/inventory/budget/effort/
  temporal request, per-candidate inclusion/exclusion reasons, labeled token
  accounting, and content-derived proposal identity.
- `record_insertion`: verified, idempotent host acknowledgment of which
  proposal blocks were *actually* inserted — subset + truncation recorded;
  fabrication and divergent re-acks are typed errors.
- Vegapunk surfaces: `vegapunk prepare-context`, `vegapunk record-insertion`,
  `--evaluator` on `trace-outcome` (host/model/execution attribution), and
  HTTP routes `POST /v1/prepare_context`, `/v1/record_insertion`,
  `/v1/trace_outcome`, `/v1/working_state`.
- `nomiso-embed`: `Embedder::ready()` bounded provider probe and
  `embed_cancellable` (worker shutdown now aborts in-flight embeds).
- `nomiso-blob`: filesystem confinement + BLAKE3 verification on fs and S3
  reads; `relocate` module with per-object verified copy reports.
- `nomiso-memory::relocate_artifacts`: CAS migration driver — verify bytes at
  destination before a guarded `rebind_artifact_location` rewrites metadata.
- `MemoryStore`: `table_counts`, `list_artifacts`, `rebind_artifact_location`;
  `vegapunk store-stats` CLI.
- `nomiso-snapshot.sh`: staged restore (extract → manifest/CAS/confine verify
  → atomic rename with recovery copies), `nomiso-snapshot-v2` manifest with
  declared table frontier, `FRONTIER.json` generation markers, `verify`
  subcommand, live-writer flock probe.
- `nomiso-eval::graph_expand_corpus`: real-corpus eval (173 spec sections, 38
  organic edges, 10 preregistered probes) — `graph_expand` evidence: graph-only
  recall 0.89–1.0 vs 0 for non-graph strategies; remains opt-in.
- Thin Python SDK (`sdk/python/nomiso_vegapunk`) and a reference harness
  (`examples/reference-harness/harness.py`) covering the full bookkeeping loop.
- `just feature-check` (PKG-003 feature closure), `just sdk-check`,
  `just harness-check`, `just release-check`, `scripts/publish-order.sh`.
- `nomiso-eval::h2h` (G22): citation-grounded head-to-head harness — the
  answering agent declares `USED: [block_ids]`, verified against the
  *actually inserted* set (Nomiso: `record_insertion` ack; foils: retrieved
  items), scoring grounded `fact_hit` / `leak` / `citation_fraud` /
  `abstain_ok` plus latency and estimated-token columns. Preregistered in
  `evals/h2h-preregistration.md`; `just eval-h2h` / `eval-h2h-http` rows,
  skip-honest without the optional services. `EVAL_HINDSIGHT_EXTRACT` pins
  or releases the Hindsight retain-extraction mode per run.
- Windows is declared unsupported; macOS arm64 is an intended target
  platform (local verification only — no CI).

### Changed

- MCP stdio conformance: real JSON-RPC lifecycle coverage — initialize,
  tools/list schema validity, typed tool errors, unknown-method `-32601`,
  malformed-input resilience, clean EOF shutdown (API-007).
- Workspace crates at `0.2.0` with shared keywords/categories metadata.
- `package-check` runs in an isolated `CARGO_TARGET_DIR` — same-version
  repackaged deps can no longer verify against stale registry rlibs.

### Fixed

- Snapshot CAS validation now uses posix-extended regex; `FRONTIER.json` is
  excluded from CAS archives; restore preflights all destinations before any
  activation (no mixed-generation installs).
- Toolkit blob/store failures carry real error codes (`blob_integrity`,
  `store`) instead of `invalid_request`.
