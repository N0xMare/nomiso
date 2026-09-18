//! Memory **reader** plane: multi-pass hard recall + context pack + abstain.
//!
//! No auto-inject into prompts. Hosts decide whether to place [`ContextPack::block`]
//! into the agent context. Optional [`QueryRewriter`] is BYOM (defaults to rule rewrite).

use async_trait::async_trait;
use nomiso_core::{rrf_fuse, ScopeMatch, ScoreKind, SearchHit};
use nomiso_service::NomisoClient;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::error::{Error, Result};
use crate::policy::MemoryPolicy;
use crate::recall::{recall, RecallOptions};

/// BYOM query rewrite for hard-recall pass 2+.
#[async_trait]
pub trait QueryRewriter: Send + Sync {
    async fn rewrite(&self, query: &str) -> Result<Vec<String>>;
}

/// Deterministic rewrites: original + de-stopped + keyword bag.
#[derive(Debug, Default, Clone)]
pub struct RuleQueryRewriter;

#[async_trait]
impl QueryRewriter for RuleQueryRewriter {
    async fn rewrite(&self, query: &str) -> Result<Vec<String>> {
        let q = query.trim();
        if q.is_empty() {
            return Ok(vec![]);
        }
        let mut out = vec![q.to_string()];
        let tokens: Vec<&str> = q
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|t| t.len() > 2)
            .filter(|t| !STOP.contains(&t.to_ascii_lowercase().as_str()))
            .collect();
        if !tokens.is_empty() {
            let bag = tokens.join(" ");
            if bag != q {
                out.push(bag);
            }
            // longest token alone (often an error code or proper noun)
            if let Some(max) = tokens.iter().max_by_key(|t| t.len()) {
                if max.len() >= 6 {
                    let s = (*max).to_string();
                    if !out.contains(&s) {
                        out.push(s);
                    }
                }
            }
        }
        Ok(out)
    }
}

const STOP: &[&str] = &[
    "the", "and", "for", "with", "from", "that", "this", "what", "when", "where", "which", "have",
    "has", "was", "were", "are", "about", "into", "your", "our",
];

/// Options for hard multi-pass recall.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardRecallOptions {
    /// Max hits after fusion.
    pub limit: u32,
    /// Max passes (query variants).
    pub max_passes: u32,
    /// Abstain if fewer than this many hits.
    pub min_hits: usize,
    /// Abstain if best engine score is below this floor (None = disabled).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_score: Option<f64>,
    /// Scope match.
    #[serde(default)]
    pub scope_match: ScopeMatch,
    /// Optional query embedding (first pass only).
    #[serde(default)]
    pub embedding: Option<Vec<f32>>,
    /// Valid-time lens forwarded to every pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_of: Option<nomiso_core::Timestamp>,
    /// Known-time lens forwarded to every pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_as_of: Option<nomiso_core::Timestamp>,
    /// System-time lens forwarded to every pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sys_as_of: Option<nomiso_core::Timestamp>,
    /// Optional category filter forwarded to every pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub categories: Option<Vec<nomiso_core::Category>>,
    /// Opt-in bounded graph candidate expansion (T5 experiment, plane field).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_expand: Option<nomiso_core::GraphExpand>,
}

impl HardRecallOptions {
    pub fn from_policy(policy: impl Into<MemoryPolicy>) -> Self {
        let policy = policy.into();
        Self {
            limit: policy.default_recall_limit,
            max_passes: 3,
            min_hits: 1,
            min_score: Some(0.0), // only filter empty/negative; products raise floor
            scope_match: ScopeMatch::Exact,
            embedding: None,
            as_of: None,
            known_as_of: None,
            sys_as_of: None,
            categories: None,
            graph_expand: None,
        }
    }
}

/// Multi-pass hard recall result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardRecallResult {
    /// Queries actually run.
    pub queries: Vec<String>,
    /// Fused hits (best score per id).
    pub hits: Vec<SearchHit>,
    /// True when reader recommends not injecting.
    pub abstained: bool,
    /// Why abstained (if any).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abstain_reason: Option<String>,
    /// Correlation id when product emitted plane traces (hard_recall_pack).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
}

/// Packed context for optional host injection (never auto-applied).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextPack {
    /// Whether host should skip injection.
    pub abstained: bool,
    pub reason: Option<String>,
    /// Structured cards.
    pub cards: Vec<ContextCard>,
    /// Ready-to-paste markdown block for system/user prompt.
    pub block: String,
    /// Rough token estimate (~4 chars/token).
    pub estimated_tokens: usize,
}

/// One card in a pack.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextCard {
    pub id: String,
    pub score: f64,
    pub category: String,
    pub scope: String,
    pub preview: String,
    pub version: u64,
    #[serde(default)]
    pub reference_only: bool,
    #[serde(default)]
    pub original_bytes: usize,
}

/// Hard multi-pass recall: rewrite variants → search → fuse by id.
pub async fn hard_recall(
    client: &NomisoClient,
    scope: &str,
    query: &str,
    opts: HardRecallOptions,
    rewriter: &dyn QueryRewriter,
) -> Result<HardRecallResult> {
    let mut queries = rewriter.rewrite(query).await?;
    if queries.is_empty() {
        return Err(Error::invalid("hard_recall requires a non-empty query"));
    }
    queries.truncate(opts.max_passes as usize);

    // Collect per-pass ranked id lists for RRF; keep best SearchHit card per id.
    let mut ranked_lists: Vec<Vec<String>> = Vec::new();
    let mut cards: HashMap<String, SearchHit> = HashMap::new();
    for (i, q) in queries.iter().enumerate() {
        let mut ro = RecallOptions {
            limit: opts.limit,
            scope_match: opts.scope_match,
            embedding: if i == 0 { opts.embedding.clone() } else { None },
            graph_enrich: false,
            graph_expand: opts.graph_expand.clone(),
            as_of: opts.as_of,
            known_as_of: opts.known_as_of,
            sys_as_of: opts.sys_as_of,
            categories: opts.categories.clone(),
        };
        if i > 0 {
            ro.limit = (opts.limit + 4).min(32);
        }
        let hits = recall(client, scope, q, ro).await?;
        let mut ranking = Vec::new();
        for h in hits {
            let key = h.id.as_str().to_string();
            ranking.push(key.clone());
            match cards.get(&key) {
                Some(prev) if prev.score >= h.score => {}
                _ => {
                    cards.insert(key, h);
                }
            }
        }
        if !ranking.is_empty() {
            ranked_lists.push(ranking);
        }
    }

    // Rank-based multipass fusion (product policy; not max-score across passes).
    let fused = if ranked_lists.is_empty() {
        vec![]
    } else {
        rrf_fuse(&ranked_lists, 60)
    };
    let multipass = ranked_lists.len() > 1;
    let mut hits: Vec<SearchHit> = Vec::new();
    // Floor abstention uses plane channel scores (BM25/hybrid), not pure RRF rank mass
    // (~1/(k+rank)), so hosts can set BM25-scale floors that still make sense multipass.
    let mut best_plane_score = 0.0_f64;
    for (id, rrf_score) in fused.into_iter().take(opts.limit as usize) {
        if let Some(mut h) = cards.remove(&id) {
            best_plane_score = best_plane_score.max(h.score);
            // Surface fused rank score as Engine RRF-scale when multi-pass.
            if multipass {
                h.score = rrf_score;
                h.score_kind = ScoreKind::Engine;
            }
            hits.push(h);
        }
    }

    let score_for_floor = if multipass {
        best_plane_score
    } else {
        hits.first().map(|h| h.score).unwrap_or(0.0)
    };
    let (abstained, abstain_reason) = if hits.len() < opts.min_hits {
        (
            true,
            Some(format!(
                "fewer than {} hits after {} passes",
                opts.min_hits,
                queries.len()
            )),
        )
    } else if opts
        .min_score
        .is_some_and(|floor| floor > 0.0 && score_for_floor < floor)
    {
        (
            true,
            Some(format!(
                "best plane score {score_for_floor:.4} below min_score floor {:?}",
                opts.min_score
            )),
        )
    } else {
        (false, None)
    };

    Ok(HardRecallResult {
        trace_id: None,
        queries,
        hits,
        abstained,
        abstain_reason,
    })
}

/// Pack whole hits and bounded reference-only cards (host decides).
pub fn pack_context(hits: &[SearchHit], max_cards: usize, max_chars: usize) -> ContextPack {
    if hits.is_empty() {
        return ContextPack {
            abstained: true,
            reason: Some("no hits".into()),
            cards: vec![],
            block: String::new(),
            estimated_tokens: 0,
        };
    }

    let mut cards = Vec::new();
    let mut body = String::from("## Nomiso memory (explicit pack)\n");
    body.push_str("Use only if relevant. Cite ids. Do not invent facts.\n\n");

    let mut deferred: Vec<&SearchHit> = Vec::new();
    for h in hits {
        if cards.len() >= max_cards {
            break;
        }
        let card = card_from_hit(h, h.preview.clone(), false);
        let line = card_line(&card);
        if body.len() + line.len() <= max_chars {
            body.push_str(&line);
            cards.push(card);
        } else {
            deferred.push(h);
        }
    }
    for h in deferred {
        if cards.len() >= max_cards {
            break;
        }
        let card = card_from_hit(
            h,
            format!(
                "Full content omitted ({} bytes); read this memory with history/read before relying on it.",
                h.preview.len()
            ),
            true,
        );
        let line = card_line(&card);
        if body.len() + line.len() <= max_chars {
            body.push_str(&line);
            cards.push(card);
        }
    }

    if cards.is_empty() {
        return ContextPack {
            abstained: true,
            reason: Some("budget too small".into()),
            cards: vec![],
            block: String::new(),
            estimated_tokens: 0,
        };
    }

    let estimated_tokens = body.len().div_ceil(4);
    ContextPack {
        abstained: false,
        reason: None,
        cards,
        block: body,
        estimated_tokens,
    }
}

fn card_from_hit(h: &SearchHit, preview: String, reference_only: bool) -> ContextCard {
    ContextCard {
        id: h.id.to_string(),
        score: h.score,
        category: h.category.as_str().to_string(),
        scope: h.scope.clone(),
        preview,
        version: h.version,
        reference_only,
        original_bytes: h.preview.len(),
    }
}

fn card_line(card: &ContextCard) -> String {
    format!(
        "- [{}] ({}) score={:.4}: {}\n",
        card.id, card.category, card.score, card.preview
    )
}

/// Hard recall then pack.
pub async fn hard_recall_pack(
    client: &NomisoClient,
    scope: &str,
    query: &str,
    opts: HardRecallOptions,
    rewriter: &dyn QueryRewriter,
    max_cards: usize,
    max_chars: usize,
) -> Result<(HardRecallResult, ContextPack)> {
    let result = hard_recall(client, scope, query, opts, rewriter).await?;
    let pack = if result.abstained {
        ContextPack {
            abstained: true,
            reason: result.abstain_reason.clone(),
            cards: vec![],
            block: String::new(),
            estimated_tokens: 0,
        }
    } else {
        pack_context(&result.hits, max_cards, max_chars)
    };
    Ok((result, pack))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nomiso_core::Category;

    fn hit(id: &str, preview: String) -> SearchHit {
        SearchHit {
            id: nomiso_core::MemoryId::new(id),
            score: 0.5,
            score_kind: ScoreKind::Engine,
            signals: Default::default(),
            preview,
            category: Category::Semantic,
            scope: "org/test".into(),
            valid_from: nomiso_core::Timestamp::now(),
            valid_until: None,
            provenance: Default::default(),
            version: 1,
            entities: vec![],
            embedding_generation: None,
            expansion: None,
        }
    }

    #[tokio::test]
    async fn rule_rewriter_expands() {
        let r = RuleQueryRewriter;
        let q = r
            .rewrite("What language does Alice prefer for TypeScript tooling?")
            .await
            .unwrap();
        assert!(q.len() >= 2);
        assert!(q[0].contains("Alice") || q[0].contains("TypeScript"));
    }

    #[test]
    fn pack_defers_oversized_hit_as_reference_only() {
        let long = "x".repeat(5000);
        let hits = vec![
            hit("memory:big", long.clone()),
            hit("memory:small", "short durable fact".into()),
        ];
        let pack = pack_context(&hits, 2, 512);
        assert!(!pack.abstained, "{:?}", pack.reason);
        assert_eq!(pack.cards.len(), 2, "{:?}", pack.cards);
        let whole = pack
            .cards
            .iter()
            .find(|c| c.id == "memory:small")
            .expect("short card must be included whole");
        assert!(!whole.reference_only);
        assert_eq!(whole.preview, "short durable fact");
        assert_eq!(whole.original_bytes, "short durable fact".len());
        let reference = pack
            .cards
            .iter()
            .find(|c| c.id == "memory:big")
            .expect("oversized hit must be emitted as reference-only");
        assert!(reference.reference_only);
        assert_eq!(reference.original_bytes, 5000);
        assert_eq!(
            reference.preview,
            "Full content omitted (5000 bytes); read this memory with history/read before relying on it."
        );
        assert!(pack.block.len() <= 512, "block={} bytes", pack.block.len());
        assert!(!pack.block.contains(&long), "no truncated content allowed");
        assert!(pack.block.contains("memory:small"));
        assert!(pack.block.contains("memory:big"));
    }

    #[test]
    fn pack_single_oversized_hit_is_bounded_reference() {
        let long = "very important detail ".repeat(250);
        let bytes = long.len();
        let hits = vec![hit("memory:long", long.clone())];
        let pack = pack_context(&hits, 4, 512);
        assert!(!pack.abstained, "{:?}", pack.reason);
        assert_eq!(pack.cards.len(), 1, "{:?}", pack.cards);
        let card = &pack.cards[0];
        assert!(card.reference_only);
        assert_eq!(card.original_bytes, bytes);
        assert!(pack.block.len() <= 512);
        assert!(pack
            .block
            .contains(&format!("Full content omitted ({bytes} bytes)")));
        assert!(
            !pack.block.contains("very important detail"),
            "original long text must not appear in the block"
        );
    }

    #[test]
    fn pack_unicode_oversized_hit_never_slices_text() {
        let long = "漢字メモリ".repeat(1200);
        let bytes = long.len();
        let hits = vec![hit("memory:uni", long.clone())];
        let pack = pack_context(&hits, 2, 512);
        assert!(!pack.abstained, "{:?}", pack.reason);
        assert_eq!(pack.cards.len(), 1, "{:?}", pack.cards);
        assert!(pack.cards[0].reference_only);
        assert_eq!(pack.cards[0].original_bytes, bytes);
        assert!(pack.block.len() <= 512);
        assert!(!pack.block.contains('漢'), "no unicode fragment in block");
    }

    #[test]
    fn pack_respects_max_cards_for_reference_cards() {
        let hits = vec![
            hit("memory:a", "a".repeat(5000)),
            hit("memory:b", "b".repeat(5000)),
            hit("memory:c", "c".repeat(5000)),
        ];
        let pack = pack_context(&hits, 2, 4096);
        assert!(!pack.abstained, "{:?}", pack.reason);
        assert!(pack.cards.len() <= 2, "{:?}", pack.cards);
        assert!(pack.cards.iter().all(|c| c.reference_only));
        assert!(pack.block.len() <= 4096);
    }

    #[test]
    fn pack_max_cards_zero_abstains() {
        let hits = vec![hit("memory:x", "tiny fact".into())];
        let pack = pack_context(&hits, 0, 4096);
        assert!(pack.abstained);
        assert!(pack.cards.is_empty());
        assert!(pack.block.is_empty());
    }

    #[test]
    fn pack_budget_smaller_than_header_abstains() {
        let hits = vec![hit("memory:x", "tiny fact".into())];
        let pack = pack_context(&hits, 4, 10);
        assert!(pack.abstained);
        assert!(pack.cards.is_empty());
        assert!(pack.block.is_empty());
    }

    #[test]
    fn pack_reference_skipped_when_reference_line_does_not_fit() {
        let header = "## Nomiso memory (explicit pack)\nUse only if relevant. Cite ids. Do not invent facts.\n\n";
        let hits = vec![
            hit("memory:small", "short durable fact".into()),
            hit("memory:big", "b".repeat(5000)),
        ];
        let budget = header.len() + 64;
        let pack = pack_context(&hits, 4, budget);
        assert!(!pack.abstained, "{:?}", pack.reason);
        assert_eq!(pack.cards.len(), 1, "{:?}", pack.cards);
        assert_eq!(pack.cards[0].id, "memory:small");
        assert!(!pack.cards[0].reference_only);
        assert!(pack.block.len() <= budget);
    }
}
