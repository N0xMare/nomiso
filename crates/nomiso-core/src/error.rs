//! Error taxonomy for Nomiso operations.

use thiserror::Error;

/// Convenient result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Structured errors returned by the library (never panics on library paths).
#[derive(Debug, Error)]
pub enum Error {
    /// Schema or validation failure (interval, empty text, bad scope, …).
    #[error("invalid operation: {0}")]
    InvalidOp(String),

    /// Optimistic version mismatch on mutate.
    #[error("version conflict: expected {expected}, found {found}")]
    Conflict {
        /// Version the client expected.
        expected: u64,
        /// Version currently stored.
        found: u64,
    },

    /// Missing id for read / supersede / forget.
    #[error("not found: {0}")]
    NotFound(String),

    /// Cross-scope access attempt (fail closed).
    #[error("scope denied: {0}")]
    ScopeDenied(String),

    /// Embedding length does not match configured dimension.
    #[error("embedding dimension mismatch: expected {expected}, got {got}")]
    DimensionMismatch {
        /// Configured HNSW / store dimension.
        expected: usize,
        /// Length of the provided vector.
        got: usize,
    },

    /// Content or payload exceeds configured limits.
    #[error("payload too large: {0}")]
    PayloadTooLarge(String),

    /// Underlying store / SurrealDB / connectivity failure.
    #[error("store error: {0}")]
    Store(String),

    /// Embedding/model provider unreachable or unusable (transport, auth, quota).
    #[error("provider unavailable: {0}")]
    ProviderUnavailable(String),

    /// Provider responded but the payload violated the contract.
    #[error("invalid provider response: {0}")]
    InvalidProviderResponse(String),

    /// Operation exceeded its deadline.
    #[error("operation deadline exceeded")]
    DeadlineExceeded,

    /// Caller-supplied cancellation token fired before or during the
    /// operation (OPS-001). Distinct from `DeadlineExceeded`: cancellation
    /// is caller-initiated, not a time budget.
    #[error("operation cancelled by caller")]
    Cancelled,

    #[doc = "An idempotency key was already committed with different input."]
    #[error("idempotency key reused with different input")]
    IdempotencyConflict,

    #[doc = "A legacy or erased receipt cannot be replayed safely."]
    #[error("idempotency receipt cannot be safely replayed")]
    IdempotencyUnavailable,

    /// Store schema/index generation is incompatible with this binary
    /// (e.g. a newer or unrecognized schema marker). Fail closed.
    #[error("incompatible store: {0}")]
    IncompatibleStore(String),

    /// A job lease fencing token no longer matches: the lease expired and was
    /// reacquired, or the job was cancelled/superseded. This worker may not
    /// commit further effects (JOB-003).
    #[error("job lease lost: {0}")]
    LeaseLost(String),

    /// A job's declared inputs no longer satisfy their pinned revisions — a
    /// referenced row was superseded, erased, or never existed (JOB-004).
    /// The job must be replanned; a valid lease does not override foreground
    /// changes.
    #[error("stale job input: {0}")]
    StaleInput(String),

    /// Internal invariant broken (should be rare).
    #[error("internal error: {0}")]
    Internal(String),
}

impl Error {
    /// Convenience constructor for invalid ops.
    pub fn invalid(msg: impl Into<String>) -> Self {
        Self::InvalidOp(msg.into())
    }

    /// Convenience constructor for store errors.
    pub fn store(msg: impl Into<String>) -> Self {
        Self::Store(msg.into())
    }

    /// Convenience constructor for internal errors.
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::Internal(msg.into())
    }

    /// Stable machine-readable error code for adapters (HTTP status, MCP, CLI JSON).
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidOp(_) => "invalid_request",
            Self::Conflict { .. } => "conflict",
            Self::NotFound(_) => "not_found",
            Self::ScopeDenied(_) => "scope_denied",
            Self::DimensionMismatch { .. } => "dimension_mismatch",
            Self::PayloadTooLarge(_) => "payload_too_large",
            Self::Store(_) => "store_error",
            Self::ProviderUnavailable(_) => "provider_unavailable",
            Self::InvalidProviderResponse(_) => "invalid_provider_response",
            Self::DeadlineExceeded => "deadline_exceeded",
            Self::Cancelled => "cancelled",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::IdempotencyUnavailable => "idempotency_unavailable",
            Self::IncompatibleStore(_) => "incompatible_store",
            Self::LeaseLost(_) => "lease_lost",
            Self::StaleInput(_) => "stale_input",
            Self::Internal(_) => "internal_error",
        }
    }

    /// Message safe to surface to untrusted callers (no store internals or
    /// provider payloads).
    pub fn public_message(&self) -> String {
        match self {
            Self::Store(_) => "storage operation failed".into(),
            Self::IncompatibleStore(_) => "store schema is incompatible with this binary".into(),
            Self::LeaseLost(_) => "job lease no longer held".into(),
            Self::StaleInput(_) => "job inputs no longer match their pinned revisions".into(),
            Self::Internal(_) => "internal invariant failure".into(),
            Self::ScopeDenied(_) => "scope access denied".into(),
            Self::ProviderUnavailable(_) => "embedding/model provider unavailable".into(),
            Self::InvalidProviderResponse(_) => "invalid provider response".into(),
            Self::DeadlineExceeded => "operation deadline exceeded".into(),
            Self::Cancelled => "operation cancelled".into(),
            _ => self.to_string(),
        }
    }
}
