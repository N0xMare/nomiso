//! Encode / remember path (structured ops only; no LLM).

use nomiso_core::{Category, Content, Provenance, PutRequest};
use nomiso_service::NomisoClient;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::policy::MemoryPolicy;
use crate::types::RememberOutcome;

/// Input to store a memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RememberInput {
    /// Scope path.
    pub scope: String,
    /// Memory text (required).
    pub text: String,
    /// Optional category override.
    #[serde(default)]
    pub category: Option<Category>,
    /// Optional confidence.
    #[serde(default)]
    pub confidence: Option<f64>,
    /// Provenance source id.
    #[serde(default)]
    pub source: Option<String>,
    /// Optional embedding (else client embedder may fill).
    #[serde(default)]
    pub embedding: Option<Vec<f32>>,
    /// Treat as raw episodic evidence.
    #[serde(default)]
    pub episodic: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

impl RememberInput {
    /// Semantic fact convenience constructor.
    pub fn fact(scope: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            scope: scope.into(),
            text: text.into(),
            category: Some(Category::Semantic),
            confidence: Some(0.9),
            source: None,
            embedding: None,
            episodic: false,
            idempotency_key: None,
        }
    }

    /// Episodic event convenience constructor.
    pub fn episode(scope: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            scope: scope.into(),
            text: text.into(),
            category: Some(Category::Episodic),
            confidence: None,
            source: None,
            embedding: None,
            episodic: true,
            idempotency_key: None,
        }
    }
}

/// Infer category under profile policy (rule-based).
pub fn infer_category(input: &RememberInput, policy: MemoryPolicy) -> Category {
    if let Some(c) = input.category {
        return c;
    }
    if input.episodic {
        return Category::Episodic;
    }
    if policy.prefer_semantic_for_facts && looks_like_fact(&input.text) {
        return Category::Semantic;
    }
    Category::Episodic
}

fn looks_like_fact(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    const CUES: &[&str] = &[
        "prefer",
        "prefers",
        "should ",
        "must ",
        "always ",
        "never ",
        "decision",
        "decided",
        "requirement",
        "constraint",
        "we use ",
        "use typescript",
        "use rust",
    ];
    // Prefer cue-based semantic; do not classify all short text as fact.
    CUES.iter().any(|c| t.contains(c))
}

pub async fn remember(
    client: &NomisoClient,
    policy: impl Into<MemoryPolicy>,
    input: RememberInput,
) -> Result<RememberOutcome> {
    let text = input.text.trim();
    if text.is_empty() {
        return Err(Error::invalid("remember text must be non-empty"));
    }
    let policy = policy.into();
    let req = prepare_put(&input, policy, false)?;
    let category = req.category;
    let scope = req.scope.clone();
    let wr = client.put(req).await?;
    Ok(RememberOutcome {
        replayed: wr.replayed,
        id: wr.id,
        version: wr.version,
        category,
        scope,
        skipped: false,
        preview: text.to_string(),
    })
}

pub fn prepare_put(
    input: &RememberInput,
    policy: impl Into<MemoryPolicy>,
    supersede: bool,
) -> Result<PutRequest> {
    let policy = policy.into();
    let category = infer_category(input, policy);
    let scope = nomiso_core::ScopePath::parse(&input.scope)?
        .as_str()
        .to_owned();
    let source = if supersede {
        "nomiso-memory:supersede"
    } else {
        "nomiso-memory"
    };
    let kind = if supersede {
        "fact_update"
    } else {
        match category {
            Category::Semantic | Category::Identity => "fact",
            Category::Episodic => "episode",
            other => other.as_str(),
        }
    };
    Ok(PutRequest {
        scope,
        category,
        content: Content::text(input.text.trim()),
        confidence: input.confidence,
        provenance: Provenance {
            source: input.source.clone().or_else(|| Some(source.into())),
            kind: Some(kind.into()),
            span: None,
        },
        embedding: input.embedding.clone(),
        embedding_identity: None,
        idempotency_key: input.idempotency_key.clone(),
        ..Default::default()
    })
}

#[cfg(test)]
mod infer_tests {
    use super::*;
    use crate::policy::MemoryPolicy;

    #[test]
    fn prefers_cues_as_semantic() {
        let p = MemoryPolicy::default();
        let input = RememberInput {
            scope: "org/x".into(),
            text: "Alice prefers TypeScript".into(),
            category: None,
            confidence: None,
            source: None,
            embedding: None,
            episodic: false,
            idempotency_key: None,
        };
        assert_eq!(infer_category(&input, p), Category::Semantic);
    }

    #[test]
    fn short_non_cue_is_episodic() {
        let p = MemoryPolicy::default();
        let input = RememberInput {
            scope: "org/x".into(),
            text: "ran the test suite successfully today".into(),
            category: None,
            confidence: None,
            source: None,
            embedding: None,
            episodic: false,
            idempotency_key: None,
        };
        assert_eq!(infer_category(&input, p), Category::Episodic);
    }
}
