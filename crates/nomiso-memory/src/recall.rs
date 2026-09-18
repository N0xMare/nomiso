//! Explicit recall path (never auto-injects into prompts).

use nomiso_core::{Category, ScopeMatch, SearchHit, SearchQuery, Timestamp};
use nomiso_service::NomisoClient;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::policy::MemoryPolicy;

/// Options for a recall call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallOptions {
    /// Max hits.
    pub limit: u32,
    /// Scope match mode.
    #[serde(default)]
    pub scope_match: ScopeMatch,
    /// Optional query embedding.
    #[serde(default)]
    pub embedding: Option<Vec<f32>>,
    /// Graph enrich.
    #[serde(default)]
    pub graph_enrich: bool,
    /// Valid-time lens (plane `as_of`). None = now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_of: Option<Timestamp>,
    /// Known-time lens (what we knew as of K).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_as_of: Option<Timestamp>,
    /// System-time lens (store as of U).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sys_as_of: Option<Timestamp>,
    /// Optional category filter (same plane field as list/count). Default: all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub categories: Option<Vec<Category>>,
    /// Opt-in bounded graph candidate expansion (T5 experiment, plane field).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_expand: Option<nomiso_core::GraphExpand>,
}

/// Filters for `list` / `count` (optional text + category + tri-temporal lenses).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EnumerateOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_of: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_as_of: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sys_as_of: Option<Timestamp>,
    /// Optional category filter (plane `ListRequest.categories`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub categories: Option<Vec<Category>>,
    /// Resume enumeration from a previously returned `next_cursor` (list only;
    /// rejected by `count_with`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<nomiso_core::ListCursor>,
}

/// Parse an RFC3339 / jiff timestamp for product temporal lenses.
pub fn parse_timestamp(s: &str) -> Result<Timestamp> {
    s.parse()
        .map_err(|e| crate::Error::invalid(format!("timestamp '{s}': {e}")))
}

impl RecallOptions {
    /// Defaults from profile policy.
    pub fn from_policy(policy: impl Into<MemoryPolicy>) -> Self {
        let policy = policy.into();
        Self {
            limit: policy.default_recall_limit,
            scope_match: ScopeMatch::Exact,
            embedding: None,
            graph_enrich: policy.graph_enrich,
            as_of: None,
            known_as_of: None,
            sys_as_of: None,
            categories: None,
            graph_expand: None,
        }
    }
}

pub async fn recall(
    client: &NomisoClient,
    scope: &str,
    query: &str,
    opts: RecallOptions,
) -> Result<Vec<SearchHit>> {
    let q = query.trim();
    if q.is_empty() && opts.embedding.is_none() {
        return Err(crate::Error::invalid(
            "recall requires non-empty query and/or embedding",
        ));
    }
    let hits = client
        .search(SearchQuery {
            query: q.to_string(),
            scope: scope.to_string(),
            scope_match: opts.scope_match,
            as_of: opts.as_of,
            known_as_of: opts.known_as_of,
            sys_as_of: opts.sys_as_of,
            categories: opts.categories,
            limit: Some(opts.limit.max(1)),
            embedding: opts.embedding,
            graph_enrich: Some(opts.graph_enrich),
            graph_expand: opts.graph_expand.clone(),
        })
        .await?;
    Ok(hits)
}
