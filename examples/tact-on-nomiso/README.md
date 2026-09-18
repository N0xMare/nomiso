# Tact memory on Nomiso

[Tact](https://github.com/clabby/tact)-shaped **global, explicit, bounded** agent memory, implemented as a thin adapter on **Nomiso** (SurrealDB) instead of SQLite.

This lives under `examples/` as a **consumer** of the Nomiso plane — not core library code. Vegapunk is the separate product binary under `bins/vegapunk`.

## Contract (Tact-like)

- Opt-in / explicit `scan` · `read` · `put` · `delete`
- **No** auto-inject of memory into prompts
- Root-only mutation; children may scan/read
- Atomic conclusions, capacity bounds, optimistic versions
- BM25-first retrieval (vectors unused for parity)
- Single corpus scope: `tact/global` by default

## Library

```rust
use tact_on_nomiso::{Actor, TactMemory, TactMemoryConfig};
use nomiso::StoreConfig;

# async fn demo() -> tact_on_nomiso::Result<()> {
let m = TactMemory::connect(StoreConfig::memory_test(8), TactMemoryConfig::default()).await?;
m.scan(Actor::Root, "prefer TypeScript", 5).await?; // arms put for root
let put = m.put(Actor::Root, "Prefer TypeScript for agent tooling.", None).await?;
let _ = m
    .read(Actor::Root, std::slice::from_ref(&put.record.key.id))
    .await?;
# Ok(())
# }
```

## CLI

```bash
# Demo (ephemeral per process)
cargo run -p tact-on-nomiso -- scan --query "TypeScript"
cargo run -p tact-on-nomiso -- put --content "Prefer TypeScript for agent tooling."

# Multi-process durable (Surreal embedded Rocks; .tact-data/ is gitignored)
cargo run -p tact-on-nomiso --features embedded-rocks -- \
  --endpoint rocksdb://./.tact-data put --content "Prefer TypeScript for agent tooling."
```

`--endpoint memory` is a **fresh** in-memory store each process. Tact is BM25-only (no vectors).

## Docs

- Design: [docs/spec/09-vegapunk-product.md](../../docs/spec/09-vegapunk-product.md)
- Tact original: https://github.com/clabby/tact/blob/main/docs/memory.md
