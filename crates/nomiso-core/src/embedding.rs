//! Embedding model/index identity and staged reindex generations (MIG-004/005).
//!
//! A store's vector index is identified by more than its dimension: the
//! embedding family, model revision, normalization, and encoding together
//! determine whether two vectors are comparable. Every write stamps the active
//! generation; re-embedding builds a new generation off to the side and only
//! activates after explicit validation.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::types::{MemoryId, Timestamp};

/// Vector normalization applied by the embedder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingNormalization {
    /// L2-normalized unit vectors (cosine geometry).
    L2,
    /// Explicitly unnormalized.
    None,
    /// Provider does not declare normalization.
    #[default]
    Unknown,
}

/// Embedding identity for one vector generation (MIG-004).
///
/// An endpoint URL is not a stable model revision; when a provider cannot
/// identify immutable weights, `limitation` records that gap honestly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EmbeddingIdentity {
    /// Provider/family, e.g. `nomiso-hashing`, `openai-compatible`.
    pub family: String,
    /// Model revision, e.g. `fnv1a-bow-v1`, `text-embedding-3-small`.
    pub model: String,
    /// Vector dimension.
    pub dimension: u32,
    /// Normalization applied.
    #[serde(default)]
    pub normalization: EmbeddingNormalization,
    /// Element encoding, e.g. `f32`.
    pub encoding: String,
    /// Recorded limitation (non-immutable weights, unstated normalization, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limitation: Option<String>,
}

impl EmbeddingIdentity {
    /// Identity for stores whose vectors predate identity tracking, or where
    /// the operator made no declaration. Missing identity stays explicitly
    /// unknown (MIG-003); it never silently becomes a named model.
    pub fn unknown(dimension: u32) -> Self {
        Self {
            family: "unknown".into(),
            model: "unknown".into(),
            dimension,
            normalization: EmbeddingNormalization::Unknown,
            encoding: "f32".into(),
            limitation: Some(
                "embedding identity not recorded; vectors predate identity tracking or were never declared"
                    .into(),
            ),
        }
    }

    /// True when this identity was never declared/attested.
    pub fn is_unknown(&self) -> bool {
        self.family == "unknown" && self.model == "unknown"
    }

    /// Two identities produce comparable vectors only when every identifying
    /// field matches — same dimension is necessary but not sufficient.
    pub fn compatible_with(&self, other: &Self) -> bool {
        self.family == other.family
            && self.model == other.model
            && self.dimension == other.dimension
            && self.normalization == other.normalization
            && self.encoding == other.encoding
    }
}

/// Lifecycle of one embedding generation (MIG-005).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GenerationStatus {
    /// Staged reindex in progress; not queried.
    Building,
    /// Live generation serving queries and stamped on new writes.
    Active,
    /// Superseded generation retained for rollback until cleanup.
    Retired,
    /// Build failed; never activated.
    Failed,
}

/// Point-in-time frontier a staged generation was built from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SourceFrontier {
    /// When the frontier was captured.
    #[schemars(with = "String")]
    pub captured_at: Timestamp,
    /// Generation this one was declared against (None for the first).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
    /// Embedded memory rows captured in the frontier.
    pub expected_count: u64,
}

/// One recorded embedding generation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EmbeddingGeneration {
    /// Monotonic generation number (1 = initial).
    pub generation: u64,
    /// Model identity.
    pub identity: EmbeddingIdentity,
    /// Lifecycle status.
    pub status: GenerationStatus,
    /// Frontier declared at build start (None for generation 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_frontier: Option<SourceFrontier>,
    /// Staged vector count for this generation.
    #[serde(default)]
    pub embedded_count: u64,
    /// Free-form operator note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Declared at.
    #[schemars(with = "String")]
    pub created_at: Timestamp,
    /// Activated at (None until activation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub activated_at: Option<Timestamp>,
}

/// Full embedding index state: active generation plus all recorded ones.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EmbeddingState {
    /// Currently active generation (None only on a pre-identity store).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<EmbeddingGeneration>,
    /// All generations, ordered by generation number.
    #[serde(default)]
    pub generations: Vec<EmbeddingGeneration>,
}

/// Declare a new staged generation to re-embed into (MIG-005).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeclareGenerationRequest {
    /// Identity the new generation's vectors will carry.
    pub identity: EmbeddingIdentity,
    /// Operator note (reason for reindex, expected delta, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One staged vector for a building generation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StagedEmbedding {
    /// Target memory.
    pub memory: MemoryId,
    /// Re-embedded vector (must match the generation's declared dimension).
    pub vector: Vec<f32>,
}
