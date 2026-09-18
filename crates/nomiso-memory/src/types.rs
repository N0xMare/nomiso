//! Public input/output types for the memory toolkit.

use nomiso_core::{Category, MemoryId};
use serde::{Deserialize, Serialize};

/// Result of an apply-ops call: per-op outcomes plus optional write-trace id.
#[derive(Debug, Clone, Deserialize)]
pub struct ApplyOpsReport {
    pub outcomes: Vec<crate::writer::ApplyOpOutcome>,
    #[serde(default)]
    pub trace_id: Option<String>,
}

impl ApplyOpsReport {
    /// True when every op outcome is `Ok`.
    pub fn is_ok(&self) -> bool {
        self.outcomes
            .iter()
            .all(crate::writer::ApplyOpOutcome::is_ok)
    }

    /// `ok` when every op succeeded; `partial` when at least one op stored or
    /// forgot and at least one op errored; `failed` otherwise.
    pub fn status(&self) -> &'static str {
        use crate::writer::{ApplyOpOutcome, ApplyResult};
        if self.is_ok() {
            "ok"
        } else if self
            .outcomes
            .iter()
            .filter_map(ApplyOpOutcome::ok_result)
            .any(|r| matches!(r, ApplyResult::Stored(_) | ApplyResult::Forgotten { .. }))
        {
            "partial"
        } else {
            "failed"
        }
    }
}

impl Serialize for ApplyOpsReport {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st =
            s.serialize_struct("ApplyOpsReport", 2 + usize::from(self.trace_id.is_some()))?;
        st.serialize_field("status", self.status())?;
        st.serialize_field("outcomes", &self.outcomes)?;
        if let Some(t) = &self.trace_id {
            st.serialize_field("trace_id", t)?;
        }
        st.end()
    }
}

/// Result of a successful remember/store path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RememberOutcome {
    #[serde(default)]
    pub replayed: bool,
    /// Plane memory id.
    pub id: MemoryId,
    /// Version after write.
    pub version: u64,
    /// Category actually stored.
    pub category: Category,
    /// Scope stored under.
    pub scope: String,
    /// Whether policy skipped (should be false for Stored paths).
    pub skipped: bool,
    /// Text preview.
    pub preview: String,
}

/// Checkpoint a durable unit of work.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointInput {
    /// Scope path.
    pub scope: String,
    /// Human summary of what was completed / decided.
    pub summary: String,
    /// Force store even if under min length (still rejects empty).
    #[serde(default)]
    pub force: bool,
}

/// Ingest host-extracted durable lines from a compacted turn.
///
/// Default [`crate::writer::RuleWriter`] puts each non-empty line. Pass
/// extracted facts, not raw chat. Checkpoint is attempted only when every
/// apply outcome is `Ok`; a checkpoint `Err` does not discard apply (see
/// [`CompactionReport::checkpoint_error`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionIngest {
    pub scope: String,
    /// Host-extracted durable lines (not a raw transcript dump).
    pub transcript: String,
    /// Optional durable-unit summary (checkpoint **after** a fully-ok apply).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// Result of a compaction-ingest call (e.g. `Vegapunk::ingest_compaction`).
///
/// After prefix-preserving apply, outcomes are always returned. Checkpoint
/// runs only when every apply outcome is `Ok`. If that checkpoint call fails,
/// `checkpoint` is `None` and `checkpoint_error` carries the message — apply
/// is not hidden behind `?`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionReport {
    pub apply: ApplyOpsReport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<crate::checkpoint::CheckpointOutcome>,
    /// Present when apply succeeded but the optional checkpoint call returned `Err`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_error: Option<String>,
}

impl CompactionReport {
    /// True when the apply report is fully ok and no checkpoint error was
    /// recorded.
    pub fn is_ok(&self) -> bool {
        self.apply.is_ok() && self.checkpoint_error.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::writer::{ApplyOpOutcome, ApplyResult};

    fn ok_outcome(index: usize, result: ApplyResult) -> ApplyOpOutcome {
        ApplyOpOutcome::Ok { index, result }
    }

    fn err_outcome(index: usize, code: &str) -> ApplyOpOutcome {
        ApplyOpOutcome::Err {
            index,
            error: "boom".into(),
            code: code.into(),
            rolled_back: vec![],
            rollback_failed: vec![],
        }
    }

    #[test]
    fn apply_ops_report_status_and_serialization() {
        let stored = ApplyResult::Stored(RememberOutcome {
            replayed: false,
            id: MemoryId::new("m1"),
            version: 1,
            category: Category::Semantic,
            scope: "s".into(),
            skipped: false,
            preview: "p".into(),
        });

        let noop_only = ApplyOpsReport {
            outcomes: vec![ok_outcome(
                0,
                ApplyResult::Noop {
                    reason: "nothing".into(),
                },
            )],
            trace_id: None,
        };
        assert!(noop_only.is_ok());
        assert_eq!(noop_only.status(), "ok");
        let v = serde_json::to_value(&noop_only).unwrap();
        assert_eq!(v["status"], "ok");
        assert!(v.get("trace_id").is_none());
        let back: ApplyOpsReport = serde_json::from_value(v).unwrap();
        assert!(back.is_ok());

        let partial = ApplyOpsReport {
            outcomes: vec![
                ok_outcome(0, stored),
                err_outcome(1, "not_found"),
                err_outcome(2, "not_attempted"),
            ],
            trace_id: Some("tid".into()),
        };
        assert!(!partial.is_ok());
        assert_eq!(partial.status(), "partial");
        let v = serde_json::to_value(&partial).unwrap();
        assert_eq!(v["status"], "partial");
        assert_eq!(v["trace_id"], "tid");
        assert_eq!(v["outcomes"].as_array().unwrap().len(), 3);
        assert_eq!(v["outcomes"][2]["code"], "not_attempted");

        let failed = ApplyOpsReport {
            outcomes: vec![
                ok_outcome(
                    0,
                    ApplyResult::Noop {
                        reason: "skip".into(),
                    },
                ),
                err_outcome(1, "conflict"),
            ],
            trace_id: None,
        };
        assert_eq!(failed.status(), "failed");

        let all_failed = ApplyOpsReport {
            outcomes: vec![err_outcome(0, "invalid_request")],
            trace_id: None,
        };
        assert_eq!(all_failed.status(), "failed");
    }

    #[test]
    fn compaction_report_surfaces_checkpoint_error() {
        let report = CompactionReport {
            apply: ApplyOpsReport {
                outcomes: vec![],
                trace_id: Some("tid".into()),
            },
            checkpoint: None,
            checkpoint_error: Some("checkpoint failed: store down".into()),
        };
        let v = serde_json::to_value(&report).unwrap();
        assert_eq!(v["checkpoint_error"], "checkpoint failed: store down");
        assert!(v.get("checkpoint").is_none());
        let back: CompactionReport = serde_json::from_value(v).unwrap();
        assert_eq!(
            back.checkpoint_error.as_deref(),
            Some("checkpoint failed: store down")
        );
        assert!(back.checkpoint.is_none());
    }
}
