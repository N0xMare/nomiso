# Offline evals (Phase C0)

Deterministic skill / dual-plane fixtures. **No live model calls** in `just check`.

| Track | What it scores | Where |
|---|---|---|
| **Plane** | Hybrid BM25+vector, scopes, validation | `crates/nomiso-eval` |
| **Plane coding v2** | Coding-agent scenarios P1–P20 + cost columns | `evals/coding_agent/` · `just eval-coding-plane` |
| **Skill C0** | Writer ops parse + RuleWriter + hard_recall multipass | this dir + `vegapunk::run_skill_suite` |
| **Skill live** | Real Grok/Codex CLI extract (manual / maintainer) | `vegapunk write-episode --writer grok` · `just eval-coding-skill-live` |
| **Skill coding pack** | Gold apply + RuleWriter vs `gold_ops`; live Grok report-only; Hindsight retain skip-honest | `just eval-coding-skill` · `vegapunk eval --coding` · `--live` / `--live-dir` |
| **Hindsight retain chat model** | Pinned via `HINDSIGHT_API_LLM_MODEL` in `.env` (Hindsight retain only — not embeddings, not the plane foil). Use one explicit model id, never `openrouter/free` or `auto`. | `scripts/hindsight.sh`, `.env.example` |

## Source of truth

**Rust built-in** `vegapunk::skill_suite_c0()` is the `just check` source of truth.  
`evals/skill_c0_suite.json` is a loadable mirror (exact case-name equality checked in tests).  
`write_cases.jsonl` is an optional partial extract for tooling experiments.

```bash
cargo test -p vegapunk skill_c0_suite_passes
cargo run -p vegapunk-cli -- eval
cargo run -p vegapunk-cli -- eval --suite evals/skill_c0_suite.json
```

## Fixture files

| File | Format |
|---|---|
| `skill_c0_suite.json` | Full `SkillEvalSuite` (write + hard_recall cases) |
| `write_cases.jsonl` | One `WriteCase` JSON object per line (optional) |

## Semantics note

- **C0 `ops_json` parse failure** with `expect_ops_parse: false` → case **passes** (negative parse probe).
- **Production `CliChatWriter`** fail-closed: unparseable model text → `WriterOp::Noop` (never invents puts). Scored via `use_cli_writer_mock` cases.

## Embedder labels (do not blend)

| Label | Used in | Notes |
|---|---|---|
| **hashing** | `just check`, C0, `HashingEmbedder` | Deterministic lexical proxy |
| **http** | Optional agent path (`embed_url`) | Real paraphrase; needs API key; not in `just check` |
| **none** | `--no-hash-embed` | BM25 only |

Maintainer live probe (skips with no env):

```bash
export VEGAPUNK_EMBED_URL=https://api.openai.com/v1
export VEGAPUNK_EMBED_API_KEY=…
export VEGAPUNK_EMBED_MODEL=text-embedding-3-small   # optional
export VEGAPUNK_EMBED_DIM=1536                       # optional
just eval-embed-http
```

Never publish one number that mixes hashing and HTTP.

## Graph expansion evals

Two preregistered workloads in `nomiso-eval`:

- **Synthetic fixture** `graph_expand_v1` — 17 authored docs, gold reachable only via edges. Proves the mechanism.
- **Real corpus** `graph_expand_corpus` — this repo's `docs/spec/*.md` split into ~173 section docs; edges mined organically from `XX-NNN` requirement references (`derived_from` to the defining section) and `](NN.md)` doc links (`depends_on` to the target preamble). Probes are requirement-style queries; gold = sections reached by ≥1 edge hop from a lexical-anchor section, split into *graph-only* (no lexical overlap with the query) and *lexical-baseline* classes. Assertions frozen before the HTTP row ran.

Measured rows (same 1200-char corpus, k=6, `just` in-process store):

| Row | graph-only recall | lexical recall | noise Δ vs direct | mean latency |
|---|---|---|---|---|
| hashing dim-8 | direct/hybrid/iterative ≈ 0 → **expanded 0.89** | expanded 0.0 (inherits hybrid collapse) | +7 slots | 232ms vs 92ms direct |
| http `bge-small` 384 (Ollama) | direct/hybrid/iterative 0 → **expanded 1.0** | all 1.0 | +3 slots | 388ms vs 131ms direct |

Findings: (1) expansion surfaces edge-reachable content nothing else reaches — consistent across embedders. (2) `expanded` seeds from the hybrid channel, so a degenerate embedder (hashing dim-8) collapses it on lexical probes; a real embedder removes the degradation entirely. (3) Candidate-cap truncation fired on dense neighborhoods and was correctly reported — budgets work as designed. Decision: `graph_expand` stays opt-in API default (real ~3× latency cost, wins only on graph-shaped queries), but is now evidence-backed for use when semantic embeddings are configured — not fixture-only.

Run: `cargo test -p nomiso-eval corpus_expansion` (hashing row); the labeled HTTP row needs `--features http` plus `VEGAPUNK_EMBED_URL/KEY/MODEL/DIM` (Ollama recipe above).

Side-by-side plane comparison (Nomiso + optional Hindsight/Mem0/Zep): `just eval-compare`. Local Hindsight (no API key): `just hindsight-up` then `just eval-compare-hindsight`. See [docs/spec/13-verification-and-evaluation.md](../docs/spec/13-verification-and-evaluation.md).

## Citation-grounded head-to-head (G22)

`nomiso-eval::h2h` puts an answering agent (grok CLI, headless) on top of two
retrieval channels over the frozen coding corpus. The agent sees only the
evidence blocks each system presents and must declare `USED: [ids]`; the
scorer verifies the declared ids against the *actually inserted* set
(Nomiso: the `record_insertion` ack — fabrication is structurally
impossible; foreign channels: the retrieved set — the asymmetry is labeled)
and checks gold assertion against the used blocks' text.

Preregistration (incl. documented amendments): [h2h-preregistration.md](./h2h-preregistration.md).

| Row | Nomiso embedder | Hindsight retain | Answering |
|---|---|---|---|
| `h2h-hash` | hashing-8 (offline, lexical-only) | chunks (plane-equivalent) | grok-4.6 low |
| `h2h-http` | Ollama bge-small 384 | chunks | grok-4.6 low |
| `h2h-llm` | Ollama bge-small 384 | container default (LLM extraction) | grok-4.6 low |

Run: `just eval-h2h` / `just eval-h2h-http` / `just eval-h2h-llm`.
Skip-honest: no `EVAL_H2H_LIVE` → skip; hindsight down → channel skipped;
grok absent → rows record `answerer:` errors. Hashing is a lexical proxy —
hashing-vs-semantic rows do not establish semantic superiority (spec 13).

### Measured rows (2026-09-18, this machine)

30 scored probes × 2 channels; skips are declared per-row (temporal/multi-hop
probes, one grok timeout). Machine reports: `h2h-hash.json`, `h2h-http.json`,
`h2h-llm.json`.

| Row | system | scored | fact_hit | leak | fraud | forbidden | est_tokens | search p50 ms |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| hash | nomiso-t4 | 29 | 25 | 0 | 0 | 1 | 3243 | 19.3 |
| hash | hindsight-chunks | 30 | 26 | 0 | 0 | 0 | 3873 | 344.9 |
| http | nomiso-t4 | 29 | 26 | 0 | 0 | 0 | 3744 | 43.8 |
| http | hindsight-chunks | 29 | 25 | 0 | 0 | 0 | 3742 | 344.9 |
| llm  | nomiso-t4 | 30 | 27 | 0 | 0 | 0 | 3921 | 43.7 |
| llm  | hindsight-default | 30 | 27 | 0 | 0 | 1 | 3873 | 344.8 |

Findings (exactly what the rows support — nothing broader):

- **Grounded parity, not superiority.** fact_hit counts are within ~1–2
  probes of each other on every row; the corpus is small and own-authored —
  this does not establish semantic advantage over Hindsight in general.
- **Zero citation fraud and zero unsupported-gold leaks on both channels**
  in these runs. For `nomiso-t4` fraud is additionally structural: declared
  ids verify against the `record_insertion` ack, not just the retrieval set.
- **Latency**: the nomiso-t4 channel (in-process store + prepare_context +
  ack) ran p50 ≈ 19–44ms vs ≈345ms for the Hindsight recall channel in this
  local container setup. These are different deployment shapes (in-process
  vs containerized service with local reranker) — the comparison is
  "channel latency as measured", not an architectural speed claim.
- **Evidence cost**: similar block/token volume both channels.
- **Scope-isolation abstains**: on the llm row `nomiso-t4` passed all 30
  scored rows (27 hits + 3 correct abstentions) while `hindsight-default`
  asserted the off-scope TACT fact on `p07b` (forbidden=1). The same probe
  also flagged hindsight-chunks in an earlier amended-scorer run, and
  nomiso-t4 flagged it once on the hash row — the answerer is
  nondeterministic, so single-row counts are indicative, not verdicts.
- The `p20`/`p10` misses are answerer-strictness misses: correct blocks
  were cited but the answer text didn't assert the gold fact verbatim.
- Hindsight `default` (LLM extraction) vs `chunks` produced identical
  fact_hit counts here — on this small corpus the extra extraction pass
  showed no measured quality gain.

## Layering

- Plane scores stay in `nomiso-eval` (no vegapunk dep).
- Skill scores use Vegapunk APIs (apply/extract) plus the shared coding pack types in `nomiso-eval`. Models attach via BYOM traits. Hindsight retain compare stays in `nomiso-eval` (HTTP foil).
- Never blend plane + skill + system into one number.
