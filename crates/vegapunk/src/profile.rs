//! Memory profiles — opinionated policy knobs.

use serde::{Deserialize, Serialize};

/// Named Vegapunk profiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Profile {
    /// Coding-agent defaults: explicit recall, precision-first writes, checkpoint durable units.
    #[default]
    CodingAgent,
    /// Minimal: only store what is forced; tight recall limit.
    Minimal,
}

/// Resolved policy for a profile (no LLM).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProfilePolicy {
    /// Default search limit.
    pub default_recall_limit: u32,
    /// Soft-inject large semantic recall into prompts (default **false** — explicit pack).
    /// Distinct from **working-state restore**, which is ON for coding profiles.
    pub allow_soft_inject: bool,
    /// Restore coding working-state slot at session start (product policy; not soft-inject).
    pub restore_working_state: bool,
    /// Max soft-inject cards if a host opts in.
    pub soft_inject_max_cards: u32,
    /// Prefer semantic category for preference/fact language.
    pub prefer_semantic_for_facts: bool,
    /// Minimum non-whitespace summary length for checkpoint.
    pub min_checkpoint_chars: usize,
    /// Graph enrich on recall.
    pub graph_enrich: bool,
    /// Allow hard-delete forget via model/writer path (default **false**).
    ///
    /// When false, `WriterOp::Forget { hard: true }` is soft-downgraded.
    /// Structured `remember`/importers do not use this gate; they stay trusted direct writes.
    pub allow_hard_forget: bool,
    /// Require `expected_version` on forget ops (default **true**).
    pub require_version_on_forget: bool,
}

impl Profile {
    /// Policy for this profile.
    pub fn policy(self) -> ProfilePolicy {
        match self {
            Self::CodingAgent => ProfilePolicy {
                default_recall_limit: 8,
                allow_soft_inject: false,    // explicit pack only
                restore_working_state: true, // WM restore ON
                soft_inject_max_cards: 4,
                prefer_semantic_for_facts: true,
                min_checkpoint_chars: 12,
                graph_enrich: false,
                allow_hard_forget: false,
                require_version_on_forget: true,
            },
            Self::Minimal => ProfilePolicy {
                default_recall_limit: 4,
                allow_soft_inject: false,
                restore_working_state: false,
                soft_inject_max_cards: 0,
                prefer_semantic_for_facts: true,
                min_checkpoint_chars: 24,
                graph_enrich: false,
                allow_hard_forget: false,
                require_version_on_forget: true,
            },
        }
    }

    /// Parse profile name.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "coding-agent" | "coding" | "coder" => Some(Self::CodingAgent),
            "minimal" | "min" => Some(Self::Minimal),
            _ => None,
        }
    }

    /// Wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CodingAgent => "coding-agent",
            Self::Minimal => "minimal",
        }
    }
}

/// Product policy resolves to the toolkit policy the mechanics consume.
impl From<ProfilePolicy> for nomiso_memory::MemoryPolicy {
    fn from(p: ProfilePolicy) -> Self {
        Self {
            default_recall_limit: p.default_recall_limit,
            min_checkpoint_chars: p.min_checkpoint_chars,
            prefer_semantic_for_facts: p.prefer_semantic_for_facts,
            graph_enrich: p.graph_enrich,
            allow_hard_forget: p.allow_hard_forget,
            require_version_on_forget: p.require_version_on_forget,
        }
    }
}
