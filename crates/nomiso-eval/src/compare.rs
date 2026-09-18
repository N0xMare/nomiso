//! Controlled plane comparison: Nomiso vs optional SOTA HTTP systems.
//!
//! Tracks stay split. Hashing embedder is lexical-only and is never scored
//! against Hindsight/Mem0/Zep semantic recall. Foreign systems skip when
//! their env URL is unset or `/health` fails.

use async_trait::async_trait;
use nomiso_core::ops::{PutRequest, SearchQuery};
use nomiso_core::scope::ScopeMatch;
use nomiso_core::types::{Category, Content, Provenance, Timestamp};
use nomiso_embed::HashingEmbedder;
use nomiso_service::NomisoClient;
use nomiso_store::StoreConfig;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::time::Instant;

use crate::{EmbedderLabel, FixtureMemory, FixtureProbe, ProbeTag};

/// How a hit was identified as a gold key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdMethod {
    /// Mapped from Nomiso write id.
    NativeId,
    /// `metadata.gold_key`.
    Metadata,
    /// `document_id` suffix.
    DocumentId,
    /// Normalized text contains gold text.
    TextFallback,
}

/// Structured gold ingest vs product retain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestMode {
    /// Verbatim gold facts (plane track).
    GoldPut,
    /// Product LLM extract (system track; not used in CI).
    LlmRetain,
}

/// One retrieved hit after adapter normalization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompareHit {
    pub key: Option<String>,
    pub text: String,
    pub rank: u32,
    pub id_method: IdMethod,
}

/// Search request sent to every system.
#[derive(Debug, Clone)]
pub struct CompareSearch {
    pub query: String,
    pub scope: String,
    pub k: u32,
    pub as_of: Option<Timestamp>,
    pub known_as_of: Option<Timestamp>,
    pub sys_as_of: Option<Timestamp>,
    pub category: Option<String>,
    pub query_embedding: Option<Vec<f32>>,
}

/// Capability advertisement.
#[derive(Debug, Clone)]
pub struct SystemCaps {
    pub id: &'static str,
    pub scores_are_relative: bool,
}

/// Optional side-by-side memory system (retain + search only).
#[async_trait]
pub trait MemorySystem: Send + Sync {
    fn caps(&self) -> SystemCaps;
    async fn ping(&self) -> Result<(), String>;
    async fn retain_gold(&self, items: &[FixtureMemory]) -> Result<(), String>;
    async fn search(&self, req: &CompareSearch) -> Result<Vec<CompareHit>, String>;
}

/// Per-probe row (one system).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompareRow {
    pub suite: String,
    pub track: String,
    pub ingest_mode: IngestMode,
    pub embedder: EmbedderLabel,
    pub system: String,
    pub probe: String,
    pub tag: ProbeTag,
    pub k: u32,
    pub hit_at_k: bool,
    pub recall_at_k: f64,
    pub abstained: bool,
    pub leaked: bool,
    pub skipped: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
    pub hit_keys: Vec<String>,
    /// Wall time for this system's search call (milliseconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_ms: Option<f64>,
}

/// Ingest + search cost for one compare run (same probes; not blended into hit@k).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CostSummary {
    pub ingest_ms: f64,
    pub search_ms_p50: f64,
    pub search_ms_p95: f64,
    pub search_ms_max: f64,
    /// Gold rows that requested an embedding at put time.
    pub embed_puts: u32,
    /// Probes that sent a query vector (explicit `embedding_from` or HTTP auto-embed).
    pub query_embeds: u32,
}

/// Aggregate by tag for one system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagStats {
    pub tag: ProbeTag,
    pub n: u32,
    pub hits: u32,
    pub skipped: u32,
}

/// Full comparison report (may include skipped foreign systems).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompareReport {
    pub suite: String,
    pub embedder: EmbedderLabel,
    pub track: String,
    pub systems: Vec<String>,
    pub skipped_systems: Vec<(String, String)>,
    pub rows: Vec<CompareRow>,
    pub by_system_tag: BTreeMap<String, Vec<TagStats>>,
    pub passed: bool,
    #[serde(default)]
    pub cost: CostSummary,
}

impl CompareReport {
    /// Markdown miss table (tag × system × hit@k). Skipped rows are noted.
    pub fn miss_markdown(&self) -> String {
        let mut s = format!(
            "# Compare report `{}`\n\ntrack={} embedder={:?} passed={}\n\n",
            self.suite, self.track, self.embedder, self.passed
        );
        s.push_str("## Systems\n\n");
        for sys in &self.systems {
            s.push_str(&format!("- ran: `{sys}`\n"));
        }
        for (sys, why) in &self.skipped_systems {
            s.push_str(&format!("- skipped: `{sys}` — {why}\n"));
        }
        s.push_str(
            "\n## By tag\n\n| system | tag | hits | n | skipped |\n|---|---|---:|---:|---:|\n",
        );
        for (sys, tags) in &self.by_system_tag {
            for t in tags {
                s.push_str(&format!(
                    "| {} | {:?} | {} | {} | {} |\n",
                    sys, t.tag, t.hits, t.n, t.skipped
                ));
            }
        }
        s.push_str("\n## Cost\n\n");
        s.push_str(&format!(
            "ingest_ms={:.2} search_ms p50={:.2} p95={:.2} max={:.2} embed_puts={} query_embeds={}\n",
            self.cost.ingest_ms,
            self.cost.search_ms_p50,
            self.cost.search_ms_p95,
            self.cost.search_ms_max,
            self.cost.embed_puts,
            self.cost.query_embeds
        ));
        s.push_str("\n## Misses / skips\n\n");
        for r in &self.rows {
            if r.skipped || !r.hit_at_k {
                s.push_str(&format!(
                    "- `{}` / {} / {:?} hit={} skip={:?} keys={:?} search_ms={:?}\n",
                    r.system, r.probe, r.tag, r.hit_at_k, r.skip_reason, r.hit_keys, r.search_ms
                ));
            }
        }
        s
    }
}

/// Nomiso plane adapter (always available).
pub struct NomisoSystem {
    client: NomisoClient,
    embedder: HashingEmbedder,
    /// When true, put/search use the client's attached embedder (HTTP).
    client_embeds: bool,
    id_to_key: HashMap<String, String>,
}

impl NomisoSystem {
    /// In-memory plane; hashing used only for explicit vectors, never auto-attached.
    pub async fn connect(dim: usize) -> nomiso_core::error::Result<Self> {
        let client = NomisoClient::connect(StoreConfig::memory_test(dim)).await?;
        let embedder = HashingEmbedder::new(dim)?;
        Ok(Self {
            client,
            embedder,
            client_embeds: false,
            id_to_key: HashMap::new(),
        })
    }

    /// In-memory plane with a host embedder (HTTP compare path).
    pub async fn connect_with_embedder(
        dim: usize,
        host: std::sync::Arc<dyn nomiso_service::Embedder>,
    ) -> nomiso_core::error::Result<Self> {
        let client = NomisoClient::connect(StoreConfig::memory_test(dim))
            .await?
            .with_embedder(host);
        let embedder = HashingEmbedder::new(dim)?;
        Ok(Self {
            client,
            embedder,
            client_embeds: true,
            id_to_key: HashMap::new(),
        })
    }

    fn map_key(&self, id: &str, bare: &str) -> Option<String> {
        self.id_to_key
            .get(id)
            .or_else(|| self.id_to_key.get(bare))
            .cloned()
    }
}

#[async_trait]
impl MemorySystem for NomisoSystem {
    fn caps(&self) -> SystemCaps {
        SystemCaps {
            id: "nomiso",
            scores_are_relative: true,
        }
    }

    async fn ping(&self) -> Result<(), String> {
        Ok(())
    }

    async fn retain_gold(&self, _items: &[FixtureMemory]) -> Result<(), String> {
        Err("use retain_gold_mut".into())
    }

    async fn search(&self, req: &CompareSearch) -> Result<Vec<CompareHit>, String> {
        let hits = self
            .client
            .search(SearchQuery {
                query: req.query.clone(),
                scope: req.scope.clone(),
                scope_match: ScopeMatch::Exact,
                as_of: req.as_of,
                known_as_of: req.known_as_of,
                sys_as_of: req.sys_as_of,
                categories: req
                    .category
                    .as_deref()
                    .and_then(Category::parse)
                    .map(|c| vec![c]),
                limit: Some(req.k),
                embedding: req.query_embedding.clone(),
                graph_enrich: Some(false),
                graph_expand: None,
            })
            .await
            .map_err(|e| e.to_string())?;
        Ok(hits
            .iter()
            .enumerate()
            .map(|(i, h)| CompareHit {
                key: self.map_key(h.id.as_str(), h.id.bare_key()),
                text: h.preview.clone(),
                rank: (i as u32) + 1,
                id_method: IdMethod::NativeId,
            })
            .collect())
    }
}

impl NomisoSystem {
    /// Put gold facts; records logical keys.
    pub async fn ingest(&mut self, items: &[FixtureMemory]) -> nomiso_core::error::Result<()> {
        for mem in items {
            let category = mem
                .category
                .as_deref()
                .and_then(Category::parse)
                .unwrap_or(Category::Semantic);
            let embedding = if self.client_embeds {
                None
            } else if mem.with_embedding {
                Some(self.embedder.embed_one(&mem.text))
            } else {
                None
            };
            // Raw-supplied vectors were produced by this embedder — claim the
            // identity honestly so the store can stamp the generation.
            let embedding_identity = if embedding.is_some() {
                nomiso_service::Embedder::identity(&self.embedder)
            } else {
                None
            };
            let wr = self
                .client
                .put(PutRequest {
                    scope: mem.scope.clone(),
                    category,
                    content: Content::text(mem.text.clone()),
                    valid_from: parse_opt_ts(&mem.valid_from)?,
                    valid_until: parse_opt_ts(&mem.valid_until)?,
                    known_at: parse_opt_ts(&mem.known_at)?,
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
                .await?;
            let id = wr.id.to_string();
            self.id_to_key.insert(id, mem.key.clone());
            self.id_to_key
                .insert(wr.id.bare_key().to_string(), mem.key.clone());
        }
        Ok(())
    }

    pub fn embed_text(&self, text: &str) -> Vec<f32> {
        self.embedder.embed_one(text)
    }
}

fn parse_opt_ts(s: &Option<String>) -> nomiso_core::error::Result<Option<Timestamp>> {
    match s {
        None => Ok(None),
        Some(raw) => raw
            .parse::<Timestamp>()
            .map(Some)
            .map_err(|e| nomiso_core::error::Error::invalid(format!("timestamp '{raw}': {e}"))),
    }
}

/// Hindsight HTTP adapter. Inactive unless `EVAL_HINDSIGHT_URL` is set.
pub struct HindsightSystem {
    base: String,
    bank: String,
    http: reqwest::Client,
}

impl HindsightSystem {
    /// From env; `None` if unset.
    pub fn from_env() -> Option<Self> {
        let base = std::env::var("EVAL_HINDSIGHT_URL").ok()?;
        let base = base.trim_end_matches('/').to_string();
        if base.is_empty() {
            return None;
        }
        let bank = std::env::var("EVAL_HINDSIGHT_BANK").unwrap_or_else(|_| "nomiso-eval".into());
        Some(Self {
            base,
            bank,
            http: reqwest::Client::new(),
        })
    }
}

#[async_trait]
impl MemorySystem for HindsightSystem {
    fn caps(&self) -> SystemCaps {
        SystemCaps {
            id: "hindsight",
            scores_are_relative: true,
        }
    }

    async fn ping(&self) -> Result<(), String> {
        let url = format!("{}/health", self.base);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("hindsight health: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("hindsight health {}", resp.status()));
        }
        Ok(())
    }

    async fn retain_gold(&self, items: &[FixtureMemory]) -> Result<(), String> {
        let mut by_scope: BTreeMap<String, Vec<&FixtureMemory>> = BTreeMap::new();
        for m in items {
            by_scope.entry(m.scope.clone()).or_default().push(m);
        }
        // EVAL_HINDSIGHT_EXTRACT: "chunks" (default, plane-equivalent — no
        // LLM at ingest), "none" (no override → container default), or any
        // explicit mode the deployment accepts (labeled in the report).
        let extract = std::env::var("EVAL_HINDSIGHT_EXTRACT").unwrap_or_else(|_| "chunks".into());
        for (scope, group) in by_scope {
            let bank = hindsight_bank(&self.bank, &scope);
            if extract != "none" {
                let put_bank = format!("{}/v1/default/banks/{}", self.base, bank);
                let _ = self
                    .http
                    .put(&put_bank)
                    .json(&serde_json::json!({ "retain_extraction_mode": extract }))
                    .send()
                    .await;
            }
            let url = format!("{}/v1/default/banks/{}/memories", self.base, bank);
            for m in group {
                let doc = if let Some(group) = m.replace_group.as_deref() {
                    format!("gold:{group}")
                } else if m.key == "pref_v1" || m.key == "pref_v2" {
                    "gold:default_language".to_string()
                } else {
                    format!("gold:{}", m.key)
                };
                let item = serde_json::json!({
                    "content": m.text,
                    "document_id": doc,
                    "metadata": { "gold_key": m.key },
                    "update_mode": "replace",
                    "timestamp": m.valid_from,
                });
                let resp = self
                    .http
                    .post(&url)
                    .json(&serde_json::json!({ "items": [item], "async": false }))
                    .send()
                    .await
                    .map_err(|e| format!("hindsight retain: {e}"))?;
                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    return Err(format!("hindsight retain {status}: {body}"));
                }
            }
        }
        Ok(())
    }

    async fn search(&self, req: &CompareSearch) -> Result<Vec<CompareHit>, String> {
        let bank = hindsight_bank(&self.bank, &req.scope);
        let url = format!("{}/v1/default/banks/{}/memories/recall", self.base, bank);
        let mut body = serde_json::json!({
            "query": req.query,
            "types": ["world", "experience"],
            "budget": "mid",
            "max_tokens": 4096,
        });
        if let Some(ts) = req.as_of {
            body["query_timestamp"] = serde_json::json!(ts.to_string());
        }
        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("hindsight recall: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("hindsight recall {}", resp.status()));
        }
        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("hindsight recall json: {e}"))?;
        let results = v
            .get("results")
            .and_then(|x| x.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(results
            .into_iter()
            .take(req.k as usize)
            .enumerate()
            .map(|(i, r)| {
                let text = r
                    .get("text")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                let (key, method) = gold_key_from_foreign(&r, &text);
                CompareHit {
                    key,
                    text,
                    rank: (i as u32) + 1,
                    id_method: method,
                }
            })
            .collect())
    }
}

/// Mem0 platform/OSS adapter. Inactive unless `EVAL_MEM0_URL` is set.
pub struct Mem0System {
    base: String,
    token: Option<String>,
    http: reqwest::Client,
}

impl Mem0System {
    /// From env; `None` if unset.
    pub fn from_env() -> Option<Self> {
        let base = std::env::var("EVAL_MEM0_URL").ok()?;
        let base = base.trim_end_matches('/').to_string();
        if base.is_empty() {
            return None;
        }
        Some(Self {
            base,
            token: std::env::var("MEM0_API_KEY").ok(),
            http: reqwest::Client::new(),
        })
    }

    fn apply_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.token {
            Some(t) => req.header("Authorization", format!("Token {t}")),
            None => req,
        }
    }
}

#[async_trait]
impl MemorySystem for Mem0System {
    fn caps(&self) -> SystemCaps {
        SystemCaps {
            id: "mem0",
            scores_are_relative: true,
        }
    }

    async fn ping(&self) -> Result<(), String> {
        // Managed API has no /health; a missing key is a skip at from_env time for URL.
        Ok(())
    }

    async fn retain_gold(&self, items: &[FixtureMemory]) -> Result<(), String> {
        for m in items {
            let url = format!("{}/v3/memories/add/", self.base);
            let body = serde_json::json!({
                "messages": [{ "role": "user", "content": m.text }],
                "user_id": m.scope,
                "infer": false,
                "metadata": { "gold_key": m.key },
            });
            let resp = self
                .apply_auth(self.http.post(&url))
                .json(&body)
                .send()
                .await
                .map_err(|e| format!("mem0 add: {e}"))?;
            if !resp.status().is_success() {
                return Err(format!("mem0 add {}", resp.status()));
            }
        }
        Ok(())
    }

    async fn search(&self, req: &CompareSearch) -> Result<Vec<CompareHit>, String> {
        let url = format!("{}/v3/memories/search/", self.base);
        let body = serde_json::json!({
            "query": req.query,
            "filters": { "user_id": req.scope },
            "top_k": req.k,
            "threshold": 0.0,
        });
        let resp = self
            .apply_auth(self.http.post(&url))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("mem0 search: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("mem0 search {}", resp.status()));
        }
        let v: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
        let results = v
            .get("results")
            .and_then(|x| x.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(results
            .into_iter()
            .take(req.k as usize)
            .enumerate()
            .map(|(i, r)| {
                let text = r
                    .get("memory")
                    .or_else(|| r.get("text"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                let (key, method) = gold_key_from_foreign(&r, &text);
                CompareHit {
                    key,
                    text,
                    rank: (i as u32) + 1,
                    id_method: method,
                }
            })
            .collect())
    }
}

/// Zep Cloud adapter. Inactive unless `EVAL_ZEP_URL` is set.
pub struct ZepSystem {
    base: String,
    key: String,
    http: reqwest::Client,
}

impl ZepSystem {
    /// From env; `None` if URL or key unset.
    pub fn from_env() -> Option<Self> {
        let base = std::env::var("EVAL_ZEP_URL").ok()?;
        let key = std::env::var("ZEP_API_KEY").ok()?;
        let base = base.trim_end_matches('/').to_string();
        if base.is_empty() || key.is_empty() {
            return None;
        }
        Some(Self {
            base,
            key,
            http: reqwest::Client::new(),
        })
    }
}

#[async_trait]
impl MemorySystem for ZepSystem {
    fn caps(&self) -> SystemCaps {
        SystemCaps {
            id: "zep",
            scores_are_relative: true,
        }
    }

    async fn ping(&self) -> Result<(), String> {
        Ok(())
    }

    async fn retain_gold(&self, items: &[FixtureMemory]) -> Result<(), String> {
        for m in items {
            let url = format!("{}/api/v2/graph", self.base);
            let body = serde_json::json!({
                "data": m.text,
                "type": "text",
                "user_id": m.scope,
                "source_description": "eval gold",
                "metadata": { "gold_key": m.key },
            });
            let resp = self
                .http
                .post(&url)
                .header("Authorization", format!("Api-Key {}", self.key))
                .json(&body)
                .send()
                .await
                .map_err(|e| format!("zep add: {e}"))?;
            if !resp.status().is_success() && resp.status().as_u16() != 202 {
                return Err(format!("zep add {}", resp.status()));
            }
        }
        Ok(())
    }

    async fn search(&self, req: &CompareSearch) -> Result<Vec<CompareHit>, String> {
        let url = format!("{}/api/v2/graph/search", self.base);
        let body = serde_json::json!({
            "query": req.query,
            "user_id": req.scope,
            "scope": "edges",
            "limit": req.k,
            "reranker": "rrf",
        });
        let resp = self
            .http
            .post(&url)
            .header("Authorization", format!("Api-Key {}", self.key))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("zep search: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("zep search {}", resp.status()));
        }
        let v: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
        let edges = v
            .get("edges")
            .and_then(|x| x.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(edges
            .into_iter()
            .take(req.k as usize)
            .enumerate()
            .map(|(i, r)| {
                let text = r
                    .get("fact")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                let (key, method) = gold_key_from_foreign(&r, &text);
                CompareHit {
                    key,
                    text,
                    rank: (i as u32) + 1,
                    id_method: method,
                }
            })
            .collect())
    }
}

fn hindsight_bank(prefix: &str, scope: &str) -> String {
    format!("{prefix}-{}", scope.replace(['/', ':'], "-"))
}

fn gold_key_from_foreign(row: &serde_json::Value, text: &str) -> (Option<String>, IdMethod) {
    if let Some(k) = row
        .pointer("/metadata/gold_key")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        return (Some(k.to_string()), IdMethod::Metadata);
    }
    if let Some(doc) = row.get("document_id").and_then(|v| v.as_str()) {
        if let Some(rest) = doc.strip_prefix("gold:") {
            return (Some(rest.to_string()), IdMethod::DocumentId);
        }
    }
    (Some(text.to_string()), IdMethod::TextFallback)
}

fn score_probe(
    suite: &str,
    system: &str,
    embedder: EmbedderLabel,
    probe: &FixtureProbe,
    hits: &[CompareHit],
    gold: &[FixtureMemory],
    search_ms: Option<f64>,
) -> CompareRow {
    let k = probe.limit.max(1);
    let tag = probe.tag.unwrap_or(ProbeTag::Exact);
    if system != "nomiso" && !foreign_probe_allowed(embedder, probe, system) {
        return skip_row(
            suite,
            system,
            embedder,
            probe,
            skip_reason_for(embedder, probe, system),
        );
    }
    if probe.skip_if_no_graph || tag == ProbeTag::MultiHop {
        return CompareRow {
            suite: suite.into(),
            track: "plane".into(),
            ingest_mode: IngestMode::GoldPut,
            embedder,
            system: system.into(),
            probe: probe.name.clone(),
            tag,
            k,
            hit_at_k: false,
            recall_at_k: 0.0,
            abstained: hits.is_empty(),
            leaked: false,
            skipped: true,
            skip_reason: Some("multi_hop / graph writer not present".into()),
            hit_keys: vec![],
            search_ms,
        };
    }
    let hit_keys: Vec<String> = hits.iter().filter_map(|h| h.key.clone()).collect();
    let mut resolved = hit_keys.clone();
    // Text-fallback: map hit text onto gold keys.
    for h in hits {
        if h.id_method == IdMethod::TextFallback {
            if let Some(g) = gold.iter().find(|g| {
                let a = normalize_text(&h.text);
                let b = normalize_text(&g.text);
                !a.is_empty() && (a.contains(&b) || b.contains(&a))
            }) {
                if !resolved.contains(&g.key) {
                    resolved.push(g.key.clone());
                }
            }
        }
    }

    let leaked = probe
        .must_not_include_keys
        .iter()
        .any(|k| resolved.contains(k));
    let expect = &probe.expect_keys_in_top;
    let found = expect.iter().filter(|k| resolved.contains(*k)).count();
    let recall = if expect.is_empty() {
        1.0
    } else {
        found as f64 / expect.len() as f64
    };
    let abstained = hits.is_empty();
    let hit_at_k = if probe.expect_abstain {
        abstained && !leaked
    } else {
        !leaked && expect.iter().all(|k| resolved.contains(k))
    };

    CompareRow {
        suite: suite.into(),
        track: "plane".into(),
        ingest_mode: IngestMode::GoldPut,
        embedder,
        system: system.into(),
        probe: probe.name.clone(),
        tag,
        k,
        hit_at_k,
        recall_at_k: recall,
        abstained,
        leaked,
        skipped: false,
        skip_reason: None,
        hit_keys: resolved,
        search_ms,
    }
}

fn normalize_text(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .flat_map(|c| c.to_lowercase())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn summarize(rows: &[CompareRow]) -> BTreeMap<String, Vec<TagStats>> {
    let mut map: BTreeMap<(String, ProbeTag), (u32, u32, u32)> = BTreeMap::new();
    for r in rows {
        let e = map.entry((r.system.clone(), r.tag)).or_default();
        e.0 += 1;
        if r.skipped {
            e.2 += 1;
        } else if r.hit_at_k {
            e.1 += 1;
        }
    }
    let mut out: BTreeMap<String, Vec<TagStats>> = BTreeMap::new();
    for ((sys, tag), (n, hits, skipped)) in map {
        out.entry(sys).or_default().push(TagStats {
            tag,
            n,
            hits,
            skipped,
        });
    }
    out
}

/// Built-in tagged plane suite (CI-safe; hashing labeled).
pub fn channel_suite() -> crate::EvalSuite {
    crate::EvalSuite {
        name: "sota_plane_v1".into(),
        embedding_dim: 32,
        memories: vec![
            FixtureMemory {
                key: "error_code".into(),
                scope: "org/eval/user/alice".into(),
                text: "Build failed with error NOMI_SCOPE_DENIED_42 on project foobar.".into(),
                category: Some("episodic".into()),
                with_embedding: true,
                valid_from: None,
                valid_until: None,
                ..Default::default()
            },
            FixtureMemory {
                key: "ts_pref".into(),
                scope: "org/eval/user/alice".into(),
                text: "Alice prefers TypeScript for agent tooling and MCP servers.".into(),
                category: Some("semantic".into()),
                with_embedding: true,
                valid_from: None,
                valid_until: None,
                ..Default::default()
            },
            FixtureMemory {
                key: "pref_v1".into(),
                scope: "org/eval/user/alice".into(),
                text: "Alice uses Python as her default language.".into(),
                category: Some("semantic".into()),
                with_embedding: true,
                valid_from: Some("2024-01-01T00:00:00Z".into()),
                valid_until: Some("2025-06-01T00:00:00Z".into()),
                ..Default::default()
            },
            FixtureMemory {
                key: "pref_v2".into(),
                scope: "org/eval/user/alice".into(),
                text: "Alice uses Rust as her default language.".into(),
                category: Some("semantic".into()),
                with_embedding: true,
                valid_from: Some("2025-06-01T00:00:00Z".into()),
                valid_until: None,
                ..Default::default()
            },
            FixtureMemory {
                key: "other_project".into(),
                scope: "org/eval/user/bob".into(),
                text: "Alice prefers TypeScript for agent tooling and MCP servers.".into(),
                category: Some("semantic".into()),
                with_embedding: true,
                valid_from: None,
                valid_until: None,
                ..Default::default()
            },
        ],
        probes: vec![
            FixtureProbe {
                name: "exact_error_code".into(),
                query: "NOMI_SCOPE_DENIED_42".into(),
                scope: "org/eval/user/alice".into(),
                scope_match: Some("exact".into()),
                limit: 5,
                expect_keys_in_top: vec!["error_code".into()],
                embedding_from: None,
                require_vector: false,
                tag: Some(ProbeTag::Exact),
                as_of: None,
                must_not_include_keys: vec![],
                expect_abstain: false,
                skip_if_no_graph: false,
                ..Default::default()
            },
            FixtureProbe {
                name: "paraphrase_typescript".into(),
                query: String::new(),
                scope: "org/eval/user/alice".into(),
                scope_match: Some("exact".into()),
                limit: 3,
                expect_keys_in_top: vec!["ts_pref".into()],
                embedding_from: Some("She likes using TypeScript when building agent tools".into()),
                require_vector: true,
                tag: Some(ProbeTag::Paraphrase),
                as_of: None,
                must_not_include_keys: vec![],
                expect_abstain: false,
                skip_if_no_graph: false,
                ..Default::default()
            },
            FixtureProbe {
                name: "hybrid_ts_pref".into(),
                query: "TypeScript MCP".into(),
                scope: "org/eval/user/alice".into(),
                scope_match: Some("exact".into()),
                limit: 5,
                expect_keys_in_top: vec!["ts_pref".into()],
                embedding_from: Some("TypeScript agent tooling preference".into()),
                require_vector: true,
                tag: Some(ProbeTag::Hybrid),
                as_of: None,
                must_not_include_keys: vec![],
                expect_abstain: false,
                skip_if_no_graph: false,
                ..Default::default()
            },
            FixtureProbe {
                name: "temporal_before_switch".into(),
                query: "default language".into(),
                scope: "org/eval/user/alice".into(),
                scope_match: Some("exact".into()),
                limit: 5,
                expect_keys_in_top: vec!["pref_v1".into()],
                embedding_from: None,
                require_vector: false,
                tag: Some(ProbeTag::Temporal),
                as_of: Some("2025-01-15T00:00:00Z".into()),
                must_not_include_keys: vec!["pref_v2".into()],
                expect_abstain: false,
                skip_if_no_graph: false,
                ..Default::default()
            },
            FixtureProbe {
                name: "temporal_after_switch".into(),
                query: "default language".into(),
                scope: "org/eval/user/alice".into(),
                scope_match: Some("exact".into()),
                limit: 5,
                expect_keys_in_top: vec!["pref_v2".into()],
                embedding_from: None,
                require_vector: false,
                tag: Some(ProbeTag::Temporal),
                as_of: Some("2026-01-15T00:00:00Z".into()),
                must_not_include_keys: vec!["pref_v1".into()],
                expect_abstain: false,
                skip_if_no_graph: false,
                ..Default::default()
            },
            FixtureProbe {
                name: "knowledge_update_current".into(),
                query: "default language".into(),
                scope: "org/eval/user/alice".into(),
                scope_match: Some("exact".into()),
                limit: 5,
                expect_keys_in_top: vec!["pref_v2".into()],
                embedding_from: None,
                require_vector: false,
                tag: Some(ProbeTag::KnowledgeUpdate),
                as_of: None,
                must_not_include_keys: vec!["pref_v1".into()],
                expect_abstain: false,
                skip_if_no_graph: false,
                ..Default::default()
            },
            FixtureProbe {
                name: "scope_isolation".into(),
                query: "TypeScript MCP".into(),
                scope: "org/eval/user/alice".into(),
                scope_match: Some("exact".into()),
                limit: 10,
                expect_keys_in_top: vec!["ts_pref".into()],
                embedding_from: None,
                require_vector: false,
                tag: Some(ProbeTag::ScopeIsolation),
                as_of: None,
                must_not_include_keys: vec!["other_project".into()],
                expect_abstain: false,
                skip_if_no_graph: false,
                ..Default::default()
            },
            FixtureProbe {
                name: "abstain_unknown_fact".into(),
                query: "ZQXY_NEVER_INGESTED_TOKEN_99".into(),
                scope: "org/eval/user/nobody".into(),
                scope_match: Some("exact".into()),
                limit: 5,
                expect_keys_in_top: vec![],
                embedding_from: None,
                require_vector: false,
                tag: Some(ProbeTag::Abstain),
                as_of: None,
                must_not_include_keys: vec![],
                expect_abstain: true,
                skip_if_no_graph: false,
                ..Default::default()
            },
            FixtureProbe {
                name: "multi_hop_placeholder".into(),
                query: "who works on the crate that failed NOMI_SCOPE_DENIED_42".into(),
                scope: "org/eval/user/alice".into(),
                scope_match: Some("exact".into()),
                limit: 5,
                expect_keys_in_top: vec![],
                embedding_from: None,
                require_vector: false,
                tag: Some(ProbeTag::MultiHop),
                as_of: None,
                must_not_include_keys: vec![],
                expect_abstain: false,
                skip_if_no_graph: true,
                ..Default::default()
            },
        ],
    }
}

fn search_for_probe(nomiso: &NomisoSystem, probe: &FixtureProbe) -> CompareSearch {
    let as_of = probe
        .as_of
        .as_deref()
        .and_then(|s| s.parse::<Timestamp>().ok());
    let known_as_of = probe
        .known_as_of
        .as_deref()
        .and_then(|s| s.parse::<Timestamp>().ok());
    let sys_as_of = probe
        .sys_as_of
        .as_deref()
        .and_then(|s| s.parse::<Timestamp>().ok());
    let (query, query_embedding) = if nomiso.client_embeds {
        let q = if probe.query.trim().is_empty() {
            probe.embedding_from.clone().unwrap_or_default()
        } else {
            probe.query.clone()
        };
        (q, None)
    } else {
        (
            probe.query.clone(),
            probe.embedding_from.as_ref().map(|t| nomiso.embed_text(t)),
        )
    };
    CompareSearch {
        query,
        scope: probe.scope.clone(),
        k: probe.limit.max(1),
        as_of,
        known_as_of,
        sys_as_of,
        category: probe.category.clone(),
        query_embedding,
    }
}

/// Run the tagged plane suite on Nomiso (always) and optional SOTA systems.
pub async fn run_sota_plane(suite: &crate::EvalSuite) -> nomiso_core::error::Result<CompareReport> {
    let mut nomiso = NomisoSystem::connect(suite.embedding_dim).await?;
    let t0 = Instant::now();
    nomiso.ingest(&suite.memories).await?;
    let ingest_ms = t0.elapsed().as_secs_f64() * 1000.0;
    run_sota_plane_on(suite, EmbedderLabel::Hashing, nomiso, ingest_ms).await
}

fn foreign_probe_allowed(embedder: EmbedderLabel, probe: &FixtureProbe, _system: &str) -> bool {
    let tag = probe.tag.unwrap_or(ProbeTag::Exact);
    if tag == ProbeTag::MultiHop || probe.skip_if_no_graph {
        return false;
    }
    if embedder == EmbedderLabel::Hashing && matches!(tag, ProbeTag::Paraphrase | ProbeTag::Hybrid)
    {
        return false;
    }
    if tag == ProbeTag::Temporal {
        return false;
    }
    if tag == ProbeTag::Uncertainty {
        return false;
    }
    true
}

fn skip_reason_for(embedder: EmbedderLabel, probe: &FixtureProbe, _system: &str) -> String {
    let tag = probe.tag.unwrap_or(ProbeTag::Exact);
    if tag == ProbeTag::MultiHop || probe.skip_if_no_graph {
        return "multi_hop / graph writer not present".into();
    }
    if embedder == EmbedderLabel::Hashing && matches!(tag, ProbeTag::Paraphrase | ProbeTag::Hybrid)
    {
        return "hashing paraphrase/hybrid is not comparable to foreign semantics".into();
    }
    if tag == ProbeTag::Temporal {
        return "foreign ingest has no Nomiso as_of intervals".into();
    }
    if tag == ProbeTag::Uncertainty {
        return "uncertainty category is Nomiso write discipline".into();
    }
    "skipped".into()
}

fn skip_row(
    suite: &str,
    system: &str,
    embedder: EmbedderLabel,
    probe: &FixtureProbe,
    reason: String,
) -> CompareRow {
    CompareRow {
        suite: suite.into(),
        track: "plane".into(),
        ingest_mode: IngestMode::GoldPut,
        embedder,
        system: system.into(),
        probe: probe.name.clone(),
        tag: probe.tag.unwrap_or(ProbeTag::Exact),
        k: probe.limit.max(1),
        hit_at_k: false,
        recall_at_k: 0.0,
        abstained: false,
        leaked: false,
        skipped: true,
        skip_reason: Some(reason),
        hit_keys: vec![],
        search_ms: None,
    }
}

/// Convenience: run built-in channel suite (Nomiso required; others skip).
pub async fn run_channel_scoreboard() -> nomiso_core::error::Result<CompareReport> {
    run_sota_plane(&channel_suite()).await
}

/// HTTP-embed plane compare. `None` when `VEGAPUNK_EMBED_URL` is unset.
///
/// Paraphrase/hybrid vs Hindsight are scored (not skipped) on this path.
#[cfg(feature = "http")]
pub async fn run_sota_plane_http(
    suite: &crate::EvalSuite,
) -> nomiso_core::error::Result<Option<CompareReport>> {
    use nomiso_embed::{HttpEmbedder, HttpEmbedderConfig};

    let url = match std::env::var("VEGAPUNK_EMBED_URL") {
        Ok(u) if !u.trim().is_empty() => u,
        _ => return Ok(None),
    };
    let key = std::env::var("VEGAPUNK_EMBED_API_KEY")
        .or_else(|_| std::env::var("OPENAI_API_KEY"))
        .map_err(|_| {
            nomiso_core::error::Error::invalid(
                "VEGAPUNK_EMBED_URL set but no VEGAPUNK_EMBED_API_KEY / OPENAI_API_KEY",
            )
        })?;
    let model =
        std::env::var("VEGAPUNK_EMBED_MODEL").unwrap_or_else(|_| "text-embedding-3-small".into());
    let dim: usize = std::env::var("VEGAPUNK_EMBED_DIM")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1536);
    let emb = HttpEmbedder::new(HttpEmbedderConfig {
        base_url: url,
        api_key: key,
        model,
        dimension: dim,
    })?;
    let mut labeled = suite.clone();
    labeled.embedding_dim = dim;
    let mut nomiso = NomisoSystem::connect_with_embedder(dim, std::sync::Arc::new(emb)).await?;
    let t0 = Instant::now();
    nomiso.ingest(&labeled.memories).await?;
    let ingest_ms = t0.elapsed().as_secs_f64() * 1000.0;
    Ok(Some(
        run_sota_plane_on(&labeled, EmbedderLabel::Http, nomiso, ingest_ms).await?,
    ))
}

/// Shared compare loop used by hashing and HTTP runners.
async fn run_sota_plane_on(
    suite: &crate::EvalSuite,
    embedder: EmbedderLabel,
    nomiso: NomisoSystem,
    ingest_ms: f64,
) -> nomiso_core::error::Result<CompareReport> {
    let mut rows = Vec::new();
    for probe in &suite.probes {
        let req = search_for_probe(&nomiso, probe);
        let t0 = Instant::now();
        let hits = MemorySystem::search(&nomiso, &req)
            .await
            .map_err(nomiso_core::error::Error::store)?;
        let search_ms = t0.elapsed().as_secs_f64() * 1000.0;
        rows.push(score_probe(
            &suite.name,
            "nomiso",
            embedder,
            probe,
            &hits,
            &suite.memories,
            Some(search_ms),
        ));
    }

    let mut skipped_systems = Vec::new();
    let mut systems = vec!["nomiso".to_string()];

    if let Some(hs) = HindsightSystem::from_env() {
        match hs.ping().await {
            Ok(()) => match hs.retain_gold(&suite.memories).await {
                Ok(()) => {
                    systems.push("hindsight".into());
                    for probe in &suite.probes {
                        if !foreign_probe_allowed(embedder, probe, "hindsight") {
                            rows.push(skip_row(
                                &suite.name,
                                "hindsight",
                                embedder,
                                probe,
                                skip_reason_for(embedder, probe, "hindsight"),
                            ));
                            continue;
                        }
                        let req = search_for_probe(&nomiso, probe);
                        let t0 = Instant::now();
                        match hs.search(&req).await {
                            Ok(hits) => {
                                let search_ms = t0.elapsed().as_secs_f64() * 1000.0;
                                rows.push(score_probe(
                                    &suite.name,
                                    "hindsight",
                                    embedder,
                                    probe,
                                    &hits,
                                    &suite.memories,
                                    Some(search_ms),
                                ))
                            }
                            Err(e) => {
                                rows.push(skip_row(&suite.name, "hindsight", embedder, probe, e))
                            }
                        }
                    }
                }
                Err(e) => skipped_systems.push(("hindsight".into(), e)),
            },
            Err(e) => skipped_systems.push(("hindsight".into(), e)),
        }
    } else {
        skipped_systems.push(("hindsight".into(), "EVAL_HINDSIGHT_URL unset".into()));
    }

    if let Some(ms) = Mem0System::from_env() {
        match ms.retain_gold(&suite.memories).await {
            Ok(()) => {
                systems.push("mem0".into());
                for probe in &suite.probes {
                    if !foreign_probe_allowed(embedder, probe, "mem0") {
                        rows.push(skip_row(
                            &suite.name,
                            "mem0",
                            embedder,
                            probe,
                            skip_reason_for(embedder, probe, "mem0"),
                        ));
                        continue;
                    }
                    let req = search_for_probe(&nomiso, probe);
                    let t0 = Instant::now();
                    match ms.search(&req).await {
                        Ok(hits) => {
                            let search_ms = t0.elapsed().as_secs_f64() * 1000.0;
                            rows.push(score_probe(
                                &suite.name,
                                "mem0",
                                embedder,
                                probe,
                                &hits,
                                &suite.memories,
                                Some(search_ms),
                            ))
                        }
                        Err(e) => rows.push(skip_row(&suite.name, "mem0", embedder, probe, e)),
                    }
                }
            }
            Err(e) => skipped_systems.push(("mem0".into(), e)),
        }
    } else {
        skipped_systems.push(("mem0".into(), "EVAL_MEM0_URL unset".into()));
    }

    if let Some(zs) = ZepSystem::from_env() {
        match zs.retain_gold(&suite.memories).await {
            Ok(()) => {
                systems.push("zep".into());
                for probe in &suite.probes {
                    if !foreign_probe_allowed(embedder, probe, "zep") {
                        rows.push(skip_row(
                            &suite.name,
                            "zep",
                            embedder,
                            probe,
                            skip_reason_for(embedder, probe, "zep"),
                        ));
                        continue;
                    }
                    let req = search_for_probe(&nomiso, probe);
                    let t0 = Instant::now();
                    match zs.search(&req).await {
                        Ok(hits) => {
                            let search_ms = t0.elapsed().as_secs_f64() * 1000.0;
                            rows.push(score_probe(
                                &suite.name,
                                "zep",
                                embedder,
                                probe,
                                &hits,
                                &suite.memories,
                                Some(search_ms),
                            ))
                        }
                        Err(e) => rows.push(skip_row(&suite.name, "zep", embedder, probe, e)),
                    }
                }
            }
            Err(e) => skipped_systems.push(("zep".into(), e)),
        }
    } else {
        skipped_systems.push(("zep".into(), "EVAL_ZEP_URL unset".into()));
    }

    let by_system_tag = summarize(&rows);
    let passed = rows
        .iter()
        .filter(|r| r.system == "nomiso" && !r.skipped)
        .filter(|r| {
            !(embedder == EmbedderLabel::Hashing
                && matches!(r.tag, ProbeTag::Paraphrase | ProbeTag::Hybrid))
        })
        .all(|r| r.hit_at_k);
    let cost = cost_summary(&rows, suite, embedder, ingest_ms);
    Ok(CompareReport {
        suite: suite.name.clone(),
        embedder,
        track: "plane".into(),
        systems,
        skipped_systems,
        rows,
        by_system_tag,
        passed,
        cost,
    })
}

fn cost_summary(
    rows: &[CompareRow],
    suite: &crate::EvalSuite,
    embedder: EmbedderLabel,
    ingest_ms: f64,
) -> CostSummary {
    let mut times: Vec<f64> = rows.iter().filter_map(|r| r.search_ms).collect();
    times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = times.len();
    let pct = |p: f64| -> f64 {
        if n == 0 {
            return 0.0;
        }
        let idx = ((p * (n as f64 - 1.0)).round() as usize).min(n - 1);
        times[idx]
    };
    let query_embeds = suite
        .probes
        .iter()
        .filter(|p| {
            p.embedding_from.is_some()
                || (embedder == EmbedderLabel::Http && !p.query.trim().is_empty())
        })
        .count() as u32;
    CostSummary {
        ingest_ms,
        search_ms_p50: pct(0.50),
        search_ms_p95: pct(0.95),
        search_ms_max: times.last().copied().unwrap_or(0.0),
        embed_puts: suite.memories.iter().filter(|m| m.with_embedding).count() as u32,
        query_embeds,
    }
}

/// Load a suite JSON file (`EvalSuite` shape).
pub fn load_suite_json(path: &std::path::Path) -> nomiso_core::error::Result<crate::EvalSuite> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| nomiso_core::error::Error::invalid(format!("read suite: {e}")))?;
    serde_json::from_str(&raw)
        .map_err(|e| nomiso_core::error::Error::invalid(format!("parse suite: {e}")))
}

#[cfg(test)]
mod skip_matrix_tests {
    use super::*;

    fn tagged(tag: ProbeTag) -> FixtureProbe {
        FixtureProbe {
            name: "skip_matrix".into(),
            query: "q".into(),
            scope: "org/eval".into(),
            scope_match: None,
            limit: 5,
            expect_keys_in_top: vec![],
            embedding_from: None,
            require_vector: false,
            tag: Some(tag),
            as_of: None,
            must_not_include_keys: vec![],
            expect_abstain: false,
            skip_if_no_graph: false,
            ..Default::default()
        }
    }

    #[test]
    fn skip_matrix_foreign_probe_allowed() {
        for tag in [ProbeTag::Paraphrase, ProbeTag::Hybrid] {
            for system in ["hindsight", "mem0", "zep"] {
                assert!(
                    !foreign_probe_allowed(EmbedderLabel::Hashing, &tagged(tag), system),
                    "hashing + {tag:?} + {system} must skip"
                );
                assert_eq!(
                    skip_reason_for(EmbedderLabel::Hashing, &tagged(tag), system),
                    "hashing paraphrase/hybrid is not comparable to foreign semantics"
                );
            }
        }

        assert!(foreign_probe_allowed(
            EmbedderLabel::Hashing,
            &tagged(ProbeTag::Exact),
            "hindsight"
        ));

        for embedder in [
            EmbedderLabel::Hashing,
            EmbedderLabel::Http,
            EmbedderLabel::None,
        ] {
            for system in ["hindsight", "mem0", "zep"] {
                assert!(
                    !foreign_probe_allowed(embedder, &tagged(ProbeTag::Temporal), system),
                    "{embedder:?} + temporal + {system} must skip"
                );
                assert_eq!(
                    skip_reason_for(embedder, &tagged(ProbeTag::Temporal), system),
                    "foreign ingest has no Nomiso as_of intervals"
                );
            }
        }

        assert!(foreign_probe_allowed(
            EmbedderLabel::Http,
            &tagged(ProbeTag::Paraphrase),
            "hindsight"
        ));

        assert!(!foreign_probe_allowed(
            EmbedderLabel::Http,
            &tagged(ProbeTag::Uncertainty),
            "hindsight"
        ));
    }
}
