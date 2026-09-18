# Plane compare miss table (2026-08-15)

**Track:** plane · **Ingest:** gold_put  
**Hindsight:** Docker full image, `LLM_PROVIDER=none`, per-scope banks, chunks retain.

Scoreboard does **not** bind graph, HyDE, or a cross-encoder. `multi_hop` is an explicit skip.

## Hashing (lexical; not comparable on paraphrase/hybrid)

**Embedder:** hashing · dim 32

| Tag | Nomiso | Hindsight | Notes |
|---|---|---|---|
| exact | hit | hit | Fair lexical |
| knowledge_update | hit | hit | Nomiso valid-now; Hindsight `document_id` replace |
| abstain | hit | hit | Empty bank for unused scope |
| scope_isolation | hit | hit | One Hindsight bank per scope |
| temporal | hit (2/2) | skip | Foreign ingest has no Nomiso `as_of` |
| paraphrase | hit (hashing) | skip | Hashing ≠ Hindsight semantics |
| hybrid | hit (hashing) | skip | Same |
| multi_hop | skip | skip | No graph writer |

## HTTP local BGE (fair semantic control)

**Embedder:** `http` · Ollama `qllama/bge-small-en-v1.5` · dim **384**  
**Runtime:** `http://127.0.0.1:11434/v1` · same family as Hindsight default `BAAI/bge-small-en-v1.5` (not bit-identical weights/pooling).  
**Command:** `just eval-semantic-ollama`  
**Nomiso `passed=true`.**

| Tag | Nomiso | Hindsight | Notes |
|---|---|---|---|
| exact | hit | hit | Fair |
| paraphrase | hit | hit | First real semantic head-to-head |
| hybrid | hit | hit | Same |
| knowledge_update | hit | hit | Valid-now vs replace |
| abstain | hit | hit | |
| scope_isolation | hit | hit | |
| temporal | hit (2/2) | skip | Still no Hindsight `as_of` intervals |
| multi_hop | skip | skip | Scoreboard still does not bind graph |

**Read:** on this suite, Nomiso hybrid + local BGE **ties** Hindsight on every tag that both systems score. That does **not** authorize a graph writer, CE, or HyDE. Next quality is daily Vegapunk use and (optional) a second labeled stronger embedder — not more retrieval channels.

## Coding-agent pack `sota_coding_v2` (2026-08-18)

**Track:** plane · **Ingest:** gold_put · **Source:** `evals/coding_agent/scenarios/` (25 files, P1–P20)  
**Command:** `just eval-coding-plane` · HTTP: `just eval-coding-plane-http` with Ollama BGE-384 + `EVAL_HINDSIGHT_URL`  
**Cost (HTTP, this machine):** Nomiso ingest ~1.1s · search p50 ~76ms · p95 ~529ms (Hindsight CE tails the p95).

| Tag | Nomiso HTTP | Hindsight chunks+CE | Notes |
|---|---|---|---|
| exact | 15/15 | 15/15 | Fair |
| paraphrase | 3/3 | 3/3 | After tightening P14 query; first draft missed on Nomiso RRF |
| hybrid | 5/5 | 5/5 | Fair |
| knowledge_update | 2/2 | 2/2 | Needs `replace_group` so Hindsight shares `document_id` |
| abstain | 1/1 | 1/1 | Empty unused scope |
| scope_isolation | 3/3 | 3/3 | Per-scope Hindsight banks |
| uncertainty | 1/1 | skip | Nomiso category; foreign skip |
| temporal | 6/6 | skip | `as_of` / `known_as_of` |
| multi_hop | skip | skip | No graph writer |

P12/P13/P16/P18 are **facts about** compaction/WM/sessions/citation. Skill track still owns those verbs. No graph/CE/HyDE bound.

## Coding skill `sota_coding_v2` (2026-08-18)

**Track:** skill · **Commands:** `just eval-coding-skill` · live extract `just eval-coding-skill-live` · re-score `just eval-coding-skill-live-dir`  
**`passed` on extract rows** = no invented `prior_id`. **P/R is extract quality.** Do not quote “25/25 passed” as a quality score.

| Extractor | Gate | Result |
|---|---|---|
| **gold_apply** | CI | 25/25 apply (supersede keys remapped). Includes vacuous p19 (`gold_ops: []`). `passed=true` |
| **RuleWriter** | report only | Line-dump baseline. `invented_prior=0`. Not host extract |
| **Hindsight retain** | report | **2026-08-31 dedicated LLM stack** (`nomiso-hindsight-retain` / `hindsight-retain-data`, not the plane foil). Chat: OpenRouter `nvidia/nemotron-3.5-lightning:free`. Ingest: **transcript only**. Metric: 6-word gold_memory substring in Hindsight recall blob (not Vegapunk extract P/R). **recall_macro=0.040** (vacuous p11 gold_n=0 is the only 1.00). First suite abort on a single `output_tokens=0` was a harness bug; per-scenario 0-token now continues. Lightning sometimes empty content / 75–400s retain. Do **not** average with Grok P/R or plane 5/5. |
| **Live host** | report only | Same 25 Grok dumps (no second host run) · `invented_prior=0` · **P=0.720 R=1.000** |

### Live Grok CLI (re-score after transcript-faithful gold)

Skill `gold_ops` is transcript-supported; plane `gold_memories` may keep retrieval wording (`TACT_ROOT_ONLY_SLOT`, `FsBlobStore`, sidecar token).

| Bucket | Scenarios | Read |
|---|---|---|
| P=R=1 | p02, p04b, p07, p07b, p08b, p09, p10, p11, p15, p17, p18, p19 | Host matched extract gold. p07 is **noop** (secret + foreign scope). p10 `uncertainty_ok=true` |
| R=1, P<1 (split) | p01, p02b, p03, p04, p05, p05b, p06, p08, p12, p13, p14, p16, p20 | Host split one gold fact into extra puts. Precision tax, not a miss |

Recall is 1.0 on this pack. Remaining P tax is split-facts, not paraphrase/category. Do not average with plane hit@k.

Hindsight retain foil **landed** (Lightning, dedicated volume). Do not blend with Grok extract P/R. Plane foil stays `LLM_PROVIDER=none` on a different volume.

**Speed smokes (2026-09-01, p01 only, `EVAL_HINDSIGHT_RETAIN_LIMIT=1`):** Super retain **15.2s** (still 0 on 6-word gold hit). Laguna S **429** upstream shared pool (skip-honest). Lightning full pack remains the labeled 25-scenario row (~40 min). Same-LLM Vegapunk extract is **not wired** (CliChatWriter is Grok/Codex CLI only).

**Super full pack (2026-09-01, `EVAL_HINDSIGHT_BANK=nomiso-coding-skill-super-v2`):** wall **539s**. `recall_macro=0.000` on the 6-word gold_memory metric. Nvidia `:free` **temporarily overloaded** on ~14/25 retains (per-scenario, suite did not abort). ~11 STREAMING RETAIN COMPLETE. First v1 attempt aborted on the first 500 before the harness change. Faster than Lightning (~40 min) but not a clean 25/25 extract. Do **not** blend with Grok P/R or plane 5/5.

## Coding system `sota_coding_v2` (2026-08-27)

**Track:** system · **Host:** Grok CLI (`grok -p`) · **Embedder:** hashing · dim 32  
**Ingest:** Vegapunk CLI `encode` of valid-now gold (closed `valid_until` skipped; plane owns `as_of`)  
**Command:** `just eval-coding-system-live` · dumps gitignored under `evals/coding_agent/system/`  
**Discipline:** `recall_skip` is the vegapunk shim log (`hard-recall`), not host `recalled: true`. Abstain requires a logged recall.

`passed` on the report is runner-ok, not quality. Do not blend with plane hit@k or skill P/R.

| Count | Value |
|---|---|
| scored | 30 |
| graph / temporal skip | 7 (6 `as_of`/`known_as_of` + p19 `multi_hop`) |
| recall_skip | 0 |
| fact_hit | 29 / 30 |
| leak | 0 |

| Tag | fact_hit | Notes |
|---|---|---|
| exact | 15/15 | Host hard-recalled `--pack` |
| paraphrase | 3/3 | Hashing still lexical; host pack+answer contained gold |
| hybrid | 4/5 | **Miss:** `p17_hybrid_inject_doctrine` missing `p17_no_autodump` |
| knowledge_update | 2/2 | Valid-now v2 encoded; closed v1 not ingested |
| scope_isolation | 3/3 | |
| uncertainty | 1/1 | |
| abstain | 1/1 | Logged hard-recall; empty pack → ABSTAIN |
| temporal | skip 6 | Plane-owned intervals |
| multi_hop | skip 1 | No graph writer |

**Sole scored miss:** `p17_near_dup` / `p17_hybrid_inject_doctrine`. Query `auto-dump pack.block soft-inject` retrieved `p17_pack_only` (soft-inject / pack.block) and not `p17_no_autodump` (“Never auto-dump…”). Hashing hybrid is not comparable to Hindsight semantics. Plane HTTP on this pack is already hybrid 5/5. This miss does **not** bind CE, graph, or HyDE.

Scorer hygiene landed after this run (shim log only; sibling queries consume one line each via full `--query` match). p07 dump already had two distinct query lines, so recall_skip=0 still holds under the stricter matcher. Isolation unset of clap `VEGAPUNK_ENDPOINT` / embed env on encode+grok also landed after; a second live host run is not required to publish this hashing row.

**Read:** Grok CLI used Layer 2 (`hard-recall --pack`) with recall_skip=0, so Layer 1 standing header stays off. HTTP system row is below. Hindsight retain remains skip-honest (no working chat key).

## Coding system HTTP `sota_coding_v2` (2026-08-27)

**Track:** system · **Host:** Grok CLI (`grok -p`) · **Embedder:** `http` · Ollama `qllama/bge-small-en-v1.5` · dim **384**  
**Command:** `just eval-coding-system-live-http` · dumps gitignored under `evals/coding_agent/system-http/`  
**Ingest:** same valid-now gold encode as the hashing row (isolated tmp rocks; clap env unset on children)

| Count | Value |
|---|---|
| scored | 30 |
| graph / temporal skip | 7 |
| recall_skip | 0 |
| fact_hit | 29 / 30 |
| leak | 0 |

Tag splits match the hashing row (exact 15/15, paraphrase 3/3, hybrid **4/5**, KU 2/2, scope 3/3, uncertainty 1/1, abstain 1/1).

**Same sole miss:** `p17_hybrid_inject_doctrine` missing `p17_no_autodump`. HTTP dump pack_preview top card is `p17_pack_only` (“Soft-inject stays off…”); autodump is not in the first 500 chars; host answer restated pack_only only. Plane HTTP on this pack is already hybrid 5/5 (full top-k, not a 500-char preview). This is a **system host/preview** miss, not a plane hybrid miss, and it still does **not** bind CE, graph, or HyDE.

**Read:** Switching hashing → BGE-384 did not move the system scoreboard. Layer 1 stays off (`recall_skip=0`). Next quality is daily dogfood, not a new retrieval channel.

**Scorer honesty (landed, 2026-08-31):** `fact_hit` uses full vegapunk `pack.block` (shim-captured stdout) plus the host answer, not the 500-char JSON `pack_preview`. Gold is unchanged. **2026-08-27 rows above are preview-era.** HTTP live re-score is below. Hashing 29/30 is still preview-era (no hashing dumps on this host). This still does **not** bind CE, graph, or HyDE.

**Dogfood (same day):** `just dogfood-coding` OK — Ollama BGE-384, standing locks encoded, Layer 2 pack hit clippy + Surreal, WM slot set, nothing auto-injected. `just dogfood-pi` layout OK; live `pi -p` skip-honest (`Connection error` / pack not in output).

## Coding system HTTP `sota_coding_v2` (2026-08-31, full pack.block)

**Track:** system · **Host:** Grok CLI `0.2.82` (`grok -p`) · **Embedder:** `http` · Ollama `qllama/bge-small-en-v1.5` · dim **384**  
**Command:** `just eval-coding-system-live-http` (~25 min) · dumps gitignored under `evals/coding_agent/system-http/` (`pack_log` present)  
**Scorer:** `fact_hit` = full vegapunk `pack.block` + answer. `leak` = forbidden gold *sentence* in blob (not 50% bag-of-words). Gold unchanged.

| Count | Value |
|---|---|
| scored | 30 |
| graph / temporal skip | 7 |
| recall_skip | 0 |
| fact_hit | **30 / 30** |
| leak | 0 |

| Tag | fact_hit | Notes |
|---|---|---|
| exact | 15/15 | |
| paraphrase | 3/3 | |
| hybrid | **5/5** | **p17_hybrid_inject_doctrine now hits** — autodump was in full pack.block |
| knowledge_update | 2/2 | |
| scope_isolation | 3/3 | p07b host ABSTAIN; `TACT_ROOT_ONLY_SLOT` absent from pack |
| uncertainty | 1/1 | |
| abstain | 1/1 | Logged hard-recall |
| temporal | skip 6 | Plane-owned |
| multi_hop | skip 1 | No graph writer |

Live runner first pass: fact_hit 30/30, leak=1 on `p07b_no_tact_leak`. Dump showed host **ABSTAIN**, forbidden sentence absent; 50% token overlap on a long pack (Nomiso/eval/coding/secrets) was a false leak. Leak matcher is now sentence containment. Dump-dir re-score of the same HTTP dumps: leak=0, fact_hit=30/30. Skip count on dump-dir is lower because skip-only scenarios have no dump files; published skip=7 is the live walk.

**Read:** p17 was a preview-cut miss, not plane hybrid. Full-pack HTTP system **ties** plane HTTP hybrid 5/5 on this pack. Layer 1 stays off (`recall_skip=0`). Does **not** bind CE, graph, or HyDE. Next quality remains daily dogfood.
