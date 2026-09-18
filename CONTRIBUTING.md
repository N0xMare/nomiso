# Contributing

Thanks for your interest. This is a pre-release prototype; the spec tree in
`docs/spec/` is the source of truth for architecture and requirements.

## Ground rules

- **The spec is normative.** When behavior and spec disagree, that's a bug —
  fix the code or mark the gap honestly in `docs/spec/14`'s register.
- **Harness authority is sacred.** Vegapunk proposes context; the host decides
  placement and inserts. Never add an auto-injection hook.
- **No silent failures.** Prefer typed errors over empty results; label
  approximations (token estimates, lexical scores) as approximations.
- **Privacy-safe errors.** Never echo secrets, provider bodies, URLs, or
  socket addresses into caller-visible errors or logs.

## Verify before submitting

```bash
just check          # fmt + clippy -D warnings + all unit/integration tests
just package-check  # release gate: crates compile from packaged tarballs
just feature-check  # advertised feature rows compile independently
just sdk-check      # Python SDK ↔ live server contract
just harness-check  # reference harness bookkeeping loop
```

Add tests for behavior changes. Rust style: `cargo fmt`, no `unsafe`,
`#![forbid(unsafe_code)]` in library crates, errors via `thiserror` with
stable `code()` + `public_message()`.

## Project layout

- `crates/nomiso-*` — the foundation plane (schema, store, service, blob,
  embed, http, mcp, eval)
- `crates/nomiso-memory` — reusable agent-memory mechanics (the T3 toolkit)
- `crates/vegapunk` — opinionated product composition (profiles, writers)
- `bins/vegapunk`, `bins/nomisod` — CLI + daemon
- `sdk/python` — thin HTTP client (stdlib only)
- `examples/` — reference harness + tact-on-nomiso
