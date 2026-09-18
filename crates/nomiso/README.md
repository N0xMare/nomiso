# nomiso

SurrealDB-native agent memory foundation: the `nomiso` facade re-exports the
core op types, store trait, service client, worker runtime, and embedder
contracts so downstream compositions depend on one crate.

- Durable writes with keyed idempotency, optimistic versioning, and
  tri-temporal lenses (`as_of` / `known_as_of` / `sys_as_of`).
- Typed relationships, bounded traversal, and opt-in graph candidate
  expansion (`graph_expand`).
- Durable job journal with fencing, checkpoints, and atomic
  `put_with_jobs` / `supersede_with_jobs` acceptance.
- In-process `Worker` claim loop plus a built-in `reindex` executor.

Lean default feature set is embedded-memory only; enable `remote-ws`,
`embedded-rocks`, `http`, or `mcp` as needed. See `docs/spec/` in the
repository for the normative contracts.
