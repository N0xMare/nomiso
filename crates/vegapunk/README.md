# Vegapunk

Opinionated agent memory **system** on **[Nomiso](../nomiso)** (SurrealDB only).

| Plane | API |
|---|---|
| Write | `remember` · `supersede` · `write_episode` + `MemoryWriter` · `apply_writer_ops` · `store_artifact` · `ingest_compaction` |
| Read | `recall` · `hard_recall` / `hard_recall_pack` (host injects) · `find_candidates` |
| Policy | profiles · `checkpoint` · `sleep` (dry-run default; `--apply` soft-forget near-dups) |

No LLM inside the plane. Attach BYOM via `MemoryWriter` / `QueryRewriter` / `LlmCompletion`. Ops are typed only.

```rust
use vegapunk::{Vegapunk, Profile, RememberInput, RuleWriter, CliChatWriter, MockLlm};
use std::sync::Arc;

# async fn demo() -> vegapunk::Result<()> {
// Rule path (no model)
let vp = Vegapunk::connect_memory(32).await?
    .with_profile(Profile::CodingAgent)
    .with_writer(Arc::new(RuleWriter));

// Or mock / CLI-backed writer (CliLlm behind feature `cli-llm`)
let mock = Arc::new(MockLlm {
    text: r#"[{"op":"put","scope":"org/demo","text":"Alice prefers Rust.","category":"semantic"}]"#.into(),
});
let _byom = Vegapunk::connect_memory(32).await?
    .with_writer(Arc::new(CliChatWriter::new(mock)));

vp.remember(RememberInput::fact(
    "org/demo/user/alice",
    "Alice prefers TypeScript for agent tooling.",
)).await?;
let (hr, pack) = vp.hard_recall_pack("org/demo/user/alice", "TypeScript").await?;
# let _ = (hr, pack);
# Ok(())
# }
```

CLI: `bins/vegapunk` (`--writer rule|grok|codex`, `eval`). Docs: `docs/spec/09-vegapunk-product.md`. Offline suites: `evals/`.
