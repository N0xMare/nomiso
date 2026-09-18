# nomiso-eval

Plane-level evaluation harness: fixtures, hybrid hit@k, and a **tagged channel scoreboard**.

No LLM required. Used for Phase A/B regression and the Hindsight-or-better plane track.

| Entry | What |
|---|---|
| `hybrid_suite` / `run_suite` | Legacy hashing hybrid probes (`just check`) |
| `channel_suite` / `run_channel_scoreboard` | Tagged `exact`/`paraphrase`/`hybrid`/`temporal`/`knowledge_update`/`abstain`/`scope_isolation`; `multi_hop` skip; optional Hindsight/Mem0/Zep |
| `coding_agent_suite` | Coding-agent pack `sota_coding_v2` (`evals/coding_agent/scenarios`); cost columns on the report |
| `run_hindsight_retain_skill` | Skill-track Hindsight retain (skip-honest; not plane hit@k) |
| `run_labeled_http_paraphrase` | Real-embed probe (`--features http`, env-gated) |

Hashing scores are **not** comparable to Hindsight semantics. `knowledge_update` is valid-now after interval ingest (Nomiso) vs `document_id` replace (Hindsight). Protocol: [docs/spec/13-verification-and-evaluation.md](../../docs/spec/13-verification-and-evaluation.md).
