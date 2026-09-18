//! Deterministic plane eval harness for Nomiso (no LLM).

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Arc;

use nomiso_core::ops::{PutRequest, SearchQuery};
use nomiso_core::scope::ScopeMatch;
use nomiso_core::types::{Category, Content, Provenance, ScoreKind};
use nomiso_embed::HashingEmbedder;
use nomiso_service::{Embedder, NomisoClient};
use nomiso_store::StoreConfig;
use serde::{Deserialize, Serialize};

pub mod coding_agent;
pub mod coding_skill;
pub mod compare;
pub mod graph_expand;
pub mod graph_expand_corpus;
pub mod h2h;

pub use coding_agent::{
    bundled_coding_scenarios, coding_agent_pack_dir, coding_agent_suite, load_coding_scenarios,
    validate_coding_pack, CodingScenario, TranscriptTurn, PACK_NAME,
};
pub use coding_skill::{run_hindsight_retain_skill, HindsightSkillReport, HindsightSkillRow};
pub use compare::{
    channel_suite, load_suite_json, run_channel_scoreboard, run_sota_plane, CompareReport,
    CompareRow, CostSummary, HindsightSystem, IngestMode, Mem0System, MemorySystem, NomisoSystem,
    TagStats, ZepSystem,
};

#[cfg(feature = "http")]
pub use compare::run_sota_plane_http;

/// Embedder label that must travel with every published score.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbedderLabel {
    /// FNV token-hash bag — lexical proxy, not semantic.
    Hashing,
    /// Real HTTP embeddings.
    Http,
    /// BM25 only (no query vector).
    None,
}

fn default_probe_limit() -> u32 {
    5
}

/// Question tag for the channel scoreboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeTag {
    Exact,
    Paraphrase,
    Hybrid,
    Temporal,
    MultiHop,
    KnowledgeUpdate,
    Abstain,
    ScopeIsolation,
    /// Unresolved conflict stored as `category: uncertainty` (Nomiso write discipline).
    Uncertainty,
}

/// A memory to insert before probes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FixtureMemory {
    /// Stable logical key for expected hits (not DB id).
    pub key: String,
    pub scope: String,
    pub text: String,
    #[serde(default)]
    pub category: Option<String>,
    /// If true, attach hashing embedding at put time.
    #[serde(default)]
    pub with_embedding: bool,
    /// Optional valid-from (RFC3339) for temporal probes.
    #[serde(default)]
    pub valid_from: Option<String>,
    /// Optional valid-until (RFC3339).
    #[serde(default)]
    pub valid_until: Option<String>,
    /// Optional known-at (RFC3339) — when the agent first believed the fact.
    #[serde(default)]
    pub known_at: Option<String>,
    /// Foreign replace key. Hindsight ingest shares `document_id=gold:{group}`
    /// so a later row replaces an earlier one (knowledge_update).
    #[serde(default)]
    pub replace_group: Option<String>,
}

/// Search probe after fixtures are loaded.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FixtureProbe {
    pub name: String,
    /// BM25 query text (empty allowed if embedding_from set).
    #[serde(default)]
    pub query: String,
    pub scope: String,
    #[serde(default)]
    pub scope_match: Option<String>,
    #[serde(default = "default_probe_limit")]
    pub limit: u32,
    /// Logical fixture keys that must appear in top-k.
    #[serde(default)]
    pub expect_keys_in_top: Vec<String>,
    /// If set, use hashing embed of this text as query vector.
    #[serde(default)]
    pub embedding_from: Option<String>,
    /// Require hybrid path (text + vector).
    #[serde(default)]
    pub require_vector: bool,
    /// Scoreboard tag. Missing → inferred only by name in legacy `run_suite`.
    #[serde(default)]
    pub tag: Option<ProbeTag>,
    /// Valid-time lens (RFC3339).
    #[serde(default)]
    pub as_of: Option<String>,
    /// Keys that must not appear (scope isolation).
    #[serde(default)]
    pub must_not_include_keys: Vec<String>,
    /// Pass iff the result set is empty.
    #[serde(default)]
    pub expect_abstain: bool,
    /// Formal skip until a graph writer exists.
    #[serde(default)]
    pub skip_if_no_graph: bool,
    /// Known-time lens (RFC3339).
    #[serde(default)]
    pub known_as_of: Option<String>,
    /// System-time lens (RFC3339).
    #[serde(default)]
    pub sys_as_of: Option<String>,
    /// Optional single-category filter (`semantic`, `uncertainty`, …).
    #[serde(default)]
    pub category: Option<String>,
}

/// One suite of memories + probes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalSuite {
    pub name: String,
    pub embedding_dim: usize,
    pub memories: Vec<FixtureMemory>,
    pub probes: Vec<FixtureProbe>,
}

/// Result of one probe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeResult {
    pub name: String,
    pub passed: bool,
    pub detail: String,
    pub hit_keys: Vec<String>,
    pub scores: Vec<f64>,
}

/// Suite summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuiteResult {
    pub name: String,
    pub passed: bool,
    pub probes: Vec<ProbeResult>,
}

/// Built-in Phase A/B hybrid suite (no external files required).
///
/// Includes discriminating BM25 df regimes (see `bm25_df_low` / `bm25_df_high_*`
/// and store tests `bm25_df_regimes_score_kind_honesty`).
pub fn hybrid_suite() -> EvalSuite {
    EvalSuite {
        name: "hybrid_plane_v1".into(),
        embedding_dim: 32,
        memories: vec![
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
                key: "rust_pref".into(),
                scope: "org/eval/user/alice".into(),
                text: "Alice also enjoys writing high-performance Rust systems code.".into(),
                category: Some("semantic".into()),
                with_embedding: true,
                valid_from: None,
                valid_until: None,
                ..Default::default()
            },
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
                key: "other_project".into(),
                scope: "org/eval/user/bob".into(),
                text: "Alice prefers TypeScript for agent tooling and MCP servers.".into(),
                category: Some("semantic".into()),
                with_embedding: true,
                valid_from: None,
                valid_until: None,
                ..Default::default()
            },
            // Shared filler docs so a common token can hit df ≥ N/2 over the whole table.
            // N grows with these; "commonword" appears in all fillers + pads.
            FixtureMemory {
                key: "pad_a".into(),
                scope: "org/eval/user/alice".into(),
                text: "commonword pad document alpha about nothing special.".into(),
                category: Some("episodic".into()),
                with_embedding: false,
                valid_from: None,
                valid_until: None,
                ..Default::default()
            },
            FixtureMemory {
                key: "pad_b".into(),
                scope: "org/eval/user/alice".into(),
                text: "commonword pad document beta about nothing special.".into(),
                category: Some("episodic".into()),
                with_embedding: false,
                valid_from: None,
                valid_until: None,
                ..Default::default()
            },
            FixtureMemory {
                key: "pad_c".into(),
                scope: "org/eval/user/alice".into(),
                text: "commonword pad document gamma about nothing special.".into(),
                category: Some("episodic".into()),
                with_embedding: false,
                valid_from: None,
                valid_until: None,
                ..Default::default()
            },
            FixtureMemory {
                key: "pad_d".into(),
                scope: "org/eval/user/alice".into(),
                text: "commonword pad document delta about nothing special rarezxq9.".into(),
                category: Some("episodic".into()),
                with_embedding: false,
                valid_from: None,
                valid_until: None,
                ..Default::default()
            },
        ],
        probes: vec![
            FixtureProbe {
                name: "bm25_exact_error_code".into(),
                query: "NOMI_SCOPE_DENIED_42".into(),
                scope: "org/eval/user/alice".into(),
                scope_match: Some("exact".into()),
                limit: 5,
                expect_keys_in_top: vec!["error_code".into()],
                embedding_from: None,
                require_vector: false,
                tag: None,
                as_of: None,
                must_not_include_keys: vec![],
                expect_abstain: false,
                skip_if_no_graph: false,
                ..Default::default()
            },
            FixtureProbe {
                name: "bm25_typescript_keyword".into(),
                query: "TypeScript MCP".into(),
                scope: "org/eval/user/alice".into(),
                scope_match: Some("exact".into()),
                limit: 5,
                expect_keys_in_top: vec!["ts_pref".into()],
                embedding_from: None,
                require_vector: false,
                tag: None,
                as_of: None,
                must_not_include_keys: vec![],
                expect_abstain: false,
                skip_if_no_graph: false,
                ..Default::default()
            },
            // df < N/2: rare token → genuine Engine BM25 (score_kind asserted in run_suite).
            FixtureProbe {
                name: "bm25_df_low_rare_engine".into(),
                query: "rarezxq9".into(),
                scope: "org/eval/user/alice".into(),
                scope_match: Some("exact".into()),
                limit: 5,
                expect_keys_in_top: vec!["pad_d".into()],
                embedding_from: None,
                require_vector: false,
                tag: None,
                as_of: None,
                must_not_include_keys: vec![],
                expect_abstain: false,
                skip_if_no_graph: false,
                ..Default::default()
            },
            // df ≥ N/2 candidate: commonword across pads; scores must be labeled
            // (Engine or RankFallback — never Unknown). Hits may be any pad_*.
            FixtureProbe {
                name: "bm25_df_high_common_labeled".into(),
                query: "commonword".into(),
                scope: "org/eval/user/alice".into(),
                scope_match: Some("exact".into()),
                limit: 8,
                expect_keys_in_top: vec!["pad_a".into()],
                embedding_from: None,
                require_vector: false,
                tag: None,
                as_of: None,
                must_not_include_keys: vec![],
                expect_abstain: false,
                skip_if_no_graph: false,
                ..Default::default()
            },
            FixtureProbe {
                name: "vector_paraphrase_typescript".into(),
                query: String::new(),
                scope: "org/eval/user/alice".into(),
                scope_match: Some("exact".into()),
                limit: 3,
                expect_keys_in_top: vec!["ts_pref".into()],
                embedding_from: Some("She likes using TypeScript when building agent tools".into()),
                require_vector: true,
                tag: None,
                as_of: None,
                must_not_include_keys: vec![],
                expect_abstain: false,
                skip_if_no_graph: false,
                ..Default::default()
            },
            FixtureProbe {
                name: "hybrid_mixed".into(),
                query: "TypeScript agent tooling".into(),
                scope: "org/eval/user/alice".into(),
                scope_match: Some("exact".into()),
                limit: 5,
                expect_keys_in_top: vec!["ts_pref".into()],
                embedding_from: Some("TypeScript agent tooling preference".into()),
                require_vector: true,
                tag: None,
                as_of: None,
                must_not_include_keys: vec![],
                expect_abstain: false,
                skip_if_no_graph: false,
                ..Default::default()
            },
            FixtureProbe {
                name: "scope_isolation_no_bob_leak".into(),
                query: "TypeScript MCP".into(),
                scope: "org/eval/user/alice".into(),
                scope_match: Some("exact".into()),
                limit: 10,
                expect_keys_in_top: vec!["ts_pref".into()],
                embedding_from: None,
                require_vector: false,
                tag: None,
                as_of: None,
                must_not_include_keys: vec![],
                expect_abstain: false,
                skip_if_no_graph: false,
                ..Default::default()
            },
        ],
    }
}

/// Run a suite against in-memory Nomiso + hashing embedder.
pub async fn run_suite(suite: &EvalSuite) -> nomiso_core::error::Result<SuiteResult> {
    let config = StoreConfig::memory_test(suite.embedding_dim);
    let embedder = Arc::new(HashingEmbedder::new(suite.embedding_dim)?);
    let client = NomisoClient::connect(config)
        .await?
        .with_embedder(embedder.clone() as Arc<dyn Embedder>);

    let mut key_to_id: HashMap<String, String> = HashMap::new();
    let mut id_to_key: HashMap<String, String> = HashMap::new();

    for mem in &suite.memories {
        let category = mem
            .category
            .as_deref()
            .and_then(Category::parse)
            .unwrap_or(Category::Semantic);
        let mut embedding = None;
        let mut embedding_identity = None;
        if mem.with_embedding {
            let v = embedder.embed_one(&mem.text);
            embedding = Some(v);
            // The vector really is this embedder's output — claim it (MIG-004).
            embedding_identity = embedder.identity();
        }
        let wr = client
            .put(PutRequest {
                scope: mem.scope.clone(),
                category,
                content: Content::text(mem.text.clone()),
                valid_from: None,
                valid_until: None,
                known_at: None,
                confidence: Some(0.9),
                provenance: Provenance {
                    source: Some(format!("fixture:{}", mem.key)),
                    kind: Some("eval".into()),
                    span: None,
                },
                entity_links: vec![],
                embedding,
                embedding_identity,
                idempotency_key: None,
                extractor_version: None,
                model_version: None,
                valid_rev_from: None,
                valid_rev_until: None,
            })
            .await?;
        let id = wr.id.to_string();
        key_to_id.insert(mem.key.clone(), id.clone());
        id_to_key.insert(id, mem.key.clone());
        // also map bare
        id_to_key.insert(wr.id.bare_key().to_string(), mem.key.clone());
    }

    let mut probes = Vec::new();
    for probe in &suite.probes {
        let scope_match = match probe.scope_match.as_deref() {
            Some("prefix") => ScopeMatch::Prefix,
            _ => ScopeMatch::Exact,
        };
        let embedding = probe
            .embedding_from
            .as_ref()
            .map(|text| embedder.embed_one(text));
        let hits = client
            .search(SearchQuery {
                query: probe.query.clone(),
                scope: probe.scope.clone(),
                scope_match,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
                categories: None,
                limit: Some(probe.limit),
                embedding,
                graph_enrich: Some(false),
                graph_expand: None,
            })
            .await?;

        let hit_keys: Vec<String> = hits
            .iter()
            .filter_map(|h| {
                id_to_key
                    .get(h.id.as_str())
                    .or_else(|| id_to_key.get(h.id.bare_key()))
                    .cloned()
            })
            .collect();
        let scores: Vec<f64> = hits.iter().map(|h| h.score).collect();
        let kinds: Vec<ScoreKind> = hits.iter().map(|h| h.score_kind).collect();

        // Scope isolation special-case: bob key must not appear
        let mut ok = true;
        let mut detail = String::new();
        if probe.name.contains("scope_isolation") && hit_keys.iter().any(|k| k == "other_project") {
            ok = false;
            detail = "leaked other_project from bob scope".into();
        }
        for expect in &probe.expect_keys_in_top {
            if !hit_keys.contains(expect) {
                ok = false;
                detail = format!(
                    "missing expected key '{expect}' in top-{}; got {hit_keys:?}",
                    probe.limit
                );
                break;
            }
        }

        // Score honesty: every hit must carry a labeled score_kind (never Unknown).
        // Note: RRF uses 1/(k+rank) which collides numerically with rank-decay; kind is the discriminator.
        for h in &hits {
            if h.score_kind == ScoreKind::Unknown {
                ok = false;
                detail = format!("unlabeled score_kind Unknown on hit score={}", h.score);
                break;
            }
        }
        // Low-df rare probe: top expected key present with Engine label (genuine channel score).
        // (Strict BM25-only df floor vs Engine is covered by nomiso-store::bm25_df_regimes_*)
        if ok && probe.name.contains("bm25_df_low") {
            let engine_ok = hits.iter().any(|h| h.score_kind == ScoreKind::Engine);
            if !engine_ok {
                ok = false;
                detail = format!(
                    "df<N/2 probe expected Engine score_kind; kinds={kinds:?} scores={scores:?}"
                );
            }
        }
        // High-df common probe: every hit labeled Engine or RankFallback (not Unknown).
        if ok && probe.name.contains("bm25_df_high") {
            let labeled = hits
                .iter()
                .all(|h| matches!(h.score_kind, ScoreKind::Engine | ScoreKind::RankFallback));
            if !labeled || hits.is_empty() {
                ok = false;
                detail =
                    format!("df≥N/2 probe needs labeled scores; kinds={kinds:?} scores={scores:?}");
            }
        }

        if ok && detail.is_empty() {
            detail = format!("ok hits={hit_keys:?} scores={scores:?} kinds={kinds:?}");
        }

        // Vector probes should not be empty
        if probe.require_vector && hits.is_empty() {
            ok = false;
            detail = "vector/hybrid probe returned no hits".into();
        }

        probes.push(ProbeResult {
            name: probe.name.clone(),
            passed: ok,
            detail,
            hit_keys,
            scores,
        });
    }

    let passed = probes.iter().all(|p| p.passed);
    Ok(SuiteResult {
        name: suite.name.clone(),
        passed,
        probes,
    })
}

/// Adversarial plane checks (validation + isolation) without fixtures file.
pub async fn run_adversarial() -> nomiso_core::error::Result<SuiteResult> {
    let dim = 16usize;
    let client = NomisoClient::connect(StoreConfig::memory_test(dim)).await?;
    let mut probes = Vec::new();

    // empty search
    let empty = client
        .search(SearchQuery {
            query: String::new(),
            scope: "org/x".into(),
            scope_match: ScopeMatch::Exact,
            as_of: None,
            known_as_of: None,
            sys_as_of: None,
            categories: None,
            limit: Some(5),
            embedding: None,
            graph_enrich: Some(false),
            graph_expand: None,
        })
        .await;
    probes.push(ProbeResult {
        name: "reject_empty_search".into(),
        passed: empty.is_err(),
        detail: format!("{empty:?}"),
        hit_keys: vec![],
        scores: vec![],
    });

    // dim mismatch
    let bad = client
        .put(PutRequest {
            scope: "org/x".into(),
            category: Category::Semantic,
            content: Content::text("bad vector"),
            valid_from: None,
            valid_until: None,
            known_at: None,
            confidence: None,
            provenance: Default::default(),
            entity_links: vec![],
            embedding: Some(vec![0.1, 0.2]),
            embedding_identity: None,
            idempotency_key: None,
            extractor_version: None,
            model_version: None,
            valid_rev_from: None,
            valid_rev_until: None,
        })
        .await;
    probes.push(ProbeResult {
        name: "reject_dim_mismatch".into(),
        passed: matches!(bad, Err(nomiso_core::Error::DimensionMismatch { .. })),
        detail: format!("{bad:?}"),
        hit_keys: vec![],
        scores: vec![],
    });

    // bad scope
    let bad_scope = client
        .put(PutRequest {
            scope: "/leading".into(),
            category: Category::Semantic,
            content: Content::text("nope"),
            valid_from: None,
            valid_until: None,
            known_at: None,
            confidence: None,
            provenance: Default::default(),
            entity_links: vec![],
            embedding: None,
            embedding_identity: None,
            idempotency_key: None,
            extractor_version: None,
            model_version: None,
            valid_rev_from: None,
            valid_rev_until: None,
        })
        .await;
    probes.push(ProbeResult {
        name: "reject_bad_scope".into(),
        passed: bad_scope.is_err(),
        detail: format!("{bad_scope:?}"),
        hit_keys: vec![],
        scores: vec![],
    });

    let passed = probes.iter().all(|p| p.passed);
    Ok(SuiteResult {
        name: "adversarial_v1".into(),
        passed,
        probes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hybrid_suite_passes() {
        let suite = hybrid_suite();
        let res = run_suite(&suite).await.expect("run");
        for p in &res.probes {
            assert!(p.passed, "{}: {}", p.name, p.detail);
        }
        assert!(res.passed);
    }

    #[tokio::test]
    async fn adversarial_suite_passes() {
        let res = run_adversarial().await.expect("run");
        for p in &res.probes {
            assert!(p.passed, "{}: {}", p.name, p.detail);
        }
        assert!(res.passed);
    }

    #[tokio::test]
    async fn channel_scoreboard_nomiso_plane() {
        let want_hindsight = std::env::var("EVAL_HINDSIGHT_URL")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let report = run_channel_scoreboard().await.expect("scoreboard");
        assert!(
            report.systems.iter().any(|s| s == "nomiso"),
            "nomiso must run"
        );
        if want_hindsight.is_none() {
            assert!(
                report
                    .skipped_systems
                    .iter()
                    .any(|(s, r)| s == "hindsight" && r.contains("EVAL_HINDSIGHT_URL")),
                "hindsight must skip without URL: {:?}",
                report.skipped_systems
            );
        }
        for row in report.rows.iter().filter(|r| r.system == "nomiso") {
            if row.skipped {
                assert_eq!(row.tag, ProbeTag::MultiHop, "{}", row.probe);
                continue;
            }
            assert!(
                row.hit_at_k,
                "{} tag={:?} keys={:?} leak={}",
                row.probe, row.tag, row.hit_keys, row.leaked
            );
        }
        assert!(report.passed, "nomiso non-skipped probes must pass");
        eprintln!("{}", report.miss_markdown());
    }

    #[tokio::test]
    async fn coding_agent_plane_v2() {
        let suite = coding_agent_suite().expect("compile coding pack");
        let report = run_sota_plane(&suite).await.expect("scoreboard");
        assert!(
            report.systems.iter().any(|s| s == "nomiso"),
            "nomiso must run"
        );
        assert!(
            report.cost.embed_puts > 0,
            "cost columns must count embed puts: {:?}",
            report.cost
        );
        assert!(
            report.cost.search_ms_max >= 0.0,
            "cost columns must include search_ms"
        );
        for row in report.rows.iter().filter(|r| r.system == "nomiso") {
            if row.skipped {
                assert_eq!(row.tag, ProbeTag::MultiHop, "{}", row.probe);
                continue;
            }
            assert!(row.search_ms.is_some(), "{} missing search_ms", row.probe);
            // Hashing is lexical. Paraphrase/hybrid on a cluttered coding bank
            // are scored on the HTTP path, not as hashing hits.
            let hashing_fair = matches!(
                row.tag,
                ProbeTag::Exact
                    | ProbeTag::Temporal
                    | ProbeTag::KnowledgeUpdate
                    | ProbeTag::Abstain
                    | ProbeTag::ScopeIsolation
                    | ProbeTag::Uncertainty
            );
            if hashing_fair {
                assert!(
                    row.hit_at_k,
                    "{} tag={:?} keys={:?} leak={} search_ms={:?}",
                    row.probe, row.tag, row.hit_keys, row.leaked, row.search_ms
                );
            }
        }
        eprintln!("{}", report.miss_markdown());
    }

    #[test]
    fn sota_fixture_json_loads() {
        let suite: EvalSuite = serde_json::from_str(include_str!("../fixtures/sota_plane_v1.json"))
            .expect("load fixture");
        assert_eq!(suite.name, "sota_plane_v1");
        assert_eq!(suite.probes.len(), channel_suite().probes.len());
        assert!(suite.probes.iter().any(|p| p.tag == Some(ProbeTag::Exact)));
        assert!(suite
            .probes
            .iter()
            .any(|p| p.tag == Some(ProbeTag::MultiHop) && p.skip_if_no_graph));
    }
}

/// Label **http**: paraphrase probe via OpenAI-compatible embeddings.
///
/// Returns `Ok(None)` when `VEGAPUNK_EMBED_URL` is unset so `just check` stays offline.
#[cfg(feature = "http")]
pub async fn run_labeled_http_paraphrase() -> nomiso_core::error::Result<Option<SuiteResult>> {
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

    let embedder = HttpEmbedder::new(HttpEmbedderConfig {
        base_url: url,
        api_key: key,
        model,
        dimension: dim,
    })?;
    let client = NomisoClient::connect(StoreConfig::memory_test(dim))
        .await?
        .with_embedder(Arc::new(embedder));

    let fact = "Alice prefers TypeScript for agent tooling and MCP servers.";
    let paraphrase = "What language does Alice like for building agents?";
    client
        .put(PutRequest {
            scope: "org/eval/http".into(),
            category: Category::Semantic,
            content: Content::text(fact),
            embedding: None,
            ..Default::default()
        })
        .await?;
    let hits = client
        .search(SearchQuery {
            query: paraphrase.into(),
            scope: "org/eval/http".into(),
            scope_match: ScopeMatch::Exact,
            as_of: None,
            known_as_of: None,
            sys_as_of: None,
            categories: None,
            limit: Some(5),
            embedding: None,
            graph_enrich: Some(false),
            graph_expand: None,
        })
        .await?;
    let passed = hits.iter().any(|h| h.preview.contains("TypeScript"));
    Ok(Some(SuiteResult {
        name: "http_paraphrase".into(),
        passed,
        probes: vec![ProbeResult {
            name: "paraphrase_hits_typescript_fact".into(),
            passed,
            detail: format!("hits={}", hits.len()),
            hit_keys: hits.iter().map(|h| h.id.to_string()).collect(),
            scores: hits.iter().map(|h| h.score).collect(),
        }],
    }))
}

#[cfg(all(test, feature = "http"))]
mod http_tests {
    use super::*;

    #[tokio::test]
    async fn labeled_http_paraphrase() {
        match run_labeled_http_paraphrase().await.expect("run") {
            None => eprintln!("skip: VEGAPUNK_EMBED_URL unset"),
            Some(res) => assert!(
                res.passed,
                "HTTP paraphrase must hit TypeScript fact: {res:?}"
            ),
        }
    }

    #[tokio::test]
    async fn channel_scoreboard_http_if_configured() {
        match run_sota_plane_http(&channel_suite())
            .await
            .expect("http compare")
        {
            None => eprintln!("skip: VEGAPUNK_EMBED_URL unset"),
            Some(report) => {
                eprintln!("{}", report.miss_markdown());
                for row in report.rows.iter().filter(|r| r.system == "nomiso") {
                    if row.skipped {
                        assert_eq!(row.tag, ProbeTag::MultiHop, "{}", row.probe);
                        continue;
                    }
                    assert!(
                        row.hit_at_k,
                        "{} tag={:?} keys={:?} leak={}",
                        row.probe, row.tag, row.hit_keys, row.leaked
                    );
                }
                assert!(report.passed, "nomiso HTTP non-skipped probes must pass");
            }
        }
    }

    #[tokio::test]
    async fn coding_agent_plane_v2_http() {
        let suite = coding_agent_suite().expect("compile coding pack");
        match run_sota_plane_http(&suite).await.expect("http compare") {
            None => eprintln!("skip: VEGAPUNK_EMBED_URL unset"),
            Some(report) => {
                eprintln!("{}", report.miss_markdown());
                for row in report.rows.iter().filter(|r| r.system == "nomiso") {
                    if row.skipped {
                        assert_eq!(row.tag, ProbeTag::MultiHop, "{}", row.probe);
                        continue;
                    }
                    assert!(
                        row.hit_at_k,
                        "{} tag={:?} keys={:?} leak={}",
                        row.probe, row.tag, row.hit_keys, row.leaked
                    );
                }
                assert!(
                    report.passed,
                    "nomiso HTTP coding v2 non-skipped probes must pass"
                );
            }
        }
    }
}
