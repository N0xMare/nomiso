//! Toolkit-level policy for the reusable mechanics.
//!
//! Product profiles (e.g. Vegapunk `ProfilePolicy`) resolve to this struct;
//! the mechanics here never see product enums, so the toolkit stays usable
//! without adopting any product's policy vocabulary.

use serde::{Deserialize, Serialize};

/// Policy knobs the memory mechanics consult. Copy-friendly; construct
/// directly or convert from a product profile via `Into<MemoryPolicy>`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryPolicy {
    /// Default search limit when a recall request does not specify one.
    pub default_recall_limit: u32,
    /// Minimum non-whitespace summary length for a checkpoint write.
    pub min_checkpoint_chars: usize,
    /// Prefer semantic category for preference/fact language.
    pub prefer_semantic_for_facts: bool,
    /// Attach typed-relationship links to recall hits (`signals.graph`).
    pub graph_enrich: bool,
    /// Allow hard-delete forget via the writer/model path (default **false** —
    /// `WriterOp::Forget { hard: true }` is soft-downgraded). Structured
    /// importers that bypass the writer path are not gated by this.
    pub allow_hard_forget: bool,
    /// Require `expected_version` on forget ops (default **true**).
    pub require_version_on_forget: bool,
}

impl Default for MemoryPolicy {
    /// Conservative toolkit baseline: explicit pack semantics, semantic
    /// preference cues, no hard-forget from writer ops, versioned forgets.
    fn default() -> Self {
        Self {
            default_recall_limit: 8,
            min_checkpoint_chars: 12,
            prefer_semantic_for_facts: true,
            graph_enrich: false,
            allow_hard_forget: false,
            require_version_on_forget: true,
        }
    }
}
