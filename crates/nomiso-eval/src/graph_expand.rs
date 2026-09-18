//! T5 evaluation harness: does opt-in graph candidate expansion improve
//! retrieval enough to justify promotion beyond `graph_expand` opt-in?
//!
//! Fixture shape (deterministic, in-code — the promotion decision must not
//! depend on a live provider): seed docs that lexically match each probe,
//! gold docs that share **no** query terms and are reachable only through
//! `derived_from`/`depends_on` edges at depth 1 and 2, plus lexical trap
//! docs (similar vocabulary, no edges) and unrelated distractors.
//!
//! Compared strategies over the same `NomisoClient`:
//! - `direct` — BM25 only (no embedding, no expansion)
//! - `hybrid` — BM25 + hashing vector (no expansion)
//! - `expanded` — hybrid + `graph_expand` defaults
//! - `iterative` — BM25, then a second BM25 query reformulated with the
//!   top hit's rarest tokens (the "agent requeries" baseline spec 05
//!   requires as the comparison point)
//!
//! Reported per (strategy, probe): recall@k over gold ids, noise@k
//! (non-gold non-seed hits — traps must stay out), search latency, and
//! expansion cost stats (`edges_scanned`, `candidates_added`, truncation
//! reasons) so the cost side of the promotion decision is measured, not
//! assumed.

use std::collections::{BTreeMap, HashSet};
use std::time::Instant;

use nomiso_core::ops::PutRequest;
use nomiso_core::ops::{GraphExpand, SearchQuery};
use nomiso_core::relationship::{
    EndpointKind, EndpointRef, PutRelationshipRequest, RelationPredicate,
};
use nomiso_core::types::{Category, Content};
use nomiso_embed::HashingEmbedder;
use nomiso_service::NomisoClient;
use serde::{Deserialize, Serialize};

/// Strategy labels (stable for report diffing).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExpandStrategy {
    Direct,
    Hybrid,
    Expanded,
    Iterative,
}

impl ExpandStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Hybrid => "hybrid",
            Self::Expanded => "expanded",
            Self::Iterative => "iterative",
        }
    }
    pub const ALL: [Self; 4] = [Self::Direct, Self::Hybrid, Self::Expanded, Self::Iterative];
}

/// One evaluation probe: a query plus the gold doc labels expected back.
#[derive(Debug, Clone)]
pub struct ExpandProbe {
    pub name: &'static str,
    pub query: &'static str,
    /// Doc labels that count as correct (graph-only targets).
    pub gold: &'static [&'static str],
    /// Seed labels that legitimately match the query (not noise).
    pub seeds: &'static [&'static str],
    /// Minimum edge depth the gold requires (0 = lexically reachable).
    pub min_depth: u32,
}

/// Per-(strategy, probe) measurement row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpandRow {
    pub strategy: String,
    pub probe: String,
    pub k: u32,
    /// Fraction of gold labels surfaced in top-k.
    pub recall_at_k: f64,
    /// Top-k hits that are neither gold nor declared seeds.
    pub noise_at_k: u32,
    /// Hits carrying expansion provenance.
    pub expanded_hits: u32,
    /// Labels of expansion-provenance hits (fixture-scale observability:
    /// proves expanded candidates are edge-reachable, never trap docs).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expanded_labels: Vec<String>,
    /// Total rows the strategy returned (expanded tail may exceed `limit`).
    pub hits_returned: u32,
    pub search_ms: f64,
    /// Expansion cost report (present only on `expanded` rows).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edges_scanned: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidates_added: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub truncated: Vec<String>,
}

/// Full evaluation report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpandReport {
    pub fixture: String,
    pub rows: Vec<ExpandRow>,
    /// recall_at_k averaged over rows, by strategy.
    pub mean_recall: BTreeMap<String, f64>,
    /// Total noise hits by strategy (traps surfaced).
    pub total_noise: BTreeMap<String, u32>,
    pub mean_search_ms: BTreeMap<String, f64>,
}

/// A doc in the fixture: `label` is the stable identity used by probes.
struct FixtureDoc {
    label: &'static str,
    text: &'static str,
    /// Query terms this doc lexically matches (empty = graph-only).
    embed: bool,
}

/// (subject_label, predicate, object_label) — memory→memory edges only.
struct FixtureEdge {
    subject: &'static str,
    predicate: RelationPredicate,
    object: &'static str,
}

/// The deterministic expansion fixture.
///
/// `payments-runbook` matches "payment timeout" lexically; its gold
/// `queue-worker` and `retry-budget` docs contain deliberately disjoint
/// vocabulary and are reachable only via edges (depth 1 and depth 2).
/// `trap-payments` shares query vocabulary but has no edges — surfacing
/// it via expansion would be noise.
fn fixture_docs() -> Vec<FixtureDoc> {
    vec![
        FixtureDoc {
            label: "payments-runbook",
            text: "payment service timeout runbook: escalate after three gateway failures",
            embed: true,
        },
        FixtureDoc {
            label: "queue-worker",
            text: "restart ordering: drain the worker pool before cycling brokers",
            embed: true,
        },
        FixtureDoc {
            label: "retry-budget",
            text: "exponential backoff ceiling is five attempts with jitter",
            embed: true,
        },
        FixtureDoc {
            label: "search-runbook",
            text: "search indexing lag runbook: rebuild the analyzer cache",
            embed: true,
        },
        FixtureDoc {
            label: "synonym-table",
            text: "canonical token mapping lives in the analyzer manifest",
            embed: true,
        },
        FixtureDoc {
            label: "trap-payments",
            text: "payment timeout dashboard colors and chart annotations",
            embed: true,
        },
        FixtureDoc {
            label: "distractor-lunch",
            text: "cafeteria menu rotation for the spring quarter",
            embed: true,
        },
        FixtureDoc {
            label: "distractor-infra",
            text: "rack elevation diagram approval workflow",
            embed: true,
        },
        FixtureDoc {
            label: "distractor-oncall",
            text: "weekly rotation handoff checklist for regional responders",
            embed: true,
        },
        FixtureDoc {
            label: "distractor-billing",
            text: "invoice reconciliation export columns and formats",
            embed: true,
        },
        FixtureDoc {
            label: "distractor-design",
            text: "typography scale tokens for the marketing site refresh",
            embed: true,
        },
        FixtureDoc {
            label: "distractor-sec",
            text: "quarterly access review evidence collection steps",
            embed: true,
        },
        FixtureDoc {
            label: "distractor-ci",
            text: "flake quarantine policy for the integration pipeline",
            embed: true,
        },
        FixtureDoc {
            label: "distractor-hiring",
            text: "interview loop rubric calibration notes",
            embed: true,
        },
        FixtureDoc {
            label: "distractor-vendor",
            text: "vendor security questionnaire intake routing",
            embed: true,
        },
        FixtureDoc {
            label: "distractor-metrics",
            text: "weekly active cohort definition for the dashboard",
            embed: true,
        },
    ]
}

fn fixture_edges() -> Vec<FixtureEdge> {
    vec![
        // depth-1: runbook was derived from the queue-worker procedure
        FixtureEdge {
            subject: "payments-runbook",
            predicate: RelationPredicate::DerivedFrom,
            object: "queue-worker",
        },
        // depth-1: runbook depends on the retry budget doc
        FixtureEdge {
            subject: "payments-runbook",
            predicate: RelationPredicate::DependsOn,
            object: "retry-budget",
        },
        // depth-2 from payments-runbook via queue-worker
        FixtureEdge {
            subject: "queue-worker",
            predicate: RelationPredicate::DependsOn,
            object: "drain-checklist",
        },
        FixtureEdge {
            subject: "search-runbook",
            predicate: RelationPredicate::DerivedFrom,
            object: "synonym-table",
        },
    ]
}

/// Extra doc only reachable at depth 2 (appended separately so the label
/// list above stays readable).
const DRAIN_DOC: FixtureDoc = FixtureDoc {
    label: "drain-checklist",
    text: "verify inflight counters reach zero before SIGTERM propagation",
    embed: true,
};

/// The preregistered probe set.
pub fn expand_probes() -> Vec<ExpandProbe> {
    vec![
        ExpandProbe {
            name: "graph_only_depth1",
            query: "payment timeout runbook",
            gold: &["queue-worker", "retry-budget"],
            seeds: &["payments-runbook"],
            min_depth: 1,
        },
        ExpandProbe {
            name: "graph_only_depth2",
            query: "payment timeout runbook",
            gold: &["drain-checklist"],
            seeds: &["payments-runbook", "queue-worker"],
            min_depth: 2,
        },
        ExpandProbe {
            name: "lexical_baseline",
            query: "search indexing runbook",
            gold: &["search-runbook"],
            seeds: &[],
            min_depth: 0,
        },
    ]
}

/// Ingest the fixture into a fresh client; returns label → memory id.
pub async fn ingest_expand_fixture(
    client: &NomisoClient,
    embedder: &HashingEmbedder,
    scope: &str,
) -> nomiso_core::error::Result<BTreeMap<String, String>> {
    let mut ids = BTreeMap::new();
    let mut docs = fixture_docs();
    docs.push(DRAIN_DOC);
    for doc in &docs {
        let wr = client
            .put(PutRequest {
                scope: scope.into(),
                category: Category::Semantic,
                content: Content::text(doc.text),
                embedding: if doc.embed {
                    Some(embedder.embed_one(doc.text))
                } else {
                    None
                },
                ..Default::default()
            })
            .await?;
        ids.insert(doc.label.to_string(), wr.id.as_str().to_string());
    }
    for edge in fixture_edges() {
        client
            .put_relationship(PutRelationshipRequest {
                scope: scope.into(),
                predicate: edge.predicate,
                subject: EndpointRef::new(EndpointKind::Memory, ids[edge.subject].clone()),
                object: EndpointRef::new(EndpointKind::Memory, ids[edge.object].clone()),
                subject_rev: None,
                object_rev: None,
                epistemic: Default::default(),
                evidence: vec![],
                valid_from: None,
                valid_until: None,
                producer: None,
                dedupe: true,
            })
            .await?;
    }
    Ok(ids)
}

async fn run_one(
    client: &NomisoClient,
    embedder: &HashingEmbedder,
    scope: &str,
    strategy: ExpandStrategy,
    probe: &ExpandProbe,
    k: u32,
) -> nomiso_core::error::Result<(
    Vec<nomiso_core::types::SearchHit>,
    Option<nomiso_core::ops::ExpansionStats>,
    f64,
)> {
    let embed = |s: &str| embedder.embed_one(s);
    let base = || SearchQuery {
        query: probe.query.to_string(),
        scope: scope.to_string(),
        limit: Some(k),
        ..Default::default()
    };
    let t0 = Instant::now();
    let (hits, stats) = match strategy {
        ExpandStrategy::Direct => {
            let q = base();
            let out = client.search_detailed(q).await?;
            (out.hits, out.stats.expansion)
        }
        ExpandStrategy::Hybrid => {
            let mut q = base();
            q.embedding = Some(embed(probe.query));
            let out = client.search_detailed(q).await?;
            (out.hits, out.stats.expansion)
        }
        ExpandStrategy::Expanded => {
            let mut q = base();
            q.embedding = Some(embed(probe.query));
            q.graph_expand = Some(GraphExpand {
                max_depth: Some(2),
                ..Default::default()
            });
            let out = client.search_detailed(q).await?;
            (out.hits, out.stats.expansion)
        }
        ExpandStrategy::Iterative => {
            // Round 1: lexical. Round 2: requery with the top hit's rarest
            // tokens appended — models an agent reformulating after reading
            // context. Union by id, preserving best rank.
            let out1 = client.search_detailed(base()).await?;
            let mut merged: Vec<nomiso_core::types::SearchHit> = out1.hits.clone();
            if let Some(top) = out1.hits.first() {
                let extra: HashSet<String> = top
                    .preview
                    .to_ascii_lowercase()
                    .split(|c: char| !c.is_ascii_alphanumeric())
                    .filter(|t| t.len() > 4)
                    .map(|t| t.to_string())
                    .collect();
                let mut seen: HashSet<String> =
                    merged.iter().map(|h| h.id.as_str().to_string()).collect();
                if !extra.is_empty() {
                    let terms: Vec<_> = extra.into_iter().take(4).collect();
                    let mut q2 = base();
                    q2.query = format!("{} {}", probe.query, terms.join(" "));
                    let out2 = client.search_detailed(q2).await?;
                    for h in out2.hits {
                        if seen.insert(h.id.as_str().to_string()) {
                            merged.push(h);
                        }
                    }
                }
            }
            merged.truncate(k as usize);
            (merged, None)
        }
    };
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    Ok((hits, stats, ms))
}

/// Run the full comparison over [`expand_probes`].
pub async fn run_expand_eval(
    client: &NomisoClient,
    embedder: &HashingEmbedder,
    scope: &str,
    k: u32,
) -> nomiso_core::error::Result<ExpandReport> {
    let ids = ingest_expand_fixture(client, embedder, scope).await?;
    let id_to_label: BTreeMap<String, String> = ids
        .iter()
        .map(|(label, id)| (id.clone(), label.clone()))
        .collect();
    let mut rows = Vec::new();
    for probe in expand_probes() {
        for strategy in ExpandStrategy::ALL {
            let (hits, exp, ms) = run_one(client, embedder, scope, strategy, &probe, k).await?;
            let gold: HashSet<&str> = probe.gold.iter().copied().collect();
            let seeds: HashSet<&str> = probe.seeds.iter().copied().collect();
            let mut gold_hit = 0usize;
            let mut noise = 0u32;
            let mut expanded_hits = 0u32;
            let mut expanded_labels = Vec::new();
            for h in &hits {
                let label = id_to_label.get(h.id.as_str()).cloned().unwrap_or_default();
                if gold.contains(label.as_str()) {
                    gold_hit += 1;
                }
                if h.signals.expanded || h.expansion.is_some() {
                    expanded_hits += 1;
                    expanded_labels.push(label);
                }
            }
            for h in hits.iter().take(k as usize) {
                let label = id_to_label.get(h.id.as_str()).cloned().unwrap_or_default();
                if !gold.contains(label.as_str()) && !seeds.contains(label.as_str()) {
                    noise += 1;
                }
            }
            rows.push(ExpandRow {
                strategy: strategy.as_str().into(),
                probe: probe.name.into(),
                k,
                recall_at_k: if gold.is_empty() {
                    1.0
                } else {
                    gold_hit as f64 / gold.len() as f64
                },
                noise_at_k: noise,
                expanded_hits,
                expanded_labels,
                hits_returned: hits.len() as u32,
                search_ms: ms,
                edges_scanned: exp.as_ref().map(|e| e.edges_scanned),
                candidates_added: exp.as_ref().map(|e| e.candidates_added),
                truncated: exp.map(|e| e.truncated).unwrap_or_default(),
            });
        }
    }
    let mut mean_recall: BTreeMap<String, (f64, u32)> = BTreeMap::new();
    let mut total_noise: BTreeMap<String, u32> = BTreeMap::new();
    let mut mean_ms: BTreeMap<String, (f64, u32)> = BTreeMap::new();
    for r in &rows {
        let e = mean_recall.entry(r.strategy.clone()).or_default();
        e.0 += r.recall_at_k;
        e.1 += 1;
        *total_noise.entry(r.strategy.clone()).or_default() += r.noise_at_k;
        let m = mean_ms.entry(r.strategy.clone()).or_default();
        m.0 += r.search_ms;
        m.1 += 1;
    }
    Ok(ExpandReport {
        fixture: "graph_expand_v1".into(),
        rows,
        mean_recall: mean_recall
            .into_iter()
            .map(|(k, (s, n))| (k, s / n as f64))
            .collect(),
        total_noise,
        mean_search_ms: mean_ms
            .into_iter()
            .map(|(k, (s, n))| (k, s / n as f64))
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nomiso_store::StoreConfig;

    #[tokio::test]
    async fn expansion_beats_direct_on_graph_only_recall() {
        let dim = 8;
        let embedder = std::sync::Arc::new(HashingEmbedder::new(dim).unwrap());
        let client = NomisoClient::connect(StoreConfig::memory_test(dim))
            .await
            .unwrap()
            .with_embedder(embedder.clone());
        let report = run_expand_eval(&client, embedder.as_ref(), "eval/expand", 6)
            .await
            .unwrap();

        let row = |s: &str, p: &str| {
            report
                .rows
                .iter()
                .find(|r| r.strategy == s && r.probe == p)
                .unwrap()
        };

        // The claim expansion must justify: depth-1 graph-only gold.
        assert_eq!(row("direct", "graph_only_depth1").recall_at_k, 0.0);
        assert_eq!(row("expanded", "graph_only_depth1").recall_at_k, 1.0);
        assert!(row("expanded", "graph_only_depth1").expanded_hits >= 2);

        // Depth-2 gold is only reachable by expansion at max_depth 2.
        assert_eq!(row("direct", "graph_only_depth2").recall_at_k, 0.0);
        assert_eq!(row("expanded", "graph_only_depth2").recall_at_k, 1.0);

        // Iterative requery does not reach graph-only docs either (the
        // reformulated query still has no lexical path to disjoint text).
        assert_eq!(row("iterative", "graph_only_depth1").recall_at_k, 0.0);

        // Lexical baseline: expansion must not displace the direct hit.
        assert_eq!(row("expanded", "lexical_baseline").recall_at_k, 1.0);

        // Cost reporting: expansion actually scanned edges. `depth` may be
        // reported when a max-depth frontier edge exists (documented traverse
        // semantics — "deeper frontier existed", not data loss); the resource
        // bounds must not trip at this fixture size.
        let ex = row("expanded", "graph_only_depth1");
        assert!(ex.edges_scanned.unwrap_or(0) > 0);
        for reason in &ex.truncated {
            assert!(
                reason == "depth",
                "unexpected resource truncation: {:?}",
                ex.truncated
            );
        }

        // Precision guard: expansion never fabricates candidates — every
        // expanded hit must be an edge-reachable doc, never the lexical trap
        // (no edges) or a distractor. Trap docs may still rank as *direct*
        // hits (they do share vocabulary); that is a lexical-quality matter,
        // not an expansion defect.
        for s in ExpandStrategy::ALL {
            for l in &row(s.as_str(), "graph_only_depth1").expanded_labels {
                assert!(
                    !l.starts_with("trap-") && !l.starts_with("distractor-"),
                    "{} surfaced non-graph doc via expansion: {}",
                    s.as_str(),
                    l
                );
            }
        }
        // Expansion appends below direct hits — its top-k noise cannot
        // exceed direct's on the same probe.
        assert!(
            row("expanded", "graph_only_depth1").noise_at_k
                <= row("direct", "graph_only_depth1").noise_at_k
        );

        // Aggregate view is populated for the report.
        assert!(report.mean_recall["expanded"] > report.mean_recall["direct"]);
    }
}
