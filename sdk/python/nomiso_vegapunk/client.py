"""Thin Vegapunk HTTP client — typed errors, no independent semantics."""

from __future__ import annotations

import json
import urllib.error
import urllib.request
from typing import Any, Dict, List, Optional


class NomisoError(Exception):
    """Typed error mirroring the server's error envelope.

    Attributes:
        code: stable machine code (e.g. ``conflict``, ``invalid_request``,
            ``idempotency_conflict``, ``policy_rejected``).
        message: server-sanitized message (safe to surface).
        status: HTTP status, when the request reached the server.
        help: operator hints attached to some errors.
    """

    def __init__(
        self,
        code: str,
        message: str,
        status: Optional[int] = None,
        help: Optional[List[str]] = None,
    ) -> None:
        super().__init__(f"[{code}] {message}")
        self.code = code
        self.message = message
        self.status = status
        self.help = help or []


def _is_idempotent_key(method: str, body: Optional[Dict[str, Any]]) -> bool:
    """Safe-retry check: only requests carrying an idempotency key (or pure
    reads) may be retried after a transport failure — a mutation without one
    might have committed already (OPS-001 honesty)."""
    if method == "GET":
        return True
    return bool(body and body.get("idempotency_key"))


class VegapunkClient:
    """Client for `vegapunk serve` (product HTTP API).

    Args:
        base_url: e.g. ``http://127.0.0.1:9091``.
        api_key: optional Bearer token (matches `--serve-api-key`).
        timeout: per-request timeout in seconds.
    """

    def __init__(
        self,
        base_url: str,
        api_key: Optional[str] = None,
        timeout: float = 30.0,
    ) -> None:
        self.base_url = base_url.rstrip("/")
        self.api_key = api_key
        self.timeout = timeout

    # -- transport ----------------------------------------------------------

    def _request(
        self,
        method: str,
        path: str,
        body: Optional[Dict[str, Any]] = None,
        *,
        _retried: bool = False,
    ) -> Any:
        url = f"{self.base_url}{path}"
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(url, data=data, method=method)
        req.add_header("Accept", "application/json")
        if data is not None:
            req.add_header("Content-Type", "application/json")
        if self.api_key:
            req.add_header("Authorization", f"Bearer {self.api_key}")
        try:
            with urllib.request.urlopen(req, timeout=self.timeout) as resp:
                raw = resp.read()
        except urllib.error.HTTPError as e:
            payload = e.read()
            try:
                doc = json.loads(payload)
            except (ValueError, UnicodeDecodeError):
                raise NomisoError("http_error", f"HTTP {e.code}", status=e.code) from None
            raise NomisoError(
                doc.get("code", "http_error"),
                doc.get("error", f"HTTP {e.code}"),
                status=e.code,
                help=doc.get("help") or [],
            ) from None
        except urllib.error.URLError as e:
            # Transport failure: retry once only for provably-safe requests.
            if not _retried and _is_idempotent_key(method, body):
                return self._request(method, path, body, _retried=True)
            raise NomisoError("unreachable", f"cannot reach server: {e.reason}") from None
        if not raw:
            return None
        return json.loads(raw)

    def _post(self, path: str, body: Dict[str, Any]) -> Any:
        return self._request("POST", path, body)

    # -- writes -------------------------------------------------------------

    def remember(
        self,
        scope: str,
        text: str,
        *,
        category: Optional[str] = None,
        confidence: Optional[float] = None,
        source: Optional[str] = None,
        episodic: bool = False,
        idempotency_key: Optional[str] = None,
    ) -> Any:
        """Store one memory (typed WriterOp equivalent of a single put)."""
        return self._post(
            "/v1/remember",
            {
                "scope": scope,
                "text": text,
                "category": category,
                "confidence": confidence,
                "source": source,
                "episodic": episodic,
                "idempotency_key": idempotency_key,
            },
        )

    def apply_ops(self, scope: str, ops: List[Dict[str, Any]]) -> Any:
        """Apply WriterOp JSON — the primary agent write path.

        Per-op outcomes come back in the report; a partial report is a typed
        error carrying the full outcome list (check ``error.status``).
        """
        return self._post("/v1/apply_ops", {"scope": scope, "ops": ops})

    def supersede(
        self,
        scope: str,
        prior_id: str,
        expected_version: int,
        text: str,
    ) -> Any:
        """Non-destructive revision of an existing memory (CAS-guarded)."""
        return self._post(
            "/v1/supersede",
            {
                "scope": scope,
                "prior_id": prior_id,
                "expected_version": expected_version,
                "text": text,
            },
        )

    # -- reads --------------------------------------------------------------

    def recall(self, scope: str, query: str, **opts: Any) -> Any:
        """Single-pass lexical/hybrid recall."""
        return self._post("/v1/recall", {"scope": scope, "query": query, **opts})

    def hard_recall(self, scope: str, query: str, **opts: Any) -> Any:
        """Multi-pass recall; pass ``pack=True`` for a context pack."""
        return self._post(
            "/v1/hard_recall", {"scope": scope, "query": query, **opts}
        )

    def read(self, scope: str, ids: List[str]) -> Any:
        """Full records for memory ids (needed before relying on a
        ``reference_only`` block)."""
        return self._post("/v1/read", {"scope": scope, "ids": ids})

    def checkpoint(
        self, scope: str, summary: str, force: bool = False
    ) -> Any:
        """Working-state checkpoint."""
        return self._post(
            "/v1/checkpoint",
            {"scope": scope, "summary": summary, "force": force},
        )

    # -- T4 controller ------------------------------------------------------

    def prepare_context(
        self,
        scope: str,
        task: str,
        *,
        entities: Optional[List[str]] = None,
        inventory: Optional[List[Dict[str, Any]]] = None,
        budget: Optional[Dict[str, Any]] = None,
        effort: Optional[str] = None,
        request_id: Optional[str] = None,
        min_score: Optional[float] = None,
        allow_partial: bool = False,
        **kw: Any,
    ) -> Any:
        """Typed context proposal (CTX-*): blocks + selection manifest.

        Returns the proposal object; the host inserts ``blocks`` itself and
        then acknowledges via :meth:`record_insertion`.
        """
        body: Dict[str, Any] = {
            "scope": scope,
            "task": task,
            "degradation": "allow_partial" if allow_partial else "strict",
            **kw,
        }
        if entities:
            body["entities"] = entities
        if inventory:
            body["inventory"] = inventory
        if budget:
            body["budget"] = budget
        if effort == "expanded":
            body["effort"] = {"expanded": {}}
        elif effort:
            body["effort"] = effort
        if request_id:
            body["request_id"] = request_id
        if min_score is not None:
            body["min_score"] = min_score
        return self._post("/v1/prepare_context", body)

    def record_insertion(
        self,
        *,
        trace_id: str,
        proposal_id: str,
        scope: str,
        host: str,
        inserted: List[Dict[str, Any]],
        session_id: Optional[str] = None,
        turn_id: Optional[str] = None,
    ) -> Any:
        """Acknowledge which proposal blocks were actually inserted.

        Idempotent: replaying an identical ack returns the same receipt; a
        divergent ack is ``idempotency_conflict``. Blocks must come from the
        proposal's selected set — fabrication is ``invalid_request``.
        """
        return self._post(
            "/v1/record_insertion",
            {
                "trace_id": trace_id,
                "proposal_id": proposal_id,
                "scope": scope,
                "host": host,
                "inserted": inserted,
                "session_id": session_id,
                "turn_id": turn_id,
            },
        )

    def trace_outcome(
        self,
        scope: str,
        trace_id: str,
        outcome: str,
        *,
        evaluator: str = "host",
        note: Optional[str] = None,
    ) -> Any:
        """Attributed outcome: ``outcome`` in helped|harmed|unknown|skipped,
        ``evaluator`` in host|model|execution (CTX-010)."""
        return self._post(
            "/v1/trace_outcome",
            {
                "scope": scope,
                "trace_id": trace_id,
                "outcome": outcome,
                "evaluator": evaluator,
                "note": note,
            },
        )

    def working_state(
        self,
        scope: str,
        put_json: Optional[Dict[str, Any]] = None,
        expected_version: Optional[int] = None,
    ) -> Any:
        """Coding working-state slot. ``put_json=None`` reads; a dict writes —
        create-only when ``expected_version`` is omitted, CAS otherwise."""
        return self._post(
            "/v1/working_state",
            {
                "scope": scope,
                "put_json": put_json,
                "expected_version": expected_version,
            },
        )

    # -- health -------------------------------------------------------------

    def health(self) -> Any:
        return self._request("GET", "/health")
