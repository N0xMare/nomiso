//! Durable generic job journal (JOB-001..007).
//!
//! Job records are toolkit-owned durable representations persisted through the
//! same store as canonical memory state — no separate database, no writes that
//! bypass the commit boundary. The journal provides:
//!
//! - durable acceptance with a deterministic deduplication key (JOB-001/002),
//! - at-least-once execution with lease fencing: every effect path re-checks
//!   the active fence in the same transaction (JOB-003),
//! - revision-bound completion: pinned input revisions are revalidated inside
//!   the completion transaction, and purged inputs cannot be republished
//!   (JOB-004),
//! - bounded retries/backoff/deadline (JOB-005), truthful cancellation
//!   (JOB-006), and inspectable lifecycle with payload-free summaries
//!   (JOB-007).
//!
//! Workers and scheduling live above this layer; constructing a client never
//! starts a worker (JOB-006).

#![allow(missing_docs)]

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::relationship::EndpointKind;
use crate::types::Timestamp;

/// Maximum byte length of a job kind name.
pub const JOB_KIND_MAX: usize = 64;
/// Maximum inputs pinned on a single job.
pub const JOB_INPUTS_MAX: usize = 64;
/// Maximum serialized bytes of `payload`, `checkpoint`, and `result` bodies.
pub const JOB_BODY_MAX_BYTES: usize = 16 * 1024;
/// Maximum retained attempt-history entries (bounded, newest kept).
pub const JOB_HISTORY_MAX: usize = 20;
/// Default retry backoff base in milliseconds.
pub const JOB_BACKOFF_DEFAULT_MS: u64 = 1_000;

/// Lifecycle states per the JOB transition table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Pending,
    Leased,
    Succeeded,
    Failed,
    Cancelled,
    Superseded,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Leased => "leased",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Superseded => "superseded",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "leased" => Some(Self::Leased),
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            "superseded" => Some(Self::Superseded),
            _ => None,
        }
    }

    /// Terminal states commit no further work and free the dedup slot.
    pub fn terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::Superseded
        )
    }
}

/// A typed input reference with an optional pinned revision (JOB-004).
///
/// `revision` pins the optimistic version for versioned kinds
/// (memory/entity); for content-addressed artifacts and spans it is ignored —
/// the record id is already immutable identity. At completion, every input
/// must still exist at its pinned revision in the job's scope; a missing or
/// moved input rejects the commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct JobInput {
    pub kind: EndpointKind,
    /// Bare record key or `kind:key` form.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
}

/// Finite resource grant for a job (JOB-005).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct JobBudget {
    /// Maximum claim attempts before terminal failure (>= 1).
    pub max_attempts: u32,
    /// Lease duration in milliseconds.
    pub lease_ms: u64,
    /// Base retry backoff in milliseconds (default 1000); retries wait
    /// `backoff * 2^min(attempts,8)` before reacquisition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_backoff_ms: Option<u64>,
    /// Optional overall deadline in milliseconds from enqueue; no further
    /// attempt may be scheduled past it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<u64>,
}

impl JobBudget {
    /// Retry backoff for the given attempt count.
    pub fn backoff_ms(&self, attempts: u32) -> u64 {
        let base = self.retry_backoff_ms.unwrap_or(JOB_BACKOFF_DEFAULT_MS);
        base.saturating_mul(1u64 << attempts.min(8))
    }
}

/// Durable acceptance request (JOB-001). `dedup_key` is computed server-side
/// from scope, kind, pinned inputs, composition, and the optional hint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EnqueueJobRequest {
    pub scope: String,
    /// Registered workflow name — a stable identifier, never arbitrary text.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<JobInput>,
    /// Producer/policy/provider composition identity for this job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composition: Option<serde_json::Value>,
    /// Job body for the worker (bounded; not read by the journal).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    pub budget: JobBudget,
    /// Optional caller extension mixed into the dedup key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedup_hint: Option<String>,
}

/// A single lease attempt, retained in bounded job history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct JobAttempt {
    pub attempt: u32,
    pub worker: String,
    pub fence: u64,
    #[schemars(with = "String")]
    pub started_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub ended_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Safe error details recorded on a job (JOB-007).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct JobError {
    /// Stable machine-readable code.
    pub code: String,
    /// Human-safe summary; must not contain secrets or provider payloads.
    pub message: String,
    /// Whether the failure may be retried within the job's budget.
    pub retryable: bool,
}

/// The durable job record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct JobRecord {
    pub id: String,
    pub scope: String,
    pub kind: String,
    pub state: JobState,
    pub dedup_key: String,
    #[serde(default)]
    pub inputs: Vec<JobInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composition: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    pub budget: JobBudget,
    pub attempts: u32,
    /// Fencing token; incremented on every lease acquisition (JOB-003).
    pub fence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub lease_until: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub not_before: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub expires_at: Option<Timestamp>,
    /// Bounded durable progress: completed input range plus committed
    /// operation identities — never a raw model transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JobError>,
    /// For superseded jobs, the replacement job id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaced_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_reason: Option<String>,
    #[serde(default)]
    pub history: Vec<JobAttempt>,
    #[schemars(with = "String")]
    pub created_at: Timestamp,
    #[schemars(with = "String")]
    pub updated_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub completed_at: Option<Timestamp>,
}

/// Result of enqueue: the durable job plus whether this call deduplicated
/// against an existing pending/leased job of the same intent (JOB-001/002).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EnqueueJobResult {
    pub job: JobRecord,
    pub deduplicated: bool,
}

/// A durable job intent committed **atomically with a canonical write**
/// (JOB-001 "commit together"). Scope is inherited from the write — an intent
/// can never target a different partition than the record it accompanies.
///
/// `self_input` pins the record created by the write as a job input at its
/// initial revision (1 for put/supersede successors). The record key is
/// generated before the transaction, so the pinned reference resolves inside
/// the same commit — read-your-writes within the transaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct JobIntent {
    /// Registered workflow name — a stable identifier, never arbitrary text.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<JobInput>,
    /// When true, the record created by the write is appended as an input
    /// pinned at revision 1.
    #[serde(default)]
    pub self_input: bool,
    /// Producer/policy/provider composition identity for this job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composition: Option<serde_json::Value>,
    /// Job body for the worker (bounded; not read by the journal).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    pub budget: JobBudget,
    /// Optional caller extension mixed into the dedup key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedup_hint: Option<String>,
}

impl JobIntent {
    /// Resolve this intent against the write's scope and (optionally) the
    /// record key the write will create.
    pub fn resolve(&self, scope: &str, self_key: Option<&str>) -> EnqueueJobRequest {
        let mut inputs = self.inputs.clone();
        if self.self_input {
            if let Some(key) = self_key {
                inputs.push(JobInput {
                    kind: EndpointKind::Memory,
                    id: key.to_string(),
                    revision: Some(1),
                });
            }
        }
        EnqueueJobRequest {
            scope: scope.to_string(),
            kind: self.kind.clone(),
            inputs,
            composition: self.composition.clone(),
            payload: self.payload.clone(),
            budget: self.budget.clone(),
            dedup_hint: self.dedup_hint.clone(),
        }
    }
}

/// Result of an atomic write+enqueue: the committed write plus one outcome
/// per declared intent (JOB-001). `jobs` entries are durable — either created
/// in the write's transaction or deduplicated against an identical live
/// intent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WriteWithJobsResult {
    pub write: crate::ops::WriteResult,
    pub jobs: Vec<EnqueueJobResult>,
}

/// Claim request: the worker's identity, its restricted scope grant, and the
/// registered kinds it may run. A job outside the grant is never leased.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ClaimJobRequest {
    pub worker: String,
    /// Exact scopes this worker is permitted to execute for.
    pub scopes: Vec<String>,
    /// Registered kinds this worker may run.
    pub kinds: Vec<String>,
}

/// An acquired lease: the job snapshot plus the fence that authorizes effects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct JobLease {
    pub job: JobRecord,
    /// Fencing token for all subsequent effect calls.
    pub fence: u64,
    #[schemars(with = "String")]
    pub lease_until: Timestamp,
}

/// Lifecycle inspection filter (JOB-007). `list_jobs` returns summaries
/// without payload/checkpoint/result bodies.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ListJobsRequest {
    /// Exact owning scope.
    pub scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<JobState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

/// Payload-free lifecycle view for operators (JOB-007).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct JobSummary {
    pub id: String,
    pub scope: String,
    pub kind: String,
    pub state: JobState,
    pub dedup_key: String,
    pub attempts: u32,
    pub fence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub lease_until: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub not_before: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JobError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaced_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_reason: Option<String>,
    /// Whether a durable checkpoint exists (its body is not included).
    pub has_checkpoint: bool,
    /// Whether a result was committed (its body is not included).
    pub has_result: bool,
    #[schemars(with = "String")]
    pub created_at: Timestamp,
    #[schemars(with = "String")]
    pub updated_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub completed_at: Option<Timestamp>,
}

impl From<&JobRecord> for JobSummary {
    fn from(j: &JobRecord) -> Self {
        Self {
            id: j.id.clone(),
            scope: j.scope.clone(),
            kind: j.kind.clone(),
            state: j.state,
            dedup_key: j.dedup_key.clone(),
            attempts: j.attempts,
            fence: j.fence,
            owner: j.owner.clone(),
            lease_until: j.lease_until,
            not_before: j.not_before,
            error: j.error.clone(),
            replaced_by: j.replaced_by.clone(),
            terminal_reason: j.terminal_reason.clone(),
            has_checkpoint: j.checkpoint.is_some(),
            has_result: j.result.is_some(),
            created_at: j.created_at,
            updated_at: j.updated_at,
            completed_at: j.completed_at,
        }
    }
}

/// Validate an enqueue request shape (kind bounds, budget sanity, input
/// bounds, body sizes). Scope parsing happens in the store where the scope
/// model lives.
pub fn validate_enqueue(req: &EnqueueJobRequest) -> Result<()> {
    let kind = req.kind.trim();
    if kind.is_empty() || kind.len() > JOB_KIND_MAX {
        return Err(Error::invalid(format!(
            "job kind must be 1..{JOB_KIND_MAX} bytes"
        )));
    }
    if req.inputs.len() > JOB_INPUTS_MAX {
        return Err(Error::PayloadTooLarge(format!(
            "job inputs exceed {JOB_INPUTS_MAX}"
        )));
    }
    if req.budget.max_attempts == 0 {
        return Err(Error::invalid("job budget max_attempts must be >= 1"));
    }
    if req.budget.lease_ms == 0 {
        return Err(Error::invalid("job budget lease_ms must be > 0"));
    }
    for (name, body) in [("composition", &req.composition), ("payload", &req.payload)] {
        if let Some(v) = body {
            let bytes = serde_json::to_vec(v).map_err(|e| Error::invalid(e.to_string()))?;
            if bytes.len() > JOB_BODY_MAX_BYTES {
                return Err(Error::PayloadTooLarge(format!(
                    "job {name} exceeds {JOB_BODY_MAX_BYTES} bytes"
                )));
            }
            if !v.is_object() {
                return Err(Error::invalid(format!("job {name} must be an object")));
            }
        }
    }
    Ok(())
}

/// Validate a checkpoint or result body bound.
pub fn validate_job_body(name: &str, body: &serde_json::Value) -> Result<()> {
    let bytes = serde_json::to_vec(body).map_err(|e| Error::invalid(e.to_string()))?;
    if bytes.len() > JOB_BODY_MAX_BYTES {
        return Err(Error::PayloadTooLarge(format!(
            "job {name} exceeds {JOB_BODY_MAX_BYTES} bytes"
        )));
    }
    Ok(())
}
