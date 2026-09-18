//! Core domain types for memory records and search hits.

use jiff::Timestamp as JiffTimestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Timestamp type used across Nomiso (jiff).
pub type Timestamp = JiffTimestamp;

/// Stable memory record identifier (uuid v7 preferred).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct MemoryId(pub String);

impl MemoryId {
    /// Create from a uuid.
    pub fn from_uuid(id: Uuid) -> Self {
        Self(id.to_string())
    }

    /// Create from a full record key like `memory:…` or bare uuid.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// String form.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Bare key without table prefix (for Surreal record id component).
    pub fn bare_key(&self) -> &str {
        let s = self.0.as_str();
        let s = s
            .strip_prefix("memory:")
            .or_else(|| s.strip_prefix("⟨memory:"))
            .unwrap_or(s);
        s.trim_matches(|c| c == '`' || c == '⟨' || c == '⟩' || c == '"')
    }
}

impl std::fmt::Display for MemoryId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for MemoryId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for MemoryId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

/// Memory category (MVP set from the architecture spec).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// Conversation turns, tool traces, raw evidence.
    Episodic,
    /// Facts, preferences, durable conclusions.
    #[default]
    Semantic,
    /// Stable user/agent profile attributes.
    Identity,
    /// Skills / playbooks.
    Procedural,
    /// Explicit conflicts.
    Uncertainty,
    /// Audit of retrieval/writes.
    Trace,
}

impl Category {
    /// Wire / SurrealQL string form.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Episodic => "episodic",
            Self::Semantic => "semantic",
            Self::Identity => "identity",
            Self::Procedural => "procedural",
            Self::Uncertainty => "uncertainty",
            Self::Trace => "trace",
        }
    }

    /// Parse from string (case-insensitive).
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "episodic" => Some(Self::Episodic),
            "semantic" => Some(Self::Semantic),
            "identity" => Some(Self::Identity),
            "procedural" => Some(Self::Procedural),
            "uncertainty" => Some(Self::Uncertainty),
            "trace" => Some(Self::Trace),
            _ => None,
        }
    }
}

impl std::fmt::Display for Category {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Human-readable content plus optional structured attributes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct Content {
    /// Searchable text body (required for searchable categories).
    pub text: String,
    /// Optional free-form attributes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attrs: Option<serde_json::Value>,
}

impl Content {
    /// Construct text-only content.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            attrs: None,
        }
    }

    /// Approximate serialized size of attrs for limit checks.
    pub fn attrs_bytes(&self) -> usize {
        self.attrs
            .as_ref()
            .map(|v| v.to_string().len())
            .unwrap_or(0)
    }
}

/// Provenance hooks for a memory write.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default, JsonSchema)]
pub struct Provenance {
    /// Source identifier (session, file, tool, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Kind of evidence (user_statement, tool_output, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Optional span / correlation id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<String>,
}

/// Full durable memory record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryRecord {
    /// Stable id.
    pub id: MemoryId,
    /// Category.
    pub category: Category,
    /// Scope path.
    pub scope: String,
    /// Content body.
    pub content: Content,
    /// Optional embedding vector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<Vec<f32>>,
    /// Embedding generation that produced `embedding` (MIG-004). None on rows
    /// predating identity tracking or written without a vector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_generation: Option<u64>,
    /// Valid-time start (when true in the world).
    #[schemars(with = "String")]
    pub valid_from: Timestamp,
    /// Valid-time end; `None` means open-ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub valid_until: Option<Timestamp>,
    /// When the system learned this fact.
    #[schemars(with = "String")]
    pub known_at: Timestamp,
    /// Optional confidence in \[0, 1\].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    /// Provenance.
    #[serde(default)]
    pub provenance: Provenance,
    /// Optimistic concurrency token.
    pub version: u64,
    /// Id this record supersedes, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<MemoryId>,
    /// Id that superseded this record, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<MemoryId>,
    /// Linked entity ids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entity_links: Vec<String>,
    /// Marked stale when a derivation source changed (plane mechanism; product decides response).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale: Option<bool>,
    /// Optional extractor software version stamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extractor_version: Option<String>,
    /// Optional model version stamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_version: Option<String>,
    /// Optional idempotency key (scope-unique when set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// Optional repo/commit validity start (coding axis).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_rev_from: Option<String>,
    /// Optional repo/commit validity end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_rev_until: Option<String>,
    /// Server time when this version row was created (Phase 3e).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub sys_created: Option<Timestamp>,
    /// Server time of last allowed metadata update.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub sys_updated: Option<Timestamp>,
    /// Server time when this version's valid interval was closed (once).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub sys_closed: Option<Timestamp>,
}

/// Channel participation hints on a search hit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, JsonSchema)]
pub struct SearchSignals {
    /// Appeared in vector candidate list.
    #[serde(default)]
    pub vector: bool,
    /// Appeared in BM25 candidate list.
    #[serde(default)]
    pub bm25: bool,
    /// Graph enrichment attached.
    #[serde(default)]
    pub graph: bool,
    /// Surfaced via graph candidate expansion (not a direct retrieval hit).
    /// Merged direct hits keep `expanded = false` and carry `expansion`
    /// provenance instead.
    #[serde(default)]
    pub expanded: bool,
}

/// How [`SearchHit::score`] was produced (additive honesty field).
///
/// Distinguishes real engine scores from rank-position fallback so
/// abstention / fusion never confuses synthetic ranks with BM25/RRF/distance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScoreKind {
    /// Score from the retrieval engine (BM25, RRF, or distance-derived).
    Engine,
    /// Rank-position fallback when no engine score was available.
    RankFallback,
    /// Score provenance not determined.
    #[default]
    Unknown,
}

/// Discovery path for a hit reached (or corroborated) by graph candidate
/// expansion (spec 05: retain discovery paths without converting association
/// into endorsement).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExpansionInfo {
    /// Direct-hit seeds whose edges reached this candidate (bounded, ≤3).
    pub from: Vec<MemoryId>,
    /// Shortest hop distance from a seed (1 = direct neighbor).
    pub depth: u32,
    /// Predicates traversed to reach this candidate (bounded, ≤3).
    pub via_predicates: Vec<String>,
}

/// Ranked search card returned to clients.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SearchHit {
    /// Memory id.
    pub id: MemoryId,
    /// Fused score (RRF scale when hybrid).
    pub score: f64,
    /// Whether `score` is engine-derived or a labeled fallback.
    #[serde(default)]
    pub score_kind: ScoreKind,
    /// Channel hints.
    pub signals: SearchSignals,
    /// Text preview.
    pub preview: String,
    /// Category.
    pub category: Category,
    /// Scope.
    pub scope: String,
    /// Valid-from.
    #[schemars(with = "String")]
    pub valid_from: Timestamp,
    /// Valid-until.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub valid_until: Option<Timestamp>,
    /// Provenance.
    #[serde(default)]
    pub provenance: Provenance,
    /// Version.
    pub version: u64,
    /// Optional entity metadata from graph enrich.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<String>,
    /// Embedding generation this hit's vector belongs to (MIG-005: queries
    /// identify the generation they used). None when the row carries no
    /// generation stamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_generation: Option<u64>,
    /// Graph-expansion discovery path; present when expansion reached this
    /// candidate — whether or not it was also a direct hit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expansion: Option<ExpansionInfo>,
}
