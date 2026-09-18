#!/usr/bin/env python3
"""Reference harness integration (API-009) — mechanical host bookkeeping.

Demonstrates the complete lifecycle a harness must own, against `vegapunk
serve` via the thin Python SDK:

    1. task-state handling   — working-state slot restore/put (CAS)
    2. event submission      — durable writes via apply_ops/remember
    3. context proposal      — prepare_context returns blocks + manifest
    4. host selection        — the HARNESS decides placement (subset here)
    5. insertion ack         — record_insertion binds what was *actually*
                               placed; unknown blocks are rejected
    6. outcome               — attributed helped/harmed with evaluator kind

Correlation ids (trace_id, proposal_id) come from Vegapunk — the model never
invents them, and insertion claims are verified, not trusted.

Run:  python3 examples/reference-harness/harness.py [path/to/vegapunk]
      (spawns `vegapunk serve` on a temp store; exits nonzero on failure)
"""

import os
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

sys.path.insert(
    0, os.path.join(os.path.dirname(__file__), "..", "..", "sdk", "python")
)
from nomiso_vegapunk import VegapunkClient  # noqa: E402

BIN = sys.argv[1] if len(sys.argv) > 1 else "target/debug/vegapunk"
PORT = 19292
SCOPE = "org/refharness"


def wait_health(url: str, proc: subprocess.Popen, deadline: float = 20.0) -> None:
    t0 = time.time()
    while time.time() - t0 < deadline:
        if proc.poll() is not None:
            raise RuntimeError(f"server exited: {proc.returncode}")
        try:
            urllib.request.urlopen(f"{url}/health", timeout=1.0)
            return
        except (urllib.error.URLError, OSError):
            time.sleep(0.15)
    raise RuntimeError("no /health within deadline")


def main() -> int:
    with tempfile.TemporaryDirectory() as td:
        cfg = os.path.join(td, "vegapunk.toml")
        with open(cfg, "w") as f:
            f.write(
                'endpoint = "rocksdb://./data"\n'
                'path_base = "config"\n'
                'embed_dim = 8\n'
                f'default_scope = "{SCOPE}"\n'
            )
        env = dict(os.environ)
        env["VEGAPUNK_CONFIG"] = cfg
        url = f"http://127.0.0.1:{PORT}"
        proc = subprocess.Popen(
            [BIN, "serve", "--bind", f"127.0.0.1:{PORT}"],
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
        )
        try:
            wait_health(url, proc)
            vp = VegapunkClient(url, timeout=10.0)
            print("[harness] server healthy")

            # 1. Task-state: restore (none yet) then put initial slot.
            state = vp.working_state(SCOPE)
            assert state["state"] is None
            vp.working_state(
                SCOPE,
                {"goal": "demo harness loop", "file": "harness.py"},
            )
            print("[harness] working-state slot created")

            # 2. Event submission: durable memory writes.
            vp.apply_ops(
                SCOPE,
                [
                    {
                        "op": "put",
                        "scope": SCOPE,
                        "text": "the demo harness uses rocksdb at ./data",
                        "category": "semantic",
                    },
                    {
                        "op": "put",
                        "scope": SCOPE,
                        "text": "release checklist lives in docs/release-matrix.md",
                        "category": "semantic",
                    },
                ],
            )
            print("[harness] events submitted")

            # 3. Context proposal — typed request, bounded budget.
            proposal = vp.prepare_context(
                scope=SCOPE,
                task="where does the release checklist live",
                budget={"max_tokens": 600, "max_blocks": 4,
                        "max_candidates": 8,
                        "token_method": {"approx_chars_per_token":
                                         {"chars_per_token": 4}}},
                request_id="demo-req-1",
            )
            print(f"[harness] proposal {proposal['proposal_id'][:12]}… "
                  f"status={proposal['status']} "
                  f"blocks={len(proposal['blocks'])}")

            # 4. HOST selection — the model/harness chooses a subset.
            selected = proposal["blocks"][:1]
            #    (placement into the model prompt happens host-side here)

            # 5. Actual-insertion ack — verified against the proposal.
            rec = vp.record_insertion(
                trace_id=proposal["trace_id"],
                proposal_id=proposal["proposal_id"],
                scope=SCOPE,
                host="reference-harness",
                inserted=[{"block_id": b["block_id"]} for b in selected],
            )
            assert rec["replayed"] is False
            print(f"[harness] insertion acked: {len(rec['inserted'])} block(s)")

            # 6. Attributed outcome — observed execution, not a guess.
            vp.trace_outcome(
                SCOPE,
                proposal["trace_id"],
                "helped",
                evaluator="execution",
                note="demo run found the checklist fact",
            )
            print("[harness] outcome recorded")

            # Resume path: working-state roundtrip persists for next run.
            state = vp.working_state(SCOPE)
            assert state["state"]["body"]["goal"] == "demo harness loop"
            print("[harness] working-state restored on resume")
            print("reference harness loop: OK")
            return 0
        finally:
            proc.terminate()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()


if __name__ == "__main__":
    sys.exit(main())
