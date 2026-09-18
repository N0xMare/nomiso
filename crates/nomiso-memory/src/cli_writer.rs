//! LLM-backed writer and query rewriter (any [`LlmCompletion`], including Mock).
//!
//! Fail-closed: unparseable model output becomes a single `WriterOp::Noop`
//! with reason (never free-form SurrealQL, never invent prior_ids on parse fail).

use std::sync::Arc;

use async_trait::async_trait;

use crate::error::Result;
use crate::llm::{
    parse_json_from_model, parse_writer_ops_from_model, CompletionRequest, LlmCompletion,
};
use crate::reader::QueryRewriter;
use crate::writer::{MemoryWriter, WriteEpisode, WriterOp};

/// System prompt for the memory writer skill (mirrors `examples/skills/VEGAPUNK_WRITER.md`).
pub const WRITER_SYSTEM_PROMPT: &str = r#"You are a memory writer. You do NOT answer the user.
You emit structured JSON ops that the host commits to Nomiso. Prefer precision.

Output: a JSON array of ops only (no prose, no markdown fences if possible).

Ops:
- {"op":"put","scope":"<scope>","text":"<atomic fact>","category":"semantic"|"episodic"|"identity"|"procedural"|"uncertainty","confidence":0.0-1.0,"episodic":false}
- {"op":"supersede","prior_id":"<id>","expected_version":1,"scope":"<scope>","text":"<new fact>","category":"semantic"}
- {"op":"forget","id":"<id>","scope":"<scope>","expected_version":null,"hard":false}
- {"op":"noop","reason":"<why>"}

Rules:
1. Atomic conclusions only — no transcripts, secrets, or plans.
2. Never invent prior_id — only supersede when the episode supplies a real id.
3. Prefer noop over low-confidence invent.
4. Scope must match the episode scope unless the episode clearly states otherwise.
5. One durable fact per put; split multi-fact episodes into multiple puts.
6. If two facts conflict and you have no trusted prior_id, put category \"uncertainty\" stating both sides — do not pick a winner. Prefer noop if you cannot state the conflict atomically.
"#;

/// System prompt for query rewrite (reader plane).
pub const REWRITER_SYSTEM_PROMPT: &str = r#"You rewrite search queries for multi-pass memory recall.
Return a JSON array of 1-4 short alternative query strings only (no prose).
Keep error codes, identifiers, and proper nouns. Prefer keywords over full questions.
"#;

/// MemoryWriter that calls an [`LlmCompletion`] and parses `WriterOp[]`.
#[derive(Clone)]
pub struct CliChatWriter {
    llm: Arc<dyn LlmCompletion>,
    system: String,
    model: Option<String>,
}

impl CliChatWriter {
    pub fn new(llm: Arc<dyn LlmCompletion>) -> Self {
        Self {
            llm,
            system: WRITER_SYSTEM_PROMPT.to_string(),
            model: None,
        }
    }

    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = system.into();
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    fn user_prompt(episode: &WriteEpisode) -> String {
        let mut s = format!(
            "Scope: {}\n\nEpisode:\n{}\n",
            episode.scope.trim(),
            episode.text.trim()
        );
        if let Some(src) = &episode.source {
            s.push_str(&format!("\nSource: {src}\n"));
        }
        s.push_str("\nEmit JSON array of WriterOps now.");
        s
    }
}

#[async_trait]
impl MemoryWriter for CliChatWriter {
    async fn extract(&self, episode: &WriteEpisode) -> Result<Vec<WriterOp>> {
        let text = episode.text.trim();
        if text.is_empty() {
            return Ok(vec![WriterOp::Noop {
                reason: Some("empty episode".into()),
            }]);
        }

        let mut req = CompletionRequest::new(&self.system, Self::user_prompt(episode));
        if let Some(m) = &self.model {
            req = req.with_model(m.clone());
        }

        let resp = self.llm.complete(&req).await?;
        match parse_writer_ops_from_model(&resp.text) {
            Ok(ops) if !ops.is_empty() => {
                // Normalize scopes; empty prior_id supersede → noop (fail closed).
                Ok(normalize_ops(ops, &episode.scope))
            }
            Ok(_) => Ok(vec![WriterOp::Noop {
                reason: Some("model returned empty ops array".into()),
            }]),
            Err(e) => {
                // Fail closed: do not invent facts from unparseable output.
                Ok(vec![WriterOp::Noop {
                    reason: Some(format!("unparseable writer output: {e}")),
                }])
            }
        }
    }
}

fn normalize_ops(ops: Vec<WriterOp>, default_scope: &str) -> Vec<WriterOp> {
    let mut out = Vec::with_capacity(ops.len());
    for mut op in ops {
        match &mut op {
            WriterOp::Put { input } if input.scope.trim().is_empty() => {
                input.scope = default_scope.to_string();
            }
            WriterOp::Supersede { prior_id, new, .. } => {
                if prior_id.as_str().trim().is_empty() {
                    out.push(WriterOp::Noop {
                        reason: Some("empty prior_id on supersede (fail closed)".into()),
                    });
                    continue;
                }
                if new.scope.trim().is_empty() {
                    new.scope = default_scope.to_string();
                }
            }
            WriterOp::Forget { id, scope, .. } => {
                if id.as_str().trim().is_empty() {
                    out.push(WriterOp::Noop {
                        reason: Some("empty id on forget (fail closed)".into()),
                    });
                    continue;
                }
                if scope.trim().is_empty() {
                    *scope = default_scope.to_string();
                }
            }
            _ => {}
        }
        out.push(op);
    }
    out
}

/// QueryRewriter backed by [`LlmCompletion`].
#[derive(Clone)]
pub struct CliQueryRewriter {
    llm: Arc<dyn LlmCompletion>,
    system: String,
    model: Option<String>,
}

impl CliQueryRewriter {
    pub fn new(llm: Arc<dyn LlmCompletion>) -> Self {
        Self {
            llm,
            system: REWRITER_SYSTEM_PROMPT.to_string(),
            model: None,
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }
}

#[async_trait]
impl QueryRewriter for CliQueryRewriter {
    async fn rewrite(&self, query: &str) -> Result<Vec<String>> {
        let q = query.trim();
        if q.is_empty() {
            return Ok(vec![]);
        }
        // Always include original first.
        let mut req = CompletionRequest::new(
            &self.system,
            format!("Original query:\n{q}\n\nEmit JSON array of alternative queries."),
        );
        if let Some(m) = &self.model {
            req = req.with_model(m.clone());
        }
        let resp = self.llm.complete(&req).await?;
        let mut out = vec![q.to_string()];
        match parse_json_from_model::<Vec<String>>(&resp.text) {
            Ok(alts) => {
                for a in alts {
                    let t = a.trim().to_string();
                    if !t.is_empty() && !out.iter().any(|x| x == &t) {
                        out.push(t);
                    }
                }
            }
            Err(_) => {
                // Fail soft for reader: keep original only (rule rewriter host can still attach).
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::MockLlm;

    #[tokio::test]
    async fn mock_writer_parses_put() {
        let llm = Arc::new(MockLlm {
            text: r#"[{"op":"put","scope":"org/w","text":"Alice prefers Rust.","category":"semantic","confidence":0.9}]"#.into(),
        });
        let w = CliChatWriter::new(llm);
        let ops = w
            .extract(&WriteEpisode {
                scope: "org/w".into(),
                text: "Alice prefers Rust for systems.".into(),
                source: Some("test".into()),
            })
            .await
            .unwrap();
        assert!(matches!(ops[0], WriterOp::Put { .. }));
    }

    #[tokio::test]
    async fn unparseable_is_noop() {
        let llm = Arc::new(MockLlm {
            text: "I think you should store that she likes cats.".into(),
        });
        let w = CliChatWriter::new(llm);
        let ops = w
            .extract(&WriteEpisode {
                scope: "org/w".into(),
                text: "she likes cats".into(),
                source: None,
            })
            .await
            .unwrap();
        assert!(matches!(ops[0], WriterOp::Noop { .. }));
    }

    #[tokio::test]
    async fn empty_prior_supersede_becomes_noop() {
        let llm = Arc::new(MockLlm {
            text: r#"[{"op":"supersede","prior_id":"","expected_version":1,"scope":"org/w","text":"x"}]"#.into(),
        });
        let w = CliChatWriter::new(llm);
        let ops = w
            .extract(&WriteEpisode {
                scope: "org/w".into(),
                text: "update fact".into(),
                source: None,
            })
            .await
            .unwrap();
        assert!(matches!(ops[0], WriterOp::Noop { .. }));
    }

    #[tokio::test]
    async fn single_object_accepted() {
        let llm = Arc::new(MockLlm {
            text:
                r#"{"op":"put","scope":"org/w","text":"Alice prefers Go.","category":"semantic"}"#
                    .into(),
        });
        let w = CliChatWriter::new(llm);
        let ops = w
            .extract(&WriteEpisode {
                scope: "org/w".into(),
                text: "prefers Go".into(),
                source: None,
            })
            .await
            .unwrap();
        assert!(matches!(ops[0], WriterOp::Put { .. }));
    }

    #[tokio::test]
    async fn empty_episode_noop() {
        let llm = Arc::new(MockLlm { text: "[]".into() });
        let w = CliChatWriter::new(llm);
        let ops = w
            .extract(&WriteEpisode {
                scope: "org/w".into(),
                text: "   ".into(),
                source: None,
            })
            .await
            .unwrap();
        match &ops[0] {
            WriterOp::Noop { reason: Some(r) } => assert!(r.contains("empty")),
            other => panic!("expected noop empty, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rewriter_includes_original() {
        let llm = Arc::new(MockLlm {
            text: r#"["TypeScript preference","Alice TypeScript"]"#.into(),
        });
        let r = CliQueryRewriter::new(llm);
        let q = r.rewrite("What does Alice prefer?").await.unwrap();
        assert_eq!(q[0], "What does Alice prefer?");
        assert!(q.len() >= 2);
    }
}
