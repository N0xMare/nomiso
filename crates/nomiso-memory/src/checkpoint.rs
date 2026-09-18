//! Checkpoint policy — decide whether to store after a durable unit of work.

use nomiso_core::Category;
use nomiso_service::NomisoClient;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::policy::MemoryPolicy;
use crate::remember::{remember, RememberInput};
use crate::types::{CheckpointInput, RememberOutcome};

/// Outcome of a checkpoint call.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CheckpointOutcome {
    /// Nothing stored (policy).
    Skipped {
        /// Why.
        reason: String,
    },
    /// Stored a memory.
    Stored(RememberOutcome),
}

pub async fn checkpoint(
    client: &NomisoClient,
    policy: impl Into<MemoryPolicy>,
    input: CheckpointInput,
) -> Result<CheckpointOutcome> {
    let policy = policy.into();
    let summary = input.summary.trim();
    if summary.is_empty() {
        return Ok(CheckpointOutcome::Skipped {
            reason: "empty summary".into(),
        });
    }
    if !input.force && summary.chars().count() < policy.min_checkpoint_chars {
        return Ok(CheckpointOutcome::Skipped {
            reason: format!(
                "summary shorter than min_checkpoint_chars ({})",
                policy.min_checkpoint_chars
            ),
        });
    }

    // Trivial / non-durable chatter (rule-based; not an LLM)
    if !input.force && is_ephemeral_chatter(summary) {
        return Ok(CheckpointOutcome::Skipped {
            reason: "looks like ephemeral chatter; use force=true to store".into(),
        });
    }

    let outcome = remember(
        client,
        policy,
        RememberInput {
            scope: input.scope,
            text: summary.to_string(),
            category: Some(Category::Semantic),
            confidence: Some(0.85),
            source: Some("nomiso-memory:checkpoint".into()),
            embedding: None,
            episodic: false,
            idempotency_key: None,
        },
    )
    .await?;

    Ok(CheckpointOutcome::Stored(outcome))
}

fn is_ephemeral_chatter(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    const EPHEMERAL: &[&str] = &[
        "ok",
        "okay",
        "thanks",
        "thank you",
        "lol",
        "lgtm",
        "wip",
        "testing",
        "hello",
        "hi ",
    ];
    let trimmed = t.trim();
    EPHEMERAL.contains(&trimmed) || (trimmed.split_whitespace().count() <= 2 && trimmed.len() < 16)
}
