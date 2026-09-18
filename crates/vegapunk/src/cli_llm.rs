//! Process backends for [`crate::llm::LlmCompletion`]: Grok CLI and Codex CLI.
//!
//! Gated by feature `cli-llm`. Uses local subscriptions (no API keys in Nomiso).

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tokio::process::Command;
use tokio::time::timeout;

use crate::error::{Error, Result};
use crate::llm::{CompletionRequest, CompletionResponse, LlmCompletion};

/// Default wall-clock budget for a single CLI completion.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(180);

/// Which headless CLI to invoke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliBackendKind {
    /// `grok -p` single-turn headless.
    Grok,
    /// `codex exec` non-interactive.
    Codex,
}

impl CliBackendKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Grok => "grok",
            Self::Codex => "codex",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "grok" | "grok-cli" => Some(Self::Grok),
            "codex" | "codex-cli" => Some(Self::Codex),
            _ => None,
        }
    }
}

/// Configuration for a process-backed completion backend.
#[derive(Debug, Clone)]
pub struct CliLlmConfig {
    pub kind: CliBackendKind,
    /// Binary name or absolute path (default: `grok` / `codex`).
    pub binary: PathBuf,
    /// Optional model flag.
    pub model: Option<String>,
    /// Timeout for the process.
    pub timeout: Duration,
    /// Working directory for the child (default: current dir).
    pub cwd: Option<PathBuf>,
}

impl CliLlmConfig {
    pub fn grok() -> Self {
        Self {
            kind: CliBackendKind::Grok,
            binary: PathBuf::from("grok"),
            model: None,
            timeout: DEFAULT_TIMEOUT,
            cwd: None,
        }
    }

    pub fn codex() -> Self {
        Self {
            kind: CliBackendKind::Codex,
            binary: PathBuf::from("codex"),
            model: None,
            timeout: DEFAULT_TIMEOUT,
            cwd: None,
        }
    }

    pub fn with_binary(mut self, bin: impl Into<PathBuf>) -> Self {
        self.binary = bin.into();
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }
}

/// Process-backed [`LlmCompletion`] (Grok or Codex CLI).
#[derive(Debug, Clone)]
pub struct CliLlm {
    config: CliLlmConfig,
}

impl CliLlm {
    pub fn new(config: CliLlmConfig) -> Self {
        Self { config }
    }

    pub fn grok() -> Self {
        Self::new(CliLlmConfig::grok())
    }

    pub fn codex() -> Self {
        Self::new(CliLlmConfig::codex())
    }

    pub fn config(&self) -> &CliLlmConfig {
        &self.config
    }
}

#[async_trait]
impl LlmCompletion for CliLlm {
    async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
        let model = request.model.clone().or_else(|| self.config.model.clone());
        match self.config.kind {
            CliBackendKind::Grok => self.run_grok(request, model.as_deref()).await,
            CliBackendKind::Codex => self.run_codex(request, model.as_deref()).await,
        }
    }
}

impl CliLlm {
    async fn run_grok(
        &self,
        request: &CompletionRequest,
        model: Option<&str>,
    ) -> Result<CompletionResponse> {
        // Flags before any positional content. System via override; user via -p.
        let mut cmd = Command::new(&self.config.binary);
        cmd.arg("--output-format")
            .arg("plain")
            .arg("--system-prompt-override")
            .arg(&request.system)
            .arg("--always-approve")
            .arg("--no-subagents")
            .arg("--no-plan")
            .arg("--verbatim")
            .arg("-p")
            .arg(&request.user)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(m) = model {
            cmd.arg("--model").arg(m);
        }
        if let Some(cwd) = &self.config.cwd {
            cmd.current_dir(cwd);
        }

        let output = run_with_timeout(cmd, self.config.timeout, "grok").await?;
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !output.status.success() {
            return Err(Error::llm(format!(
                "grok failed (status={:?}): stderr={stderr} stdout_len={}",
                output.status.code(),
                text.len()
            )));
        }
        if text.is_empty() {
            return Err(Error::llm(format!(
                "grok returned empty stdout (status={:?}): {stderr}",
                output.status.code()
            )));
        }
        Ok(CompletionResponse {
            text,
            backend: Some("grok-cli".into()),
            model: model.map(str::to_string),
        })
    }

    async fn run_codex(
        &self,
        request: &CompletionRequest,
        model: Option<&str>,
    ) -> Result<CompletionResponse> {
        // codex exec: all flags before the positional prompt; last message to -o file.
        let tmp = match tempfile::NamedTempFile::new() {
            Ok(f) => f,
            Err(e) => return Err(Error::llm(format!("codex tempfile: {e}"))),
        };
        let tmp_path = tmp.path().to_path_buf();
        // Keep file alive until we read it; NamedTempFile drops delete on drop.
        let _tmp_guard = tmp;

        let prompt = format!(
            "{}\n\n---\n\n{}",
            request.system.trim(),
            request.user.trim()
        );

        let mut cmd = Command::new(&self.config.binary);
        cmd.arg("exec")
            .arg("--ephemeral")
            .arg("--skip-git-repo-check")
            .arg("--sandbox")
            .arg("read-only")
            .arg("--color")
            .arg("never")
            .arg("--output-last-message")
            .arg(&tmp_path);
        if let Some(m) = model {
            cmd.arg("--model").arg(m);
        }
        // Positional prompt last.
        cmd.arg(prompt)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(cwd) = &self.config.cwd {
            cmd.current_dir(cwd);
        }

        let output = match run_with_timeout(cmd, self.config.timeout, "codex").await {
            Ok(o) => o,
            Err(e) => {
                // NamedTempFile drops on scope exit.
                return Err(e);
            }
        };
        let file_text = tokio::fs::read_to_string(&tmp_path)
            .await
            .unwrap_or_default();
        let stderr = String::from_utf8_lossy(&output.stderr);

        let text = file_text.trim().to_string();
        if !output.status.success() {
            return Err(Error::llm(format!(
                "codex failed (status={:?}): {stderr}",
                output.status.code()
            )));
        }
        if text.is_empty() {
            return Err(Error::llm(format!(
                "codex returned empty last-message file (status={:?}): {stderr}",
                output.status.code()
            )));
        }

        Ok(CompletionResponse {
            text,
            backend: Some("codex-cli".into()),
            model: model.map(str::to_string),
        })
    }
}

async fn run_with_timeout(
    mut cmd: Command,
    dur: Duration,
    name: &str,
) -> Result<std::process::Output> {
    let fut = cmd.output();
    match timeout(dur, fut).await {
        Ok(Ok(out)) => Ok(out),
        Ok(Err(e)) => Err(Error::llm(format!("{name} spawn/io error: {e}"))),
        Err(_) => Err(Error::llm(format!(
            "{name} timed out after {}s",
            dur.as_secs()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_kind_parse() {
        assert_eq!(CliBackendKind::parse("grok"), Some(CliBackendKind::Grok));
        assert_eq!(
            CliBackendKind::parse("codex-cli"),
            Some(CliBackendKind::Codex)
        );
        assert!(CliBackendKind::parse("openai").is_none());
    }
}
