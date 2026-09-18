//! Trace plane types (Phase 2.5) — separate from Category::Trace on memory.

#![allow(missing_docs)]

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::{MemoryId, ScoreKind, Timestamp};

/// Fixed event kinds for v1 (no open free-string kinds).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TraceEventKind {
    Search,
    Pack,
    Inject,
    Write,
    Outcome,
}

impl TraceEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Pack => "pack",
            Self::Inject => "inject",
            Self::Write => "write",
            Self::Outcome => "outcome",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "search" => Some(Self::Search),
            "pack" => Some(Self::Pack),
            "inject" => Some(Self::Inject),
            "write" => Some(Self::Write),
            "outcome" => Some(Self::Outcome),
            _ => None,
        }
    }
}

/// Host-reported outcome (plane does not infer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TraceOutcome {
    Helped,
    Harmed,
    Unknown,
    Skipped,
}

impl TraceOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Helped => "helped",
            Self::Harmed => "harmed",
            Self::Unknown => "unknown",
            Self::Skipped => "skipped",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "helped" => Some(Self::Helped),
            "harmed" => Some(Self::Harmed),
            "unknown" => Some(Self::Unknown),
            "skipped" => Some(Self::Skipped),
            _ => None,
        }
    }
}

/// Compact hit card for search/pack events (ids + ranks + score_kind + small preview).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TraceHitCard {
    pub id: MemoryId,
    pub rank: u32,
    pub score: f64,
    #[serde(default)]
    pub score_kind: ScoreKind,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub preview: String,
}

/// Append a trace event (lazy-creates parent `trace` row when needed).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AppendTraceEvent {
    /// Client-supplied or server-generated correlation id.
    pub trace_id: String,
    /// Exact scope (required).
    pub scope: String,
    pub kind: TraceEventKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    /// Kind-specific payload (hits, write ids, outcome, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
    /// Memory ids referenced by this event (inject / search / write).
    ///
    /// When empty at append time the store extracts ids from `payload`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub memory_ids: Vec<MemoryId>,
}

/// Stored event row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TraceEventRecord {
    pub id: String,
    pub trace_id: String,
    pub scope: String,
    pub kind: TraceEventKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
    #[schemars(with = "String")]
    pub created_at: Timestamp,
}

/// Inventory a scope's traces (parent rows only).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ListTracesRequest {
    pub scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub since: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub until: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Exact session filter (scope remains the tenant filter).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Exact turn filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
}

/// Which traces referenced a memory, plus the latest host outcome.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TracesByMemoryRequest {
    pub scope: String,
    pub memory_id: MemoryId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub since: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub until: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
}

/// One trace that referenced a memory, with latest host outcome if any.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TraceByMemory {
    pub trace_id: String,
    pub scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[schemars(with = "String")]
    pub created_at: Timestamp,
    pub matched_kinds: Vec<TraceEventKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<TraceOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome_note: Option<String>,
}

/// One parent-trace row from [`ListTracesRequest`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TraceSummary {
    pub trace_id: String,
    pub scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[schemars(with = "String")]
    pub created_at: Timestamp,
}

/// Parent trace handle + ordered events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TraceBundle {
    pub trace_id: String,
    pub scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    pub events: Vec<TraceEventRecord>,
}

/// Max preview chars on trace hit cards.
pub const TRACE_PREVIEW_MAX: usize = 160;

/// Build capped preview for traces.
pub fn cap_preview(s: &str) -> String {
    let t = s.trim();
    if t.chars().count() <= TRACE_PREVIEW_MAX {
        t.to_string()
    } else {
        t.chars().take(TRACE_PREVIEW_MAX).collect::<String>() + "…"
    }
}

/// Pull memory ids from inject / search / write payload shapes.
pub fn memory_ids_from_payload(payload: &Value) -> Vec<MemoryId> {
    let mut out = Vec::new();
    let mut push = |raw: &str| {
        let t = raw.trim();
        if !t.is_empty() {
            out.push(MemoryId::new(t));
        }
    };
    if let Some(arr) = payload.get("memory_ids").and_then(|v| v.as_array()) {
        for v in arr {
            if let Some(s) = v.as_str() {
                push(s);
            }
        }
    }
    if let Some(arr) = payload.get("hits").and_then(|v| v.as_array()) {
        for hit in arr {
            if let Some(s) = hit.get("id").and_then(|v| v.as_str()) {
                push(s);
            }
        }
    }
    if let Some(s) = payload.get("id").and_then(|v| v.as_str()) {
        push(s);
    }
    if let Some(arr) = payload.get("ops").and_then(|v| v.as_array()) {
        for op in arr {
            if let Some(s) = op.get("id").and_then(|v| v.as_str()) {
                push(s);
            }
            if let Some(s) = op.get("forgotten").and_then(|v| v.as_str()) {
                push(s);
            }
        }
    }
    out
}

/// Bare uuid and `memory:` forms for dual-id store/query.
pub fn memory_id_index_forms(id: &MemoryId) -> Vec<String> {
    let bare = id.bare_key();
    if bare.is_empty() {
        return Vec::new();
    }
    let prefixed = format!("memory:{bare}");
    if bare == prefixed {
        vec![bare.to_string()]
    } else {
        vec![bare.to_string(), prefixed]
    }
}
