//! Toolkit errors (wrap plane errors + policy).

use thiserror::Error;

/// Result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Memory-toolkit error taxonomy.
#[derive(Debug, Error)]
pub enum Error {
    /// Underlying Nomiso plane error.
    #[error(transparent)]
    Nomiso(#[from] nomiso_core::Error),

    /// Policy rejected the operation.
    #[error("policy: {0}")]
    Policy(String),

    /// Invalid user input at the toolkit layer.
    #[error("invalid input: {0}")]
    Invalid(String),

    /// I/O (CLI file load, etc.).
    #[error("io: {0}")]
    Io(String),

    /// CAS/blob backend failure (integrity, not-found, transport).
    #[error("blob: {0}")]
    Blob(#[from] nomiso_blob::BlobError),

    /// BYOM / LLM backend failure (CLI process, timeout, empty response).
    #[error("llm: {0}")]
    Llm(String),
}

impl Error {
    pub fn invalid(msg: impl Into<String>) -> Self {
        Self::Invalid(msg.into())
    }

    pub fn llm(msg: impl Into<String>) -> Self {
        Self::Llm(msg.into())
    }

    pub fn io(msg: impl Into<String>) -> Self {
        Self::Io(msg.into())
    }

    /// Stable machine-readable code for CLI JSON / HTTP / MCP adapters.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Nomiso(e) => e.code(),
            Self::Policy(_) => "policy_rejected",
            Self::Invalid(_) => "invalid_request",
            Self::Io(_) => "io_error",
            Self::Blob(e) => e.code(),
            Self::Llm(_) => "provider_unavailable",
        }
    }

    /// Message safe to surface to callers (Io/Blob transport/Llm internals
    /// are dropped — they can carry paths, authorities, or provider text).
    pub fn public_message(&self) -> String {
        match self {
            Self::Nomiso(e) => e.public_message(),
            Self::Io(_) => "io operation failed".into(),
            Self::Blob(e) => e.public_message(),
            Self::Llm(_) => "model provider unavailable".into(),
            _ => self.to_string(),
        }
    }
}
