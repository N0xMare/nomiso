//! Tact-facing types (API surface).

use nomiso::{MemoryId, Timestamp};
use serde::{Deserialize, Serialize};

/// Who is calling the tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Actor {
    /// Coordinating root agent (may mutate).
    Root,
    /// Child / subagent (scan/read only).
    Child,
}

/// Stable key + version (plane id + optimistic version).
///
/// **Nomiso improvement:** replace/supersede creates a new plane id; the product
/// logical key is [`MemoryRecord::logical_id`] (normalized identity), stable
/// across supersession for host identity tracking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryKey {
    pub id: MemoryId,
    pub version: u64,
}

/// Full record after read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub key: MemoryKey,
    /// Stable logical identity (normalized content); survives supersede.
    pub logical_id: String,
    pub content: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub scan_count: u64,
    pub use_count: u64,
    pub probation_until: Option<Timestamp>,
}

/// Compact scan card.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryCandidate {
    pub key: MemoryKey,
    pub logical_id: String,
    pub preview: String,
    pub score: f64,
}

/// Scan result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryScan {
    pub abstained: bool,
    pub candidates: Vec<MemoryCandidate>,
}

/// Put result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PutResult {
    pub record: MemoryRecord,
    pub replaced: bool,
}

/// Optional replace target for put.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplaceTarget {
    pub id: MemoryId,
    pub expected_version: u64,
}
