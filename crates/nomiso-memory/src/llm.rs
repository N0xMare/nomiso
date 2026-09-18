//! Generic LLM completion surface for BYOM (no vendor SDK in Nomiso plane).
//!
//! Core trait is always available. Process backends (Grok CLI, Codex CLI) live
//! behind the `cli-llm` feature. HTTP / other transports plug the same trait.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// One completion turn: system + user text → model text.
///
/// Intentionally **not** OpenAI Completions-shaped. Backends map to whatever
/// surface they own (CLI, Responses, chat, etc.).
#[async_trait]
pub trait LlmCompletion: Send + Sync {
    /// Complete a system+user pair. Returns the model text body (not tool calls).
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse>;
}

/// Generic completion request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionRequest {
    /// System / developer instructions.
    pub system: String,
    /// User content for this turn.
    pub user: String,
    /// Optional model id (backend-specific; ignored if unset).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Soft max output tokens (advisory; backends may ignore).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Soft temperature (advisory).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
}

impl CompletionRequest {
    /// Build a simple system+user request.
    pub fn new(system: impl Into<String>, user: impl Into<String>) -> Self {
        Self {
            system: system.into(),
            user: user.into(),
            model: None,
            max_tokens: None,
            temperature: None,
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }
}

/// Generic completion response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionResponse {
    /// Primary text content from the model.
    pub text: String,
    /// Backend id (e.g. `grok-cli`, `codex-cli`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    /// Model used if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Strip markdown fences / leading prose and parse JSON of type `T`.
///
/// Fail-closed: returns `Error::Invalid` if no parseable JSON is found.
pub fn parse_json_from_model<T: for<'de> Deserialize<'de>>(raw: &str) -> Result<T> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(Error::invalid("empty model response"));
    }

    // Direct parse first.
    if let Ok(v) = serde_json::from_str::<T>(trimmed) {
        return Ok(v);
    }

    // Fenced ```json ... ``` or ``` ... ```
    if let Some(inner) = extract_fenced_json(trimmed) {
        if let Ok(v) = serde_json::from_str::<T>(inner) {
            return Ok(v);
        }
    }

    // First JSON array or object substring.
    if let Some(slice) = extract_json_substring(trimmed) {
        if let Ok(v) = serde_json::from_str::<T>(slice) {
            return Ok(v);
        }
    }

    Err(Error::invalid(format!(
        "failed to parse JSON from model response (len={})",
        trimmed.len()
    )))
}

/// Parse `WriterOp[]`, accepting a bare single object as a one-element array.
pub fn parse_writer_ops_from_model(raw: &str) -> Result<Vec<crate::writer::WriterOp>> {
    match parse_json_from_model::<Vec<crate::writer::WriterOp>>(raw) {
        Ok(ops) => Ok(ops),
        Err(arr_err) => match parse_json_from_model::<crate::writer::WriterOp>(raw) {
            Ok(op) => Ok(vec![op]),
            Err(_) => Err(arr_err),
        },
    }
}

fn extract_fenced_json(s: &str) -> Option<&str> {
    let start = s.find("```")?;
    let after = &s[start + 3..];
    // optional language tag
    let body = if after.starts_with("json") {
        after.strip_prefix("json")?.trim_start_matches(['\r', '\n'])
    } else if after.starts_with(['\r', '\n']) {
        after.trim_start_matches(['\r', '\n'])
    } else {
        // e.g. ```JSON
        let nl = after.find(['\n', '\r'])?;
        after[nl..].trim_start_matches(['\r', '\n'])
    };
    let end = body.find("```")?;
    Some(body[..end].trim())
}

fn extract_json_substring(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    let mut start = None;
    let mut depth: i32 = 0;
    let mut in_str = false;
    let mut escape = false;
    let mut open: u8 = 0;
    for (i, &b) in bytes.iter().enumerate() {
        if in_str {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'[' | b'{' => {
                if depth == 0 {
                    start = Some(i);
                    open = b;
                }
                depth += 1;
            }
            b']' | b'}' => {
                if depth == 0 {
                    continue;
                }
                let expect = if open == b'[' { b']' } else { b'}' };
                if b != expect && depth == 1 {
                    // mismatched top-level — reset
                    depth = 0;
                    start = None;
                    continue;
                }
                depth -= 1;
                if depth == 0 {
                    let s0 = start?;
                    return Some(&s[s0..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Fixed-response backend for offline tests (no process, no network).
#[derive(Debug, Clone)]
pub struct MockLlm {
    /// Fixed response text for every call.
    pub text: String,
}

#[async_trait]
impl LlmCompletion for MockLlm {
    async fn complete(&self, _request: &CompletionRequest) -> Result<CompletionResponse> {
        Ok(CompletionResponse {
            text: self.text.clone(),
            backend: Some("mock".into()),
            model: Some("mock".into()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::writer::WriterOp;

    #[test]
    fn parse_direct_array() {
        let ops: Vec<WriterOp> =
            parse_json_from_model(r#"[{"op":"noop","reason":"empty"}]"#).unwrap();
        assert!(matches!(ops[0], WriterOp::Noop { .. }));
    }

    #[test]
    fn parse_fenced_array() {
        let raw = r#"Here you go:
```json
[{"op":"put","scope":"org/x","text":"Alice prefers Rust.","category":"semantic"}]
```
"#;
        let ops: Vec<WriterOp> = parse_json_from_model(raw).unwrap();
        assert!(matches!(ops[0], WriterOp::Put { .. }));
    }

    #[test]
    fn parse_prose_wrapped() {
        let raw = r#"Sure.
[{"op":"noop","reason":"no durable fact"}]
Done."#;
        let ops: Vec<WriterOp> = parse_json_from_model(raw).unwrap();
        assert_eq!(ops.len(), 1);
    }

    #[test]
    fn parse_empty_fails() {
        assert!(parse_json_from_model::<Vec<WriterOp>>("").is_err());
    }
}
