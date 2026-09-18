# nomisod

Nomiso daemon binary: serves the memory plane over HTTP (`/v1/*`) and MCP
(`nomiso_*` tools) from one `NomisoClient`. Structured ops only — no LLM
inside Nomiso; hosts own prompt assembly and model calls.

Run against an embedded RocksDB path or a remote SurrealDB endpoint; Bearer
auth applies to HTTP routes when an API key is configured. See
`docs/spec/10-interfaces-and-integration.md` for the surface contract.
