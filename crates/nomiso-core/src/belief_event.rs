//! Append-only belief mutation journal (Phase 3e).
//!
//! Distinct from `trace_event` (retrieval/use flywheel). Required journal
//! events commit **inside the same transaction** as the belief mutation —
//! an event describes a committed effect, never an attempted one.

#![allow(missing_docs)]

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::{MemoryId, Timestamp};

/// Kind of belief mutation recorded in the journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BeliefEventKind {
    Assert,
    Close,
    SoftForget,
    HardErase,
    Annotate,
    Correct,
}

impl BeliefEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Assert => "assert",
            Self::Close => "close",
            Self::SoftForget => "soft_forget",
            Self::HardErase => "hard_erase",
            Self::Annotate => "annotate",
            Self::Correct => "correct",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "assert" => Some(Self::Assert),
            "close" => Some(Self::Close),
            "soft_forget" => Some(Self::SoftForget),
            "hard_erase" => Some(Self::HardErase),
            "annotate" => Some(Self::Annotate),
            "correct" => Some(Self::Correct),
            _ => None,
        }
    }
}

/// One append-only belief_event row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BeliefEvent {
    pub id: String,
    pub scope: String,
    pub memory_id: MemoryId,
    pub kind: BeliefEventKind,
    #[schemars(with = "String")]
    pub at_sys: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
}

/// Request to list belief events for a memory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ListBeliefEventsRequest {
    pub scope: String,
    pub memory_id: MemoryId,
}
