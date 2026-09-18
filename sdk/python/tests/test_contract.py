#!/usr/bin/env python3
"""End-to-end contract test for the thin Python SDK.

Spawns `vegapunk serve` against a temp embedded-RocksDB store and exercises
the real HTTP contract: remember → prepare_context → record_insertion, plus
typed-error behavior. Run directly or via `just sdk-check`.

Usage: python3 sdk/python/tests/test_contract.py [path/to/vegapunk]
"""

import json
import os
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from nomiso_vegapunk import NomisoError, VegapunkClient  # noqa: E402

BIN = sys.argv[1] if len(sys.argv) > 1 else "target/debug/vegapunk"
PORT = 19191


def wait_health(url: str, proc: subprocess.Popen, deadline: float = 20.0) -> None:
    t0 = time.time()
    while time.time() - t0 < deadline:
        if proc.poll() is not None:
            raise RuntimeError(f"server exited early: {proc.returncode}")
        try:
            urllib.request.urlopen(f"{url}/health", timeout=1.0)
            return
        except (urllib.error.URLError, OSError):
            time.sleep(0.15)
    raise RuntimeError("server did not become healthy")


def main() -> int:
    with tempfile.TemporaryDirectory() as td:
        cfg = os.path.join(td, "vegapunk.toml")
        with open(cfg, "w") as f:
            f.write(
                'endpoint = "rocksdb://./data"\n'
                'path_base = "config"\n'
                'embed_dim = 8\n'
                'default_scope = "org/sdk"\n'
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

            # Write path.
            r = vp.remember(
                scope="org/sdk",
                text="python sdk contract fact",
                category="semantic",
                idempotency_key="sdk-test-1",
            )
            assert r is not None

            # T4 controller loop: proposal → host insertion → ack.
            p = vp.prepare_context(
                scope="org/sdk", task="sdk contract", request_id="sdk-req-1"
            )
            assert p["status"] in ("ready", "partial"), p
            assert p["proposal_id"] and p["trace_id"]
            blocks = p["blocks"]
            assert blocks, p
            bid = blocks[0]["block_id"]
            rendered = p["rendered"]
            assert blocks[0]["excerpt"] in rendered  # manifest fidelity

            rec = vp.record_insertion(
                trace_id=p["trace_id"],
                proposal_id=p["proposal_id"],
                scope="org/sdk",
                host="python-sdk-test",
                inserted=[{"block_id": bid, "truncated": False}],
            )
            assert rec["inserted"] == [bid] and rec["replayed"] is False
            # Idempotent replay.
            rec2 = vp.record_insertion(
                trace_id=p["trace_id"],
                proposal_id=p["proposal_id"],
                scope="org/sdk",
                host="python-sdk-test",
                inserted=[{"block_id": bid, "truncated": False}],
            )
            assert rec2["replayed"] is True

            # Typed errors: fabricated block id → invalid_request.
            try:
                vp.record_insertion(
                    trace_id=p["trace_id"],
                    proposal_id=p["proposal_id"],
                    scope="org/sdk",
                    host="python-sdk-test",
                    inserted=[{"block_id": "fabricated"}],
                )
                raise AssertionError("fabricated insertion was accepted")
            except NomisoError as e:
                assert e.code in ("invalid_request", "idempotency_conflict"), e.code
                assert e.status in (400, 409), e.status

            # Unknown route/method surface is a typed error, not a crash.
            try:
                vp.read(scope="!!bad scope", ids=["memory:x"])
                raise AssertionError("bad scope accepted")
            except NomisoError as e:
                assert e.code == "invalid_request", e.code

            print("python SDK contract: OK")
            return 0
        finally:
            proc.terminate()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()


if __name__ == "__main__":
    sys.exit(main())
