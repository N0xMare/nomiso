"""Thin Python client for the Vegapunk product HTTP API (`vegapunk serve`).

API-008 contract: this SDK is deliberately thin — it reuses the versioned
server contract and typed errors, and implements **no** independent conflict
resolution, retention, or ranking. The server stays authoritative.

Stdlib only (urllib); no third-party dependencies. Requires Python ≥ 3.9.

Usage:
    from nomiso_vegapunk import VegapunkClient, NomisoError

    vp = VegapunkClient("http://127.0.0.1:9091", api_key=None)
    vp.remember(scope="org/demo", text="Alice prefers TypeScript.")
    proposal = vp.prepare_context(scope="org/demo", task="language prefs")
    # host inserts proposal["blocks"] itself, then acknowledges:
    vp.record_insertion(
        trace_id=proposal["trace_id"],
        proposal_id=proposal["proposal_id"],
        scope="org/demo",
        host="my-harness",
        inserted=[{"block_id": b["block_id"]} for b in proposal["blocks"][:1]],
    )
"""

from .client import NomisoError, VegapunkClient

__all__ = ["VegapunkClient", "NomisoError"]
__version__ = "0.2.0"
