//! Structured memory operations (`nomiso.op.v1`).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::relationship::{RelationPredicate, TraverseDirection};
use crate::scope::ScopeMatch;
use crate::types::{Category, Content, MemoryId, MemoryRecord, Provenance, Timestamp};

/// Insert a new structured memory record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct PutRequest {
    /// Scope path.
    pub scope: String,
    /// Category.
    pub category: Category,
    /// Content.
    pub content: Content,
    /// Valid-from (defaults to now if omitted at service layer).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub valid_from: Option<Timestamp>,
    /// Valid-until (open-ended if none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub valid_until: Option<Timestamp>,
    /// Known-at (defaults to now if omitted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub known_at: Option<Timestamp>,
    /// Optional confidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    /// Provenance.
    #[serde(default)]
    pub provenance: Provenance,
    /// Entity link ids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entity_links: Vec<String>,
    /// Optional embedding vector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<Vec<f32>>,
    /// Attested identity of the supplied/derived embedding (MIG-004). Set by
    /// the service layer when auto-embedding; callers supplying raw vectors
    /// may attest the producing model. Validated against the active embedding
    /// generation — a mismatch fails closed rather than silently mixing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_identity: Option<crate::embedding::EmbeddingIdentity>,
    /// Optional idempotency key (replay returns prior write when scope+key match).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// Optional extractor version stamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extractor_version: Option<String>,
    /// Optional model version stamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_version: Option<String>,
    /// Optional commit/rev validity start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_rev_from: Option<String>,
    /// Optional commit/rev validity end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_rev_until: Option<String>,
}

/// Assert a new fact and close/link a prior fact by id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SupersedeRequest {
    /// Prior memory id to close.
    pub prior_id: MemoryId,
    /// Expected optimistic version of the prior.
    pub expected_version: u64,
    /// New fact body (same shape as put).
    pub new: PutRequest,
    /// Optional override for prior `valid_until` (default: new.valid_from or now).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub close_at: Option<Timestamp>,
}

/// Hard caps for graph expansion into search (T5 experiment). Spec:
/// bound expansion before ranking, not just final output.
/// Deepest hop distance an expansion may reach.
pub const GRAPH_EXPAND_MAX_DEPTH: u32 = 2;
/// Most direct hits usable as expansion seeds.
pub const GRAPH_EXPAND_MAX_SEEDS: u32 = 16;
/// Most distinct expansion candidates merged into one search.
pub const GRAPH_EXPAND_MAX_CANDIDATES: u32 = 64;
/// Edge-scan budget: expansion may inspect at most this many edges total.
pub const GRAPH_EXPAND_MAX_EDGES: u32 = 256;

/// Opt-in graph candidate expansion for [`SearchQuery`] (T5 experiment).
///
/// Expansion treats the top direct hits as seeds and walks `active`
/// relationship edges (same scope filter, same temporal lenses) to surface
/// additional memory candidates. Expanded-only candidates are bounded before
/// ranking, appended below every direct hit with a labeled `RankFallback`
/// score, and carry [`crate::types::ExpansionInfo`] provenance — association never becomes
/// endorsement. Candidates already present as direct hits keep their rank and
/// gain the discovery path.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GraphExpand {
    /// Predicate allowlist; `None` uses the default traversable set
    /// (foundation-maintained predicates are never expanded).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicates: Option<Vec<RelationPredicate>>,
    /// Edge direction to follow (default: both).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<TraverseDirection>,
    /// Max hops from a seed (default 1, capped at [`GRAPH_EXPAND_MAX_DEPTH`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
    /// How many top direct hits may seed expansion (default 8, capped at
    /// [`GRAPH_EXPAND_MAX_SEEDS`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_seeds: Option<u32>,
    /// Max distinct expansion candidates discovered (default 32, capped at
    /// [`GRAPH_EXPAND_MAX_CANDIDATES`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_candidates: Option<u32>,
}

/// Expansion cost/truncation report (spec 05: report expansion cost).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExpansionStats {
    /// Direct hits used as expansion seeds.
    pub seeds: u32,
    /// Relationship rows inspected.
    pub edges_scanned: u32,
    /// Distinct in-scope, in-window candidates discovered.
    pub candidates_found: u32,
    /// Expansion-only candidates appended to the result.
    pub candidates_added: u32,
    /// Bounded reasons when expansion was cut short (`depth`, `candidates`,
    /// `edges`, `deadline`). Empty = not truncated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub truncated: Vec<String>,
    /// Wall time for the expansion pass.
    pub elapsed_ms: u64,
}

/// Retrieval cost channels beyond the hit list itself.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SearchStats {
    /// Expansion cost/truncation report; present iff `graph_expand` ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expansion: Option<ExpansionStats>,
}

/// Hits plus retrieval-cost reporting for controllers and evaluations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SearchOutcome {
    /// Ranked hits, direct first, expanded-only appended.
    pub hits: Vec<crate::types::SearchHit>,
    /// Retrieval cost channels (expansion cost/truncation when enabled).
    pub stats: SearchStats,
}

/// Hybrid search request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct SearchQuery {
    /// Free-text query (BM25). Empty allowed if embedding provided.
    #[serde(default)]
    pub query: String,
    /// Scope path.
    pub scope: String,
    /// Scope match mode (default exact).
    #[serde(default)]
    pub scope_match: ScopeMatch,
    /// Valid-time lens; `None` means "now".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub as_of: Option<Timestamp>,
    /// Known-time lens: only rows with known_at ≤ this instant (Phase 3e).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub known_as_of: Option<Timestamp>,
    /// System-time lens: rows current in the store as of this instant (Phase 3e).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub sys_as_of: Option<Timestamp>,
    /// Optional category filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub categories: Option<Vec<Category>>,
    /// Result limit (service applies defaults/caps).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Optional query embedding for vector channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<Vec<f32>>,
    /// Enable graph 1-hop enrich on results.
    #[serde(default)]
    pub graph_enrich: Option<bool>,
    /// Opt-in bounded graph candidate expansion (T5 experiment).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_expand: Option<GraphExpand>,
}

/// Find open-validity priors for Put vs Supersede (valid-now search).
///
/// Discovery only — the caller still chooses `prior_id` + `expected_version`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct FindCandidatesRequest {
    /// Free-text query (BM25). Empty allowed if embedding provided.
    #[serde(default)]
    pub query: String,
    /// Scope path.
    pub scope: String,
    /// Scope match mode (default exact).
    #[serde(default)]
    pub scope_match: ScopeMatch,
    /// Optional category filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub categories: Option<Vec<Category>>,
    /// Result limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Optional query embedding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<Vec<f32>>,
}

impl FindCandidatesRequest {
    /// Convert to a valid-now [`SearchQuery`] (no graph walk).
    pub fn into_search(self) -> SearchQuery {
        SearchQuery {
            query: self.query,
            scope: self.scope,
            scope_match: self.scope_match,
            as_of: None,
            known_as_of: None,
            sys_as_of: None,
            categories: self.categories,
            limit: self.limit,
            embedding: self.embedding,
            graph_enrich: Some(false),
            graph_expand: None,
        }
    }
}

/// Read full records by id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReadRequest {
    /// Ids to read.
    pub ids: Vec<MemoryId>,
    /// Scope used for access check.
    pub scope: String,
    /// Scope match for access check.
    #[serde(default)]
    pub scope_match: ScopeMatch,
    /// Optional valid-time lens for filtering validity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub as_of: Option<Timestamp>,
    /// Known-time lens (Phase 3e).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub known_as_of: Option<Timestamp>,
    /// System-time lens (Phase 3e).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub sys_as_of: Option<Timestamp>,
}

/// Soft-expire or hard-delete a memory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ForgetRequest {
    /// Target id.
    pub id: MemoryId,
    /// Scope for access check.
    pub scope: String,
    /// Expected version for optimistic concurrency (optional for soft).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_version: Option<u64>,
    /// Hard delete when true (default soft expire).
    #[serde(default)]
    pub hard: bool,
    /// Soft-expire time (default now).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub at: Option<Timestamp>,
}

/// Result of a put or supersede write.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WriteResult {
    #[doc = "True when returning the original acknowledgement for a keyed replay."]
    #[serde(default)]
    pub replayed: bool,
    /// New record id.
    pub id: MemoryId,
    /// New version.
    pub version: u64,
    /// Full record (optional for thin clients).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<MemoryRecord>,
}
