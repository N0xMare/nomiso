//! Working-state / task_state slot (Phase 3.5) — mutable scoped document.

#![allow(missing_docs)]

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::Timestamp;

/// Put or replace a task_state slot (optimistic version when expected_version set).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PutTaskStateRequest {
    pub scope: String,
    /// Slot name within scope (e.g. "coding-wm", "default").
    pub slot: String,
    /// Arbitrary JSON body (product-defined shape).
    pub body: Value,
    /// When set, must match current version or Conflict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_version: Option<u64>,
    /// When true, create the slot only; a second write fails with Conflict.
    /// Mutually exclusive with `expected_version`.
    #[serde(default)]
    pub create_only: bool,
}

/// Stored task_state row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TaskStateRecord {
    pub id: String,
    pub scope: String,
    pub slot: String,
    pub body: Value,
    pub version: u64,
    #[schemars(with = "String")]
    pub sys_created: Timestamp,
    #[schemars(with = "String")]
    pub sys_updated: Timestamp,
}

/// Read a slot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GetTaskStateRequest {
    pub scope: String,
    pub slot: String,
}
