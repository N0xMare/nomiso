# Security Policy

## Reporting a vulnerability

Please report security issues privately to the maintainers via GitHub's
private vulnerability reporting on this repository. Do not open a public
issue for a suspected vulnerability.

## Scope and trust model

Nomiso/Vegapunk is an agent-memory toolkit. The security-relevant surface:

- **HTTP surfaces** (`vegapunk serve`, `nomisod`) — optional Bearer-key auth;
  bind to localhost by default. Treat any exposed port as a trusted-network
  boundary; the API is not hardened for untrusted multi-tenant use.
- **MCP stdio** — protocol-safe stdout; tool errors are typed results, never
  process crashes.
- **CAS/blob store** — filesystem paths are confined to the configured root
  (no `..`/absolute/symlink escapes); BLAKE3 content integrity is verified on
  reads.
- **Provider calls** — embed/LLM credentials are never logged; provider error
  detail is classified (URLs, socket addresses, response bodies are not
  echoed into caller-visible errors).
- **Snapshot/restore** — archives are sha256-manifested and staged-verified
  before activation; path traversal and symlink escapes are rejected.

## Honest limits

- Scope filtering is not multi-tenant authorization.
- Retrieved memory is attributed *evidence*, never instruction authority —
  but prompt-injection resistance of the host model is out of scope.
- Hard-forget removes the record; CAS object GC is not yet implemented
  (artifacts are append-only).
- `remote-ws` backend backup has no snapshot story yet.

## Supported versions

Pre-release (`0.2.x`): only the latest commit on `main` receives fixes.
