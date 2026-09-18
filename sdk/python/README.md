# nomiso_vegapunk — thin Python SDK

Stdlib-only client for the Vegapunk product HTTP API (`vegapunk serve`).
Python ≥ 3.9, zero dependencies.

```python
from nomiso_vegapunk import VegapunkClient, NomisoError

vp = VegapunkClient("http://127.0.0.1:9191", api_key=None)

vp.remember(scope="org/demo", text="Alice prefers TypeScript.",
            category="semantic", idempotency_key="fact-1")

proposal = vp.prepare_context(scope="org/demo", task="language prefs")
for b in proposal["blocks"]:
    print(b["memory_id"], b["excerpt"])

# The host inserts blocks itself, then acknowledges the actual subset:
vp.record_insertion(trace_id=proposal["trace_id"],
                    proposal_id=proposal["proposal_id"],
                    scope="org/demo", host="my-harness",
                    inserted=[{"block_id": proposal["blocks"][0]["block_id"]}])

vp.trace_outcome("org/demo", proposal["trace_id"], "helped",
                 evaluator="execution")
```

## Design contract (API-008)

- **Thin**: no independent conflict resolution, retention, or ranking — the
  server is authoritative.
- **Typed errors**: `NomisoError.code` mirrors the server taxonomy
  (`conflict`, `invalid_request`, `idempotency_conflict`, `policy_rejected`,
  `provider_unavailable`, …); `e.status` carries the HTTP status.
- **Safe retries only**: transport-level retry is limited to GETs and
  requests carrying an `idempotency_key` — a mutation without one may have
  committed already and is never silently retried.
- **Privacy**: server error bodies are already sanitized; the SDK surfaces
  `error`/`code`/`help` verbatim.

## Test

```bash
just sdk-check    # or: python3 sdk/python/tests/test_contract.py <bin>
```
