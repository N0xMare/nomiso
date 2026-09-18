//! Citation-grounded head-to-head eval (G22 / TEST-002).
//!
//! The existing system track scores `fact_hit` as "pack text + answer text vs
//! gold" — overlap does not prove the answer was *grounded* in the evidence.
//! This harness presents evidence blocks to an answering agent, requires it to
//! declare which blocks it used (`USED: [ids]` or `ABSTAIN`), and scores
//! mechanically:
//!
//! - `fact_hit`: answer asserts a gold fact AND at least one *declared-used*
//!   block's text supports that fact — grounded, not lucky.
//! - `leak`: answer asserts a gold fact but no declared-used block supports it
//!   — parametric recall or unsupported claim.
//! - `citation_fraud`: declared a block id that was never presented (for the
//!   Nomiso channel "presented" is exactly the verified `record_insertion`
//!   subset — a hallucinated citation is unrepresentable downstream).
//! - `abstain_ok`: abstain probes must abstain without asserting gold.
//! - `forbidden_asserted`: answer asserts a `must_not_include` fact without
//!   asserting gold (stale claim / cross-scope leak) — conservative by
//!   design since near-duplicate supersede texts can't be separated by
//!   lexical match.
//!
//! Channels: `NomisoT4Channel` exercises the real T4 loop (prepare_context →
//! host subset-insert → record_insertion); `HindsightChannel` reuses the
//! compare adapter (presented = recalled items — foreign systems have no
//! insertion-ack protocol, an honest asymmetry the report names).
//!
//! Live runs are skip-honest: `EVAL_H2H_LIVE=1` plus a host binary
//! (`EVAL_H2H_GROK`, default `grok`). Deterministic coverage uses a scripted
//! `MockAnswerer` — the Nomiso T4 path itself runs for real in tests.

use async_trait::async_trait;
use nomiso_core::ops::PutRequest;
use nomiso_core::types::{Category, Content, Provenance};
use nomiso_embed::HashingEmbedder;
use nomiso_memory::{
    prepare_context, record_insertion, ContextBlock, InsertedBlock, InsertionAck,
    PrepareContextRequest,
};
use nomiso_service::NomisoClient;
use nomiso_store::StoreConfig;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::time::{Duration, Instant};

use crate::compare::{CompareSearch, HindsightSystem, MemorySystem};
use crate::{CodingScenario, FixtureProbe, ProbeTag};

/// One evidence block shown to the answering agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceBlock {
    pub id: String,
    pub text: String,
}

/// Parsed answering-agent output.
#[derive(Debug, Clone, PartialEq)]
pub struct CitationAnswer {
    /// Answer text with the USED/ABSTAIN marker lines removed.
    pub text: String,
    /// Declared evidence ids the answer relied on.
    pub used: Vec<String>,
    pub abstained: bool,
}

/// A `USED` marker line: `USED:`, `USED [`, `USED[`, bare `USED`, or
/// `USED ` followed only by id-ish characters (a body sentence like
/// "Used the block above." contains `.` and is not a marker).
fn is_used_marker(upper: &str) -> bool {
    if upper == "USED" || upper.starts_with("USED:") || upper.starts_with("USED[") {
        return true;
    }
    upper.strip_prefix("USED ").is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '[' | ']' | ',' | '-' | '_' | ' '))
    })
}

/// Parse `USED: [b1, b2]` / `ABSTAIN` markers tolerantly.
pub fn parse_citation_answer(raw: &str) -> CitationAnswer {
    let mut used = Vec::new();
    let mut abstained = false;
    let mut body = Vec::new();
    for line in raw.lines() {
        let t = line.trim();
        let upper = t.to_ascii_uppercase();
        if is_used_marker(&upper) {
            // Parse only the bracketed list (`[a, b]`); trailing prose after
            // `]` must not mint phantom ids. Without brackets, take the text
            // after `USED:`/`USED ` and split on commas.
            let inner: &str = match (t.find('['), t.rfind(']')) {
                (Some(l), Some(r)) if r > l => &t[l + 1..r],
                _ => t.split_once(':').map(|(_, rest)| rest).unwrap_or_else(|| {
                    t.trim_start_matches(['U', 'u', 'S', 's', 'E', 'e', 'D', 'd'])
                }),
            };
            for tok in inner.split(',') {
                let id = tok
                    .trim()
                    .trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_');
                if !id.is_empty() {
                    used.push(id.to_string());
                }
            }
            continue;
        }
        if upper == "ABSTAIN" || upper.starts_with("ABSTAIN:") {
            abstained = true;
            continue;
        }
        body.push(line);
    }
    CitationAnswer {
        text: body.join("\n").trim().to_string(),
        used,
        abstained,
    }
}

/// What one channel presented for one probe.
pub struct Presented {
    pub blocks: Vec<EvidenceBlock>,
    pub search_ms: f64,
}

/// A memory system under evaluation (retain + present only).
#[async_trait]
pub trait EvidenceChannel: Send + Sync {
    /// Labeled channel name (e.g. `hindsight-chunks`); recorded on every row.
    fn id(&self) -> String;
    /// Load one scenario's gold memories.
    async fn ingest(&mut self, scenario: &CodingScenario) -> Result<(), String>;
    /// Present evidence for a probe.
    async fn present(&mut self, probe: &FixtureProbe) -> Result<Presented, String>;
}

/// Answering agent (host LLM or scripted mock).
#[async_trait]
pub trait Answerer: Send + Sync {
    async fn answer(&self, system: &str, user: &str) -> Result<String, String>;
    fn id(&self) -> String;
}

/// Per-probe row for one system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct H2hRow {
    pub system: String,
    pub scenario: String,
    pub probe: String,
    pub tag: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
    pub fact_hit: bool,
    pub leak: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abstain_ok: Option<bool>,
    /// Declared ids never presented (hallucinated citation).
    pub citation_fraud: Vec<String>,
    /// Answer asserted a `must_not_include` fact (scope-isolation breach).
    #[serde(default)]
    pub forbidden_asserted: bool,
    pub n_presented: u32,
    /// Gold facts supported by at least one declared-used block.
    pub gold_supported: u32,
    pub n_gold: u32,
    /// Estimated tokens presented to the answerer (chars/4).
    pub est_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer_ms: Option<f64>,
    pub passed: bool,
    pub detail: String,
}

/// Aggregate per system.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SystemStats {
    pub scored: u32,
    pub fact_hit: u32,
    pub leak: u32,
    pub abstain_ok: u32,
    pub abstain_miss: u32,
    pub citation_fraud_rows: u32,
    /// Rows where the answer asserted a `must_not_include` fact.
    pub forbidden_asserted: u32,
    pub presented_blocks: u32,
    pub est_tokens: u32,
    pub search_ms_p50: f64,
    pub search_ms_p95: f64,
    pub answer_calls: u32,
}

/// Full head-to-head report. `passed` is runner-health only; quality is counts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct H2hReport {
    pub suite: String,
    pub host: String,
    pub embedder: String,
    pub systems: Vec<String>,
    pub skipped_systems: Vec<(String, String)>,
    pub rows: Vec<H2hRow>,
    pub by_system: BTreeMap<String, SystemStats>,
    pub passed: bool,
}

impl H2hReport {
    pub fn markdown(&self) -> String {
        let mut s = format!(
            "# Head-to-head `{}`\n\nhost={} embedder={} passed={}\n\n",
            self.suite, self.host, self.embedder, self.passed
        );
        s.push_str(
            "| system | scored | fact_hit | leak | abstain_ok | fraud_rows | forbidden | blocks | est_tokens | search p50 ms |\n|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n",
        );
        for (sys, st) in &self.by_system {
            s.push_str(&format!(
                "| {sys} | {} | {} | {} | {} | {} | {} | {} | {} | {:.1} |\n",
                st.scored,
                st.fact_hit,
                st.leak,
                st.abstain_ok,
                st.citation_fraud_rows,
                st.forbidden_asserted,
                st.presented_blocks,
                st.est_tokens,
                st.search_ms_p50
            ));
        }
        for (sys, why) in &self.skipped_systems {
            s.push_str(&format!("\n- skipped `{sys}`: {why}\n"));
        }
        s.push_str("\n## Misses\n\n");
        for r in &self.rows {
            if r.skipped.is_none() && !r.passed {
                s.push_str(&format!(
                    "- `{}`/{}/{} tag={} hit={} leak={} fraud={:?} detail={}\n",
                    r.system,
                    r.scenario,
                    r.probe,
                    r.tag,
                    r.fact_hit,
                    r.leak,
                    r.citation_fraud,
                    r.detail
                ));
            }
        }
        s
    }
}

// --- scoring ---------------------------------------------------------------

fn normalize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || c.is_whitespace() {
                c
            } else {
                ' '
            }
        })
        .flat_map(|c| c.to_lowercase())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Same gold-match convention as the coding skill track: normalized
/// containment or ≥50% token overlap of the shorter token set.
fn texts_match(gold: &str, pred: &str) -> bool {
    if gold.is_empty() || pred.is_empty() {
        return false;
    }
    let a = normalize(gold);
    let b = normalize(pred);
    if a.contains(&b) || b.contains(&a) {
        return true;
    }
    let ta: HashSet<&str> = a.split_whitespace().filter(|w| w.len() > 3).collect();
    let tb: HashSet<&str> = b.split_whitespace().filter(|w| w.len() > 3).collect();
    if ta.is_empty() || tb.is_empty() {
        return false;
    }
    let inter = ta.intersection(&tb).count();
    inter * 2 >= ta.len().min(tb.len())
}

/// Inputs for one scored probe (borrowed).
pub struct CitationInput<'a> {
    pub system: &'a str,
    pub scenario: &'a str,
    /// Gold fact texts behind `expect_keys_in_top`.
    pub gold_texts: &'a [String],
    /// Forbidden fact texts behind `must_not_include_keys` (scope isolation).
    pub forbidden_texts: &'a [String],
    pub presented: &'a [EvidenceBlock],
    pub answer: &'a CitationAnswer,
    pub search_ms: Option<f64>,
    pub answer_ms: Option<f64>,
}

/// Score one probe's presented evidence + declared answer. Gold texts are the
/// fixture memory texts behind `expect_keys_in_top`.
pub fn score_citation_row(probe: &FixtureProbe, input: CitationInput<'_>) -> H2hRow {
    let CitationInput {
        system,
        scenario,
        gold_texts,
        forbidden_texts,
        presented,
        answer,
        search_ms,
        answer_ms,
    } = input;
    let presented_ids: HashSet<&str> = presented.iter().map(|b| b.id.as_str()).collect();
    let fraud: Vec<String> = answer
        .used
        .iter()
        .filter(|id| !presented_ids.contains(id.as_str()))
        .cloned()
        .collect();
    let used_blocks: Vec<&EvidenceBlock> = presented
        .iter()
        .filter(|b| answer.used.iter().any(|u| u == &b.id))
        .collect();
    let gold_supported = gold_texts
        .iter()
        .filter(|g| used_blocks.iter().any(|b| texts_match(g, &b.text)))
        .count() as u32;
    let asserts_gold = gold_texts.iter().any(|g| texts_match(g, &answer.text));
    // Conservative: supersede/isolation fixtures pair near-duplicate
    // gold↔forbidden texts (the corrected fact lexically overlaps the stale
    // one), so an assertion that already matches gold is an echo, not a
    // stale claim. Only flag forbidden content asserted *without* the gold
    // — a stale claim or cross-scope leak with no gold cover.
    let forbidden_asserted =
        !asserts_gold && forbidden_texts.iter().any(|f| texts_match(f, &answer.text));

    let fact_hit = !probe.expect_abstain && asserts_gold && gold_supported > 0;
    let leak = !probe.expect_abstain && asserts_gold && gold_supported == 0;
    let abstain_ok = if probe.expect_abstain {
        Some(answer.abstained && !asserts_gold && !forbidden_asserted)
    } else {
        None
    };
    // Keyless negative probe (scope isolation): no gold to assert; the row
    // passes iff the forbidden text is not asserted — abstaining is correct.
    let passed = fraud.is_empty()
        && !forbidden_asserted
        && if probe.expect_abstain {
            abstain_ok.unwrap_or(false)
        } else if gold_texts.is_empty() {
            true
        } else {
            fact_hit
        };
    let detail = format!(
        "asserts_gold={asserts_gold} supported={gold_supported}/{} used={:?} abstain={} forbidden={}",
        gold_texts.len(),
        answer.used,
        answer.abstained,
        forbidden_asserted
    );
    H2hRow {
        system: system.into(),
        scenario: scenario.into(),
        probe: probe.name.clone(),
        tag: probe
            .tag
            .map(|t| format!("{t:?}").to_lowercase())
            .unwrap_or_else(|| "untagged".into()),
        skipped: None,
        fact_hit,
        leak,
        abstain_ok,
        citation_fraud: fraud,
        forbidden_asserted,
        n_presented: presented.len() as u32,
        gold_supported,
        n_gold: gold_texts.len() as u32,
        search_ms,
        answer_ms,
        est_tokens: presented.iter().map(|b| b.text.len() as u32 / 4).sum(),
        passed,
        detail,
    }
}

/// Probes the answer track cannot score (same discipline as the system track).
fn h2h_skip_reason(p: &FixtureProbe) -> Option<String> {
    if p.skip_if_no_graph {
        return Some("no graph writer".into());
    }
    if p.as_of.is_some() || p.known_as_of.is_some() || p.sys_as_of.is_some() {
        return Some("temporal lens is a plane concern".into());
    }
    match p.tag {
        Some(ProbeTag::Temporal) | Some(ProbeTag::MultiHop) => {
            Some("temporal/multi-hop not scored on the answer track".into())
        }
        _ => None,
    }
}

fn gold_texts_for(scenario: &CodingScenario, probe: &FixtureProbe) -> Vec<String> {
    texts_for_keys(scenario, &probe.expect_keys_in_top)
}

fn forbidden_texts_for(scenario: &CodingScenario, probe: &FixtureProbe) -> Vec<String> {
    texts_for_keys(scenario, &probe.must_not_include_keys)
}

fn texts_for_keys(scenario: &CodingScenario, keys: &[String]) -> Vec<String> {
    let want: HashSet<&str> = keys.iter().map(String::as_str).collect();
    scenario
        .gold_memories
        .iter()
        .filter(|m| want.contains(m.key.as_str()))
        .map(|m| m.text.clone())
        .collect()
}

// --- channels ---------------------------------------------------------------

/// Nomiso channel driving the real T4 loop: `prepare_context` → host inserts
/// the full selected subset → `record_insertion` verifies it. Presented
/// blocks are exactly the acknowledged insertions.
pub struct NomisoT4Channel {
    client: NomisoClient,
    embedder: HashingEmbedder,
    client_embeds: bool,
}

impl NomisoT4Channel {
    /// Hashing embedder row (offline).
    pub async fn connect(dim: usize) -> nomiso_core::error::Result<Self> {
        Ok(Self {
            client: NomisoClient::connect(StoreConfig::memory_test(dim)).await?,
            embedder: HashingEmbedder::new(dim)?,
            client_embeds: false,
        })
    }

    /// Host-attached embedder row (e.g. HTTP provider).
    pub async fn connect_with_embedder(
        dim: usize,
        host: std::sync::Arc<dyn nomiso_service::Embedder>,
    ) -> nomiso_core::error::Result<Self> {
        Ok(Self {
            client: NomisoClient::connect(StoreConfig::memory_test(dim))
                .await?
                .with_embedder(host),
            embedder: HashingEmbedder::new(dim)?,
            client_embeds: true,
        })
    }
}

#[async_trait]
impl EvidenceChannel for NomisoT4Channel {
    fn id(&self) -> String {
        "nomiso-t4".into()
    }

    async fn ingest(&mut self, scenario: &CodingScenario) -> Result<(), String> {
        for mem in &scenario.gold_memories {
            let category = mem
                .category
                .as_deref()
                .and_then(Category::parse)
                .unwrap_or(Category::Semantic);
            let embedding = if !self.client_embeds && mem.with_embedding {
                Some(self.embedder.embed_one(&mem.text))
            } else {
                None
            };
            let embedding_identity = if embedding.is_some() {
                nomiso_service::Embedder::identity(&self.embedder)
            } else {
                None
            };
            self.client
                .put(PutRequest {
                    scope: mem.scope.clone(),
                    category,
                    content: Content::text(mem.text.clone()),
                    valid_from: mem
                        .valid_from
                        .as_deref()
                        .map(|s| s.parse())
                        .transpose()
                        .map_err(|e| format!("valid_from: {e}"))?,
                    valid_until: mem
                        .valid_until
                        .as_deref()
                        .map(|s| s.parse())
                        .transpose()
                        .map_err(|e| format!("valid_until: {e}"))?,
                    known_at: mem
                        .known_at
                        .as_deref()
                        .map(|s| s.parse())
                        .transpose()
                        .map_err(|e| format!("known_at: {e}"))?,
                    confidence: Some(0.9),
                    provenance: Provenance {
                        source: Some(format!("fixture:{}", mem.key)),
                        kind: Some("eval".into()),
                        span: None,
                    },
                    entity_links: vec![],
                    embedding,
                    embedding_identity,
                    idempotency_key: Some(mem.key.clone()),
                    extractor_version: None,
                    model_version: None,
                    valid_rev_from: None,
                    valid_rev_until: None,
                })
                .await
                .map_err(|e| format!("nomiso put: {e}"))?;
        }
        Ok(())
    }

    async fn present(&mut self, probe: &FixtureProbe) -> Result<Presented, String> {
        let t0 = Instant::now();
        let mut req = PrepareContextRequest {
            request_id: None,
            scope: probe.scope.clone(),
            scope_match: nomiso_core::scope::ScopeMatch::Exact,
            task: probe.query.clone(),
            entities: vec![],
            inventory: vec![],
            budget: Default::default(),
            effort: None,
            as_of: None,
            known_as_of: None,
            sys_as_of: None,
            categories: None,
            degradation: nomiso_memory::DegradationPolicy::Strict,
            min_score: None,
            session_id: None,
            turn_id: None,
            emit_trace: true,
            trace_id: None,
        };
        req.budget.max_tokens = 1200;
        req.budget.max_blocks = 6;
        req.budget.max_candidates = 20;
        let proposal = prepare_context(&self.client, req)
            .await
            .map_err(|e| format!("prepare_context: {e}"))?;
        // Host inserts the full selected subset; ack verifies it durably.
        let inserted: Vec<InsertedBlock> = proposal
            .blocks
            .iter()
            .map(|b: &ContextBlock| InsertedBlock {
                block_id: b.block_id.clone(),
                truncated: false,
                note: None,
            })
            .collect();
        let receipt = record_insertion(
            &self.client,
            InsertionAck {
                trace_id: proposal.trace_id.clone(),
                proposal_id: proposal.proposal_id.clone(),
                scope: probe.scope.clone(),
                host: "h2h-eval".into(),
                inserted,
                session_id: None,
                turn_id: None,
            },
        )
        .await
        .map_err(|e| format!("record_insertion: {e}"))?;
        let acked: HashSet<&str> = receipt.inserted.iter().map(String::as_str).collect();
        Ok(Presented {
            blocks: proposal
                .blocks
                .iter()
                .filter(|b| acked.contains(b.block_id.as_str()))
                .map(|b| EvidenceBlock {
                    id: b.block_id.clone(),
                    text: b.excerpt.clone(),
                })
                .collect(),
            search_ms: t0.elapsed().as_secs_f64() * 1000.0,
        })
    }
}

/// Hindsight channel — presented = recalled items (no insertion ack exists
/// on the foreign API; citations verify against retrieval only).
pub struct HindsightChannel {
    inner: HindsightSystem,
    label: String,
}

impl HindsightChannel {
    pub fn from_env() -> Option<Self> {
        let extract = std::env::var("EVAL_HINDSIGHT_EXTRACT").unwrap_or_else(|_| "chunks".into());
        let mode = if extract == "none" {
            "default".to_string()
        } else {
            extract
        };
        HindsightSystem::from_env().map(|inner| Self {
            inner,
            label: format!("hindsight-{mode}"),
        })
    }
    pub async fn ping(&self) -> Result<(), String> {
        self.inner.ping().await
    }
}

#[async_trait]
impl EvidenceChannel for HindsightChannel {
    fn id(&self) -> String {
        self.label.clone()
    }
    async fn ingest(&mut self, scenario: &CodingScenario) -> Result<(), String> {
        self.inner.retain_gold(&scenario.gold_memories).await
    }
    async fn present(&mut self, probe: &FixtureProbe) -> Result<Presented, String> {
        let t0 = Instant::now();
        let hits = self
            .inner
            .search(&CompareSearch {
                query: probe.query.clone(),
                scope: probe.scope.clone(),
                k: probe.limit.max(1),
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
                category: None,
                query_embedding: None,
            })
            .await?;
        Ok(Presented {
            blocks: hits
                .into_iter()
                .enumerate()
                .map(|(i, h)| EvidenceBlock {
                    id: format!("h{}", i + 1),
                    text: h.text,
                })
                .collect(),
            search_ms: t0.elapsed().as_secs_f64() * 1000.0,
        })
    }
}

// --- answerer ---------------------------------------------------------------

/// Grok CLI answering agent (same headless flags as `CliLlm`).
pub struct GrokAnswerer {
    pub bin: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub timeout: Duration,
}

impl GrokAnswerer {
    pub fn from_env() -> Self {
        Self {
            bin: std::env::var("EVAL_H2H_GROK").unwrap_or_else(|_| "grok".into()),
            model: std::env::var("EVAL_H2H_MODEL")
                .ok()
                .filter(|s| !s.is_empty()),
            effort: std::env::var("EVAL_H2H_EFFORT")
                .ok()
                .filter(|s| !s.is_empty())
                .or_else(|| Some("low".into())),
            timeout: Duration::from_secs(
                std::env::var("EVAL_H2H_TIMEOUT_S")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(120),
            ),
        }
    }
}

#[async_trait]
impl Answerer for GrokAnswerer {
    fn id(&self) -> String {
        match &self.model {
            Some(m) => format!("grok-cli:{m}"),
            None => "grok-cli".into(),
        }
    }
    async fn answer(&self, system: &str, user: &str) -> Result<String, String> {
        let mut cmd = tokio::process::Command::new(&self.bin);
        cmd.arg("--output-format")
            .arg("plain")
            .arg("--system-prompt-override")
            .arg(system)
            .arg("--always-approve")
            .arg("--no-subagents")
            .arg("--no-plan")
            .arg("--verbatim")
            .arg("-p")
            .arg(user)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        if let Some(m) = &self.model {
            cmd.arg("--model").arg(m);
        }
        if let Some(e) = &self.effort {
            cmd.arg("--effort").arg(e);
        }
        let out = match tokio::time::timeout(self.timeout, cmd.output()).await {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => return Err(format!("grok spawn: {e}")),
            Err(_) => return Err("grok timeout".into()),
        };
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !out.status.success() {
            return Err(format!(
                "grok status {:?}: {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
                    .chars()
                    .take(300)
                    .collect::<String>()
            ));
        }
        if text.is_empty() {
            return Err("grok returned empty stdout".into());
        }
        Ok(text)
    }
}

// --- runner -----------------------------------------------------------------

const ANSWER_SYSTEM: &str = "You are an answering agent inside a memory harness. \
Answer the question using ONLY the evidence blocks provided.\n\
Rules:\n\
- After your answer, output one line: USED: [id1, id2] listing every evidence \
block your answer relied on. If you used none, output USED: [].\n\
- If the evidence does not contain the answer, output exactly ABSTAIN and \
nothing else. Do not answer from your own knowledge.\n\
- Never invent evidence ids.\n\
- In your answer, restate the relevant fact from the evidence verbatim, then \
give your conclusion in one sentence.";

fn answer_user(probe: &FixtureProbe, presented: &[EvidenceBlock]) -> String {
    let mut s = format!("Question: {}\n\nEvidence:\n", probe.query);
    if presented.is_empty() {
        s.push_str("(no evidence provided)\n");
    }
    for b in presented {
        s.push_str(&format!("[{}] {}\n", b.id, b.text));
    }
    s
}

/// Drive channels over scenarios; `answerer` answers per probe.
/// Skips are per-system (foreign channels may be absent or down).
pub async fn run_h2h(
    scenarios: &[CodingScenario],
    channels: &mut [Box<dyn EvidenceChannel>],
    answerer: &dyn Answerer,
    embedder_label: &str,
) -> H2hReport {
    let mut rows = Vec::new();
    let mut systems = Vec::new();
    let mut skipped_systems = Vec::new();
    for ch in channels.iter_mut() {
        systems.push(ch.id().to_string());
        for s in scenarios {
            if let Err(e) = ch.ingest(s).await {
                skipped_systems.push((ch.id().to_string(), format!("ingest: {e}")));
                break;
            }
            for p in &s.probes {
                if let Some(why) = h2h_skip_reason(p) {
                    rows.push(H2hRow {
                        system: ch.id(),
                        scenario: s.id.clone(),
                        probe: p.name.clone(),
                        tag: p
                            .tag
                            .map(|t| format!("{t:?}").to_lowercase())
                            .unwrap_or_else(|| "untagged".into()),
                        skipped: Some(why),
                        fact_hit: false,
                        leak: false,
                        abstain_ok: None,
                        citation_fraud: vec![],
                        forbidden_asserted: false,
                        n_presented: 0,
                        gold_supported: 0,
                        n_gold: 0,
                        est_tokens: 0,
                        search_ms: None,
                        answer_ms: None,
                        passed: true,
                        detail: "skipped".into(),
                    });
                    continue;
                }
                let presented = match ch.present(p).await {
                    Ok(v) => v,
                    Err(e) => {
                        skipped_systems.push((ch.id().to_string(), format!("present: {e}")));
                        break;
                    }
                };
                let t0 = Instant::now();
                let raw = answerer
                    .answer(ANSWER_SYSTEM, &answer_user(p, &presented.blocks))
                    .await;
                let answer_ms = t0.elapsed().as_secs_f64() * 1000.0;
                match raw {
                    Ok(text) => {
                        let parsed = parse_citation_answer(&text);
                        let gold = gold_texts_for(s, p);
                        let forbidden = forbidden_texts_for(s, p);
                        let sys = ch.id();
                        rows.push(score_citation_row(
                            p,
                            CitationInput {
                                system: &sys,
                                scenario: &s.id,
                                gold_texts: &gold,
                                forbidden_texts: &forbidden,
                                presented: &presented.blocks,
                                answer: &parsed,
                                search_ms: Some(presented.search_ms),
                                answer_ms: Some(answer_ms),
                            },
                        ));
                    }
                    Err(e) => rows.push(H2hRow {
                        system: ch.id(),
                        scenario: s.id.clone(),
                        probe: p.name.clone(),
                        tag: p
                            .tag
                            .map(|t| format!("{t:?}").to_lowercase())
                            .unwrap_or_else(|| "untagged".into()),
                        skipped: Some(format!("answerer: {e}")),
                        fact_hit: false,
                        leak: false,
                        abstain_ok: None,
                        citation_fraud: vec![],
                        forbidden_asserted: false,
                        n_presented: presented.blocks.len() as u32,
                        gold_supported: 0,
                        n_gold: 0,
                        est_tokens: 0,
                        search_ms: Some(presented.search_ms),
                        answer_ms: Some(answer_ms),
                        passed: true,
                        detail: "answerer failed".into(),
                    }),
                }
            }
        }
    }

    // De-dup skip markers; a system marked skipped still keeps earlier rows.
    skipped_systems.sort();
    skipped_systems.dedup();
    let mut by_system: BTreeMap<String, SystemStats> = BTreeMap::new();
    let mut search_ms: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for r in &rows {
        if r.skipped.is_some() {
            continue;
        }
        let st = by_system.entry(r.system.clone()).or_default();
        st.scored += 1;
        st.fact_hit += r.fact_hit as u32;
        st.leak += r.leak as u32;
        if let Some(ok) = r.abstain_ok {
            if ok {
                st.abstain_ok += 1;
            } else {
                st.abstain_miss += 1;
            }
        }
        if !r.citation_fraud.is_empty() {
            st.citation_fraud_rows += 1;
        }
        st.forbidden_asserted += r.forbidden_asserted as u32;
        st.presented_blocks += r.n_presented;
        st.est_tokens += r.est_tokens;
        if let Some(ms) = r.search_ms {
            search_ms.entry(r.system.clone()).or_default().push(ms);
        }
        if r.answer_ms.is_some() {
            st.answer_calls += 1;
        }
    }
    for (sys, st) in by_system.iter_mut() {
        if let Some(v) = search_ms.get_mut(sys) {
            v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            st.search_ms_p50 = v.get(v.len() / 2).copied().unwrap_or(0.0);
            st.search_ms_p95 = v
                .get(((v.len() * 95) / 100).min(v.len().saturating_sub(1)))
                .copied()
                .unwrap_or(0.0);
        }
    }

    H2hReport {
        suite: "coding-agent h2h".into(),
        host: answerer.id(),
        embedder: embedder_label.into(),
        systems,
        skipped_systems,
        rows,
        by_system,
        passed: true,
    }
}

/// Live entry: `EVAL_H2H_LIVE=1` + grok on PATH + optional channels.
pub async fn run_h2h_live() -> H2hReport {
    let scenarios = crate::bundled_coding_scenarios().unwrap_or_default();
    let limit = std::env::var("EVAL_H2H_LIMIT")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(usize::MAX);
    let scenarios: Vec<CodingScenario> = scenarios.into_iter().take(limit).collect();

    let mut channels: Vec<Box<dyn EvidenceChannel>> = Vec::new();
    #[cfg(feature = "http")]
    let mut embedder_label = "hashing-8".to_string();
    #[cfg(not(feature = "http"))]
    let embedder_label = "hashing-8".to_string();

    #[cfg(feature = "http")]
    if let Ok(url) = std::env::var("VEGAPUNK_EMBED_URL") {
        let model = std::env::var("VEGAPUNK_EMBED_MODEL").unwrap_or_else(|_| "bge-small".into());
        let edim: usize = std::env::var("VEGAPUNK_EMBED_DIM")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(384);
        match nomiso_embed::HttpEmbedder::new(nomiso_embed::HttpEmbedderConfig {
            base_url: url,
            model: model.clone(),
            dimension: edim,
            api_key: std::env::var("VEGAPUNK_EMBED_API_KEY").unwrap_or_default(),
        }) {
            Ok(emb) => {
                let host: std::sync::Arc<dyn nomiso_service::Embedder> = std::sync::Arc::new(emb);
                match NomisoT4Channel::connect_with_embedder(edim, host).await {
                    Ok(ch) => {
                        embedder_label = format!("http:{model}");
                        channels.push(Box::new(ch));
                    }
                    Err(e) => eprintln!("h2h: nomiso http channel failed: {e}"),
                }
            }
            Err(e) => eprintln!("h2h: http embedder rejected: {e}"),
        }
    }
    if channels.is_empty() {
        match NomisoT4Channel::connect(8).await {
            Ok(ch) => channels.push(Box::new(ch)),
            Err(e) => eprintln!("h2h: nomiso channel failed: {e}"),
        }
    }

    if let Some(hs) = HindsightChannel::from_env() {
        match hs.ping().await {
            Ok(()) => channels.push(Box::new(hs)),
            Err(e) => eprintln!("h2h: hindsight down ({e}) — skipped"),
        }
    }

    let answerer = GrokAnswerer::from_env();
    run_h2h(&scenarios, &mut channels, &answerer, &embedder_label).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundled_coding_scenarios;

    fn probe(expect_keys: &[&str], abstain: bool) -> FixtureProbe {
        FixtureProbe {
            name: "p_test".into(),
            query: "q?".into(),
            scope: "org/t".into(),
            limit: 5,
            expect_keys_in_top: expect_keys.iter().map(|s| s.to_string()).collect(),
            expect_abstain: abstain,
            tag: Some(ProbeTag::Exact),
            ..Default::default()
        }
    }

    fn blocks(pairs: &[(&str, &str)]) -> Vec<EvidenceBlock> {
        pairs
            .iter()
            .map(|(id, t)| EvidenceBlock {
                id: id.to_string(),
                text: t.to_string(),
            })
            .collect()
    }

    #[test]
    fn parse_used_and_abstain() {
        let a = parse_citation_answer("The CLI is Rust.\nUSED: [b1, b2]\n");
        assert_eq!(a.used, vec!["b1", "b2"]);
        assert_eq!(a.text, "The CLI is Rust.");
        let b = parse_citation_answer("ABSTAIN\n");
        assert!(b.abstained);
        let c = parse_citation_answer("No idea.\nUSED: []\n");
        assert!(c.used.is_empty() && !c.abstained);
        // Trailing prose after the bracket must not mint phantom ids.
        let d = parse_citation_answer("Answer.\nUSED: [h1] and nothing else\n");
        assert_eq!(d.used, vec!["h1"]);
        // A body line merely starting with "Used" is not a marker.
        let e = parse_citation_answer("Used the block above to conclude.");
        assert!(e.used.is_empty() && e.text.contains("Used the block"));
        // Comma list without brackets still parses.
        let f = parse_citation_answer("Answer.\nUSED: b1, b2\n");
        assert_eq!(f.used, vec!["b1", "b2"]);
    }

    #[test]
    fn fraud_only_for_unpresented_ids() {
        let gold = vec!["the port is 9090".to_string()];
        let forbidden = vec![];
        let presented = blocks(&[("b1", "the port is 9090")]);
        let p = probe(&["k1"], false);
        // Duplicate declared id is not fraud; unknown id is.
        let a = parse_citation_answer("Port is 9090.\nUSED: [b1, b1, ghost]");
        let r = score_citation_row(
            &p,
            CitationInput {
                system: "sys",
                scenario: "s",
                gold_texts: &gold,
                forbidden_texts: &forbidden,
                presented: &presented,
                answer: &a,
                search_ms: None,
                answer_ms: None,
            },
        );
        assert_eq!(r.citation_fraud, vec!["ghost".to_string()]);
        assert!(!r.passed);
        // Empty presented set: any declared id is fraud.
        let a2 = parse_citation_answer("Port is 9090.\nUSED: [b1]");
        let r2 = score_citation_row(
            &p,
            CitationInput {
                system: "sys",
                scenario: "s",
                gold_texts: &gold,
                forbidden_texts: &[],
                presented: &[],
                answer: &a2,
                search_ms: None,
                answer_ms: None,
            },
        );
        assert_eq!(r2.citation_fraud, vec!["b1".to_string()]);
    }

    #[test]
    fn grounded_hit_vs_leak_vs_fraud() {
        let gold = vec!["vegapunk-cli is implemented in Rust".to_string()];
        let forbidden = vec![];
        let presented = blocks(&[
            (
                "b1",
                "vegapunk-cli is implemented in Rust. Do not rewrite it.",
            ),
            ("b2", "unrelated fact about ports"),
        ]);
        let p = probe(&["k1"], false);

        let grounded = parse_citation_answer("It is Rust.\nUSED: [b1]");
        let r = score_citation_row(
            &p,
            CitationInput {
                system: "sys",
                scenario: "s",
                gold_texts: &gold,
                forbidden_texts: &forbidden,
                presented: &presented,
                answer: &grounded,
                search_ms: None,
                answer_ms: None,
            },
        );
        assert!(r.fact_hit && !r.leak && r.citation_fraud.is_empty() && r.passed);

        let leak = parse_citation_answer("It is Rust.\nUSED: [b2]");
        let r = score_citation_row(
            &p,
            CitationInput {
                system: "sys",
                scenario: "s",
                gold_texts: &gold,
                forbidden_texts: &forbidden,
                presented: &presented,
                answer: &leak,
                search_ms: None,
                answer_ms: None,
            },
        );
        assert!(r.leak && !r.fact_hit && !r.passed);

        let fraud = parse_citation_answer("It is Rust.\nUSED: [b9]");
        let r = score_citation_row(
            &p,
            CitationInput {
                system: "sys",
                scenario: "s",
                gold_texts: &gold,
                forbidden_texts: &forbidden,
                presented: &presented,
                answer: &fraud,
                search_ms: None,
                answer_ms: None,
            },
        );
        assert_eq!(r.citation_fraud, vec!["b9"]);
        assert!(!r.passed);

        let no_used = parse_citation_answer("It is Rust.\nUSED: []");
        let r = score_citation_row(
            &p,
            CitationInput {
                system: "sys",
                scenario: "s",
                gold_texts: &gold,
                forbidden_texts: &forbidden,
                presented: &presented,
                answer: &no_used,
                search_ms: None,
                answer_ms: None,
            },
        );
        assert!(r.leak, "asserting gold with zero used blocks is a leak");
    }

    #[test]
    fn abstain_contract() {
        let gold = vec!["secret fact".to_string()];
        let p = probe(&[], true);
        let a = parse_citation_answer("ABSTAIN");
        let r = score_citation_row(
            &p,
            CitationInput {
                system: "sys",
                scenario: "s",
                gold_texts: &gold,
                forbidden_texts: &[],
                presented: &[],
                answer: &a,
                search_ms: None,
                answer_ms: None,
            },
        );
        assert_eq!(r.abstain_ok, Some(true));
        assert!(r.passed);

        let b = parse_citation_answer("The secret fact is known.\nUSED: []");
        let r = score_citation_row(
            &p,
            CitationInput {
                system: "sys",
                scenario: "s",
                gold_texts: &gold,
                forbidden_texts: &[],
                presented: &[],
                answer: &b,
                search_ms: None,
                answer_ms: None,
            },
        );
        assert_eq!(r.abstain_ok, Some(false));
    }

    #[test]
    fn scope_isolation_negative_probe() {
        // Keyless probe with must_not_include keys: abstain = pass; asserting
        // the off-scope secret = forbidden_asserted fail.
        let p = FixtureProbe {
            must_not_include_keys: vec!["secret_key".into()],
            ..probe(&[], false)
        };
        let gold = vec![];
        let forbidden = vec!["staging token is rot13-not-real".to_string()];

        let a = parse_citation_answer("ABSTAIN");
        let r = score_citation_row(
            &p,
            CitationInput {
                system: "sys",
                scenario: "s",
                gold_texts: &gold,
                forbidden_texts: &forbidden,
                presented: &blocks(&[("b1", "staging token is rot13-not-real")]),
                answer: &a,
                search_ms: None,
                answer_ms: None,
            },
        );
        assert!(r.passed && !r.fact_hit && !r.forbidden_asserted);

        let b = parse_citation_answer("The staging token is rot13-not-real.\nUSED: [b1]");
        let r = score_citation_row(
            &p,
            CitationInput {
                system: "sys",
                scenario: "s",
                gold_texts: &gold,
                forbidden_texts: &forbidden,
                presented: &blocks(&[("b1", "staging token is rot13-not-real")]),
                answer: &b,
                search_ms: None,
                answer_ms: None,
            },
        );
        assert!(r.forbidden_asserted && !r.passed);
    }

    struct MockAnswerer;
    #[async_trait]
    impl Answerer for MockAnswerer {
        fn id(&self) -> String {
            "mock".into()
        }
        async fn answer(&self, _s: &str, user: &str) -> Result<String, String> {
            // Scripted: cite the first presented block when the gold text is
            // visible in it; abstain otherwise (mirrors a compliant host).
            let mut ids = Vec::new();
            for line in user.lines() {
                if line.starts_with('[') {
                    if let Some(id) = line.strip_prefix('[').and_then(|l| l.split(']').next()) {
                        if line.contains("Rust") || line.contains("port") {
                            ids.push(id.to_string());
                        }
                    }
                }
            }
            if ids.is_empty() {
                return Ok("ABSTAIN".into());
            }
            Ok(format!(
                "The answer is in the evidence.\nUSED: [{}]",
                ids.join(", ")
            ))
        }
    }

    /// Deterministic end-to-end: real NomisoT4Channel, scripted answerer.
    /// Proves the T4 loop + scoring path without a live LLM.
    #[tokio::test]
    async fn h2h_mock_end_to_end() {
        let scenarios: Vec<CodingScenario> = bundled_coding_scenarios()
            .unwrap()
            .into_iter()
            .take(3)
            .collect();
        let mut channels: Vec<Box<dyn EvidenceChannel>> =
            vec![Box::new(NomisoT4Channel::connect(8).await.unwrap())];
        let answerer = MockAnswerer;
        let report = run_h2h(&scenarios, &mut channels, &answerer, "hashing-8").await;
        eprintln!("{}", report.markdown());
        assert!(report.passed);
        let nomiso = report.by_system.get("nomiso-t4").expect("nomiso stats");
        assert!(nomiso.scored > 0);
        // Every scored row's citations verified against the acked subset —
        // fraud must be structurally impossible on this channel.
        assert_eq!(nomiso.citation_fraud_rows, 0);
    }

    /// Live head-to-head (skip-honest): `EVAL_H2H_LIVE=1`, grok on PATH,
    /// optional `EVAL_HINDSIGHT_URL` / `VEGAPUNK_EMBED_*` channels.
    /// Writes the JSON report to `EVAL_H2H_OUT` when set.
    #[tokio::test]
    async fn h2h_live() {
        if std::env::var("EVAL_H2H_LIVE").is_err() {
            eprintln!("skip: EVAL_H2H_LIVE unset");
            return;
        }
        let report = run_h2h_live().await;
        eprintln!("{}", report.markdown());
        if let Ok(path) = std::env::var("EVAL_H2H_OUT") {
            // Cargo tests run with cwd = crate root; resolve repo-relative
            // paths against the workspace root so `evals/…` lands correctly.
            let mut path = std::path::PathBuf::from(path);
            if path.is_relative() {
                if let Some(root) = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .parent()
                    .and_then(|p| p.parent())
                {
                    path = root.join(path);
                }
            }
            match std::fs::write(
                &path,
                serde_json::to_string_pretty(&report).unwrap_or_default(),
            ) {
                Ok(()) => eprintln!("h2h report written: {}", path.display()),
                Err(e) => eprintln!("h2h report write failed {}: {e}", path.display()),
            }
        }
        // Runner health: at least the nomiso channel must have scored rows.
        assert!(
            report
                .by_system
                .get("nomiso-t4")
                .map(|s| s.scored)
                .unwrap_or(0)
                > 0,
            "nomiso-t4 scored no rows"
        );
    }
}
