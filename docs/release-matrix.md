# Release matrix (PKG-004)

Honest record of what is actually verified. A build on one platform is not
support for every platform; combinations below are the advertised surface.

## Verified combinations

| Axis | Verified | How |
|---|---|---|
| OS / arch | Linux x86_64 | full `just check` + `release-check` gate |
| Rust toolchain | stable (1.98.x tested); **MSRV 1.97** | `cargo +1.97.1 check --workspace` |
| Engines | `embedded-mem`, `embedded-rocks` | workspace tests + durable smokes |
| Transports | product HTTP (`vegapunk serve`), MCP stdio (plane + product) | contract tests incl. real JSON-RPC lifecycle |
| Embedders | hashing (default, offline), OpenAI-compatible HTTP (`nomiso-embed/http`), local Ollama BGE-384 | unit tests + labeled eval rows |
| Packaging | all 14 publishable crates compile-verify from packaged tarballs (offline tmp-registry) | `just package-check` |

## Explicitly not verified

- macOS arm64 (Apple Silicon) — intended target platform; local development
  only, no automated run has executed yet.

- Windows builds — not a supported target; the `remote-ws` engine path can
  substitute for embedded RocksDB if ever needed there.
- crates.io naming: all `nomiso*` names are unclaimed, but **`vegapunk` is
  taken by an unrelated published crate** — the library would publish under a
  renamed package (e.g. `nomiso-vegapunk`) or stay path-only.
- `remote-ws` (remote SurrealDB) backup/snapshot story — snapshot tooling is
  filesystem-copy only (OPS-004 is partial for remote backends).
- `remote-ws` and `embedded-surrealkv` engine rows beyond compile checks.
- `cargo publish --dry-run` against the live index (no registry auth in dev);
  `scripts/publish-order.sh --dry-run` runs it when credentials exist.
- TypeScript SDK — only the Python thin client exists (API-008).

## MSRV note

`rust-version` is the floor the workspace *type-checks* against, not a
best-effort claim. Transitive SurrealDB dependencies set the floor: the
dependency set currently requires ≥1.94 (`fastnum`); 1.89 fails resolution.
We declare **1.97** — the oldest toolchain actually verified.

## Crate publish order

`./scripts/publish-order.sh` computes the dependency-first order from
`cargo metadata`:

```
nomiso-blob nomiso-core nomiso-schema nomiso-store nomiso-service
nomiso-embed nomiso-http nomiso-mcp nomiso-memory nomiso nomiso-eval
nomisod vegapunk vegapunk-cli
```

Each crate must be live on crates.io (index-propagated) before its dependents
publish — `--publish` sleeps between crates for this.
