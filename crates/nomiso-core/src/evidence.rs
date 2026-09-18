//! Evidence plane types (Phase 3b/3c).

#![allow(missing_docs)]

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::types::{MemoryId, Timestamp};

/// Span unit for citations into artifacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SpanUnit {
    Byte,
    Line,
    Char,
}

impl SpanUnit {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Byte => "byte",
            Self::Line => "line",
            Self::Char => "char",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "byte" => Some(Self::Byte),
            "line" => Some(Self::Line),
            "char" => Some(Self::Char),
            _ => None,
        }
    }
}

/// Register an artifact (bytes already stored externally; plane holds meta).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PutArtifactRequest {
    pub scope: String,
    /// Lowercase hex BLAKE3 (64 chars).
    pub blake3: String,
    /// URI/path where bytes live (s3://, file://, …).
    pub location: String,
    pub media_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust: Option<f64>,
}

/// Stored artifact metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ArtifactRecord {
    pub id: String,
    pub scope: String,
    pub blake3: String,
    pub location: String,
    pub media_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust: Option<f64>,
    #[schemars(with = "String")]
    pub created_at: Timestamp,
}

/// Create a span into an artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PutSpanRequest {
    pub scope: String,
    pub artifact_id: String,
    pub start: u64,
    pub end: u64,
    pub unit: SpanUnit,
}

/// Stored span.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SpanRecord {
    pub id: String,
    pub artifact_id: String,
    pub scope: String,
    pub start: u64,
    pub end: u64,
    pub unit: SpanUnit,
    #[schemars(with = "String")]
    pub created_at: Timestamp,
}

/// Link a memory to evidence or another memory (derived_from).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "target_kind", rename_all = "snake_case")]
pub enum DerivedFromTarget {
    Artifact { id: String },
    Span { id: String },
    Memory { id: MemoryId },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LinkDerivedFromRequest {
    pub memory_id: MemoryId,
    pub scope: String,
    pub target: DerivedFromTarget,
}

/// Annotate metadata without forging valid time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AnnotateRequest {
    pub id: MemoryId,
    pub scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_version: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attrs: Option<serde_json::Value>,
    /// When false, update metadata without bumping optimistic version
    /// (product telemetry: scan/use counts). Default true.
    #[serde(default = "default_true")]
    pub bump_version: bool,
}

fn default_true() -> bool {
    true
}

/// History chain entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HistoryEntry {
    pub record: crate::types::MemoryRecord,
}
