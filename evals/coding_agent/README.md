# Coding-agent memory pack (`sota_coding_v2`)

Pattern-constrained scenarios for long-horizon **coding agents**. Not LongMemEval. Not Alice-at-Google chat.

| Path | What |
|---|---|
| [SCHEMA.md](./SCHEMA.md) | Envelope + validation rules |
| `scenarios/*.json` | One scenario per file |
| `nomiso-eval::coding_agent_suite()` | Compile → plane suite |

```bash
cargo test -p nomiso-eval coding_pack_validates_and_compiles
cargo test -p nomiso-eval coding_agent_plane_v2 -- --nocapture
just eval-coding-plane
just eval-coding-skill          # gold apply (gate) + RuleWriter report + Hindsight retain skip
just eval-coding-skill-live     # Grok CLI extract (writes evals/coding_agent/live/, gitignored)
just eval-coding-skill-live-dir # re-score those dumps; no host call
just eval-coding-system         # skip-honest (no host; unsets VEGAPUNK_SYSTEM_LIVE)
just eval-coding-system-live    # Grok CLI must hard-recall --pack (hashing unless EMBED_URL)
just eval-coding-system-live-http # Ollama BGE-384 labeled http (skip if Ollama down)
just eval-coding-system-live-dir  # re-score dumps; no host (needs pack_log for full pack.block)
```

System dumps (both gitignored): `evals/coding_agent/system/` (hashing) and `evals/coding_agent/system-http/` (HTTP BGE). Preview-era 2026-08-27: fact_hit 29/30 (p17 preview). **HTTP BGE full-pack 2026-08-31:** fact_hit 30/30, leak=0, hybrid 5/5. See `evals/compare/last-miss.md`.

HTTP foil (skip-honest without a key): `just eval-coding-plane-http`.

Patterns P1–P18 scored; P19 `multi_hop` skip; P20 identity light. See the historical protocol and target evaluation requirements in `docs/spec/13-verification-and-evaluation.md`.
