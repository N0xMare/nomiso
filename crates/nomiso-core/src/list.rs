//! List / count ops (Phase 3a).

#![allow(missing_docs)]

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::scope::ScopeMatch;
use crate::types::{Category, MemoryId, MemoryRecord, ScoreKind, Timestamp};

/// Query context pinned by a list cursor (filters + effective valid time).
///
/// Cursors are only valid against an identical normalized query; a cursor
/// issued for different filters/lenses/scope must be rejected.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ListCursorQuery {
    /// Normalized scope the cursor was issued under.
    pub scope: String,
    #[serde(default)]
    pub scope_match: ScopeMatch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub categories: Option<Vec<Category>>,
    /// Effective valid-time lens (resolved on the first page).
    #[schemars(with = "String")]
    pub as_of: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub known_as_of: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub sys_as_of: Option<Timestamp>,
    /// Normalized text filter (trimmed, empty → None).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// Keyset cursor for list pagination (valid_from + id).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ListCursor {
    #[schemars(with = "String")]
    pub valid_from: Timestamp,
    pub id: MemoryId,
    /// Query context this cursor was issued under (required on use).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<ListCursorQuery>,
}

/// List memories under scope (enumerate + optional lexical text filter).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ListRequest {
    pub scope: String,
    #[serde(default)]
    pub scope_match: ScopeMatch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub categories: Option<Vec<Category>>,
    /// Valid-time lens; None = now.
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
    /// Optional BM25/text filter (not hybrid ranking product surface).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<ListCursor>,
}

/// One list row (record + optional lexical score when text filter used).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ListItem {
    pub record: MemoryRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score_kind: Option<ScoreKind>,
}

/// List page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ListResponse {
    pub items: Vec<ListItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<ListCursor>,
}

/// Count request (same filters as list, no cursor/limit).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CountRequest {
    pub scope: String,
    #[serde(default)]
    pub scope_match: ScopeMatch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub categories: Option<Vec<Category>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub as_of: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub known_as_of: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub sys_as_of: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}
