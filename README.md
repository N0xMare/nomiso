# Nomiso

**agent memory experiments using surrealdb**

The current state is a thin, deterministic memory plane: schema, belief revision, hybrid retrieval, scopes, provenance, and the structured op contract (`nomiso.op.v1` — **evolving**, not a frozen public ABI). Consumers bring their own models no API keys required for core put/search.

## Documentation

| Doc | Contents |
|---|---|
| [`docs/spec/00-index.md`](docs/spec/00-index.md) | Canonical specification and reading guide |
| [`docs/spec/02-architecture-and-dependencies.md`](docs/spec/02-architecture-and-dependencies.md) | Rust-first foundation, reusable toolkit, and product composition |
| [`docs/spec/09-vegapunk-product.md`](docs/spec/09-vegapunk-product.md) | Vegapunk profiles and supported product contract |
| [`docs/spec/10-interfaces-and-integration.md`](docs/spec/10-interfaces-and-integration.md) | Library/CLI/HTTP/MCP integration and current durable recipe |
| [`docs/spec/13-verification-and-evaluation.md`](docs/spec/13-verification-and-evaluation.md) | Correctness gates and separate evaluation tracks |
| [`docs/spec/14-current-state-and-transition.md`](docs/spec/14-current-state-and-transition.md) | Verified baseline, gaps, and implementation sequence |

The specification defines the target architecture. The current implementation remains experimental

### Layout

- **`crates/`** — Nomiso library, `nomiso-memory` toolkit (+ Vegapunk lib)  
- **`bins/vegapunk`**, **`bins/nomisod`** — product / plane binaries  
- **`sdk/python/`** — thin HTTP client (stdlib only)  
- **`examples/`** — reference harness + memory-system implementations on Nomiso (e.g. `tact-on-nomiso`)  
- **`evals/`** — Phase C0 offline suites + citation-grounded head-to-head (`h2h`, live-gated: nomiso-t4 vs local Hindsight under one answering agent, preregistered in `evals/h2h-preregistration.md`). **Measured (own suite, 2026-09):** grounded fact-hit parity with the Hindsight channel (25–27 of 29–30 on both), 0 citation-fraud/leak rows, `nomiso-t4` channel p50 ≈19–44ms vs ≈345ms for the local Hindsight channel — parity evidence, not a SOTA claim; see `evals/README.md`
- **`AGENTS.md`**, **`.pi/skills/vegapunk/`** — Pi (this repo): Vegapunk via CLI; `just dogfood-pi`

> **Nomiso** is the Rust memory toolkit (foundation plus reusable memory-system components). **Vegapunk** is its opinionated product composition. Harnesses own execution and actual context insertion.

### Vegapunk (built on Nomiso)

```bash
cargo run -p vegapunk-cli -- encode --scope org/demo/user/alice \
  --text "Alice prefers TypeScript for agent tooling."
cargo run -p vegapunk-cli -- recall --scope org/demo/user/alice --query "TypeScript"
cargo run -p vegapunk-cli -- hard-recall --scope org/demo --query "NOMI_SCOPE_DENIED" --pack
cargo run -p vegapunk-cli -- write-episode --scope org/demo --text "Alice prefers Rust."
cargo run -p vegapunk-cli -- --writer grok write-episode --scope org/demo --text "..."  # BYOM CLI
cargo run -p vegapunk-cli -- eval   # offline skill C0
cargo run -p vegapunk-cli -- checkpoint --scope org/demo --summary "Finished auth refactor; prefer JWT."
```

Library: `vegapunk::Vegapunk` — see [`docs/spec/09-vegapunk-product.md`](docs/spec/09-vegapunk-product.md).

T4 controller surface (typed proposals, verified insertion acks):

```bash
vegapunk prepare-context --scope org/demo --task "deploy freeze rules" \
  --budget-tokens 600 --effort direct
vegapunk record-insertion --trace-id <trace_id> --proposal-id <proposal_id> \
  --block <block_id>            # verified subset ack, idempotent
vegapunk trace-outcome --trace-id <id> --outcome helped --evaluator execution
```

Over HTTP (`vegapunk serve`): `POST /v1/prepare_context`, `/v1/record_insertion`,
`/v1/trace_outcome`, `/v1/working_state`. Thin Python SDK:
`sdk/python/nomiso_vegapunk`; reference harness loop:
`examples/reference-harness/harness.py` (API-009).

## Features (MVP)

- Embeddable Rust library + `nomisod` daemon (HTTP + MCP stdio)
- Structured ops: `put` / `supersede` / `search` / `read` / `forget` / `list` / `count` / `task_state` / `belief_events`
- Hybrid search: BM25 + HNSW vectors + in-engine RRF (`search::rrf`)
- Non-destructive supersession in a **single transaction** with valid-time `as_of` and typed version `Conflict`
- Tri-temporal lenses: `as_of` (valid), `known_as_of`, `sys_as_of` + one-shot close + `belief_event` journal
- Scope **filtering** (exact default, optional prefix) — not multi-tenant authorization
- Optimistic concurrency on mutate; search hits label `score_kind` (engine vs rank fallback)
- T4 controller contract: typed `prepare-context` proposals + selection manifests, verified `record-insertion` acknowledgments, attributed outcomes — the host stays authoritative over insertion
- Plane MCP: 35 tools; product (Vegapunk) MCP: 42 tools — see surface policy in `docs/spec/10-interfaces-and-integration.md`

### Local verify (no GitHub Actions required)

```bash
just check          # fmt --check + workspace clippy + plane/surfaces/vegapunk/tact tests
just test-plane
just test-vegapunk
just smoke               # vegapunk single-process smoke
just smoke-durable       # embedded rocks (feature)
just smoke-durable-mp    # multi-process plane put → search
just smoke-agent-durable # multi-process agent encode → hard-recall
just foundation          # unit gate + durable/agent smokes + coding-loop snapshot
just snapshot            # offline copy of .nomiso-data + .nomiso-blobs
just sdk-check           # Python SDK ↔ `vegapunk serve` contract test
just harness-check       # reference harness bookkeeping loop (API-009)
just feature-check       # advertised feature rows compile independently (PKG-003)
just package-check       # all crates compile-verify from packaged tarballs
just release-check       # package + feature + sdk + harness gates
just eval-semantic       # HTTP vs Hindsight; skip-honest without a key
```

Run `just check` before pushing — it is the whole gate.

**Durable agents:** `vegapunk init` defaults to `rocksdb://./.nomiso-data` (Surreal embedded Rocks). Use `memory` only as an explicit demo. HTTP embed is optional (`embed_url` + env key). See [`docs/spec/10-interfaces-and-integration.md`](docs/spec/10-interfaces-and-integration.md).

## embedded path

```toml
# Cargo.toml
nomiso = { path = "crates/nomiso", features = ["embedded-mem"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

```rust
use nomiso::{NomisoClient, PutRequest, SearchQuery, StoreConfig};
use nomiso::types::{Category, Content};

#[tokio::main]
async fn main() -> nomiso::Result<()> {
    let client = NomisoClient::connect(StoreConfig::memory_test(8)).await?;

    client
        .put(PutRequest {
            scope: "org/demo/user/alice".into(),
            category: Category::Semantic,
            content: Content::text("Alice prefers TypeScript for agent tooling."),
            confidence: Some(0.9),
            ..Default::default()
        })
        .await?;

    let hits = client
        .search(SearchQuery {
            query: "TypeScript".into(),
            scope: "org/demo/user/alice".into(),
            limit: Some(5),
            graph_enrich: Some(false),
            ..Default::default()
        })
        .await?;

    for h in hits {
        println!("{}  {}", h.score, h.preview);
    }
    Ok(())
}
```

```bash
cargo test -p nomiso-store --features embedded-mem
cargo test -p nomiso-core
```

## daemon path

```bash
# In-memory demo (no external SurrealDB required).
# WARNING: endpoint "memory" / mem:// is ephemeral — process exit loses all data.
# Prefer a durable Surreal endpoint for any real work.
cargo run -p nomisod -- serve --bind 127.0.0.1:9090

# Put
curl -s localhost:9090/v1/put -H 'content-type: application/json' -d '{
  "scope": "org/demo",
  "category": "semantic",
  "content": { "text": "Alice prefers TypeScript" }
}'

# Search
curl -s localhost:9090/v1/search -H 'content-type: application/json' -d '{
  "query": "TypeScript",
  "scope": "org/demo",
  "limit": 5
}'
```

### MCP stdio

```bash
cargo run -p nomisod -- serve --mcp-stdio
```

Tools (10): `nomiso_search`, `nomiso_read`, `nomiso_put`, `nomiso_supersede`, `nomiso_forget`,
`nomiso_list`, `nomiso_count`, `nomiso_put_task_state`, `nomiso_get_task_state`, `nomiso_belief_events`.

Point a coding agent at the binary; paste a skill from `examples/skills/`.

### Config (`nomiso.toml`)

```toml
[surreal]
endpoint = "memory"          # or ws://127.0.0.1:8000
namespace = "nomiso"
database = "memory"

[embedding]
dimension = 1536

[daemon]
bind = "127.0.0.1:9090"
api_key = "change-me"
```

Env overrides use prefix `NOMI_SO_` with `__` nesting (e.g. `NOMI_SO_SURREAL__ENDPOINT`).

## Crate layout

| Crate | Role |
|---|---|
| `nomiso-core` | Types, ABI, validation, traits |
| `nomiso-schema` | Embedded SurrealQL migrations |
| `nomiso-store` | SurrealDB repository + hybrid queries |
| `nomiso-service` | `NomisoClient` facade |
| `nomiso-http` | Axum JSON API |
| `nomiso-mcp` | Official `rmcp` tools |
| `nomiso` | Public facade |
| `nomisod` | Daemon binary |

## License

Apache-2.0
