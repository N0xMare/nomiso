# nomiso-memory

Reusable agent-memory mechanics on the [Nomiso](../nomiso) plane.

This crate is the toolkit layer: writer ops (`WriterOp`, `apply_ops`,
`preflight_ops`), recall and context packs (`recall`, `hard_recall`,
`pack_context`), structured writes (`remember`), consolidation (`sleep_pass`),
checkpoints, artifacts, and the BYOM ports (`MemoryWriter`, `LlmCompletion`,
`QueryRewriter`).

It carries **no product policy**: every mechanic takes a plain
[`MemoryPolicy`](src/policy.rs) value. Vegapunk composes these mechanics with
profile-derived policy; other products or harnesses can do the same without
pulling in Vegapunk itself.

The host stays authoritative over prompt injection — `pack_context` produces a
pack, the host decides whether to inject it.
