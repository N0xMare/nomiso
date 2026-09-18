//! `nomiso-memory` — reusable agent-memory mechanics on the Nomiso plane.
//!
//! This crate is the T3 toolkit: policy-parameterized memory mechanics that
//! products (Vegapunk) or harnesses compose directly. It carries **no product
//! defaults** — a [`MemoryPolicy`] value comes in from the caller, the host
//! stays authoritative over what is injected into a prompt, and storage stays
//! authoritative over durability.
//!
//! - **Writer path**: [`WriterOp`], [`preflight_ops`], [`apply_ops`]
//!   (prefix-preserving, per-op outcomes), [`MemoryWriter`]/[`RuleWriter`] and
//!   the BYOM [`LlmCompletion`] port.
//! - **Reader path**: [`recall`], [`hard_recall`], [`hard_recall_pack`],
//!   [`pack_context`] — explicit packs, never implicit injection.
//! - **Mechanics**: [`remember`], [`checkpoint`], [`sleep_pass`],
//!   [`store_artifact`], trace emission helpers.

#![forbid(unsafe_code)]

pub mod artifact;
pub mod checkpoint;
pub mod cli_writer;
pub mod context;
pub mod error;
pub mod llm;
pub mod policy;
pub mod reader;
pub mod recall;
pub mod remember;
pub mod sleep;
pub mod trace_emit;
pub mod types;
pub mod writer;

pub use artifact::{
    relocate_artifacts, ArtifactRelocation, ArtifactRelocationReport, BlobConfig,
    StoreArtifactInput, StoreArtifactJson,
};
pub use checkpoint::{checkpoint, CheckpointOutcome};
pub use cli_writer::{
    CliChatWriter, CliQueryRewriter, REWRITER_SYSTEM_PROMPT, WRITER_SYSTEM_PROMPT,
};
pub use context::{
    prepare_context, record_insertion, BlockProvenance, BudgetUsage, CandidateRecord,
    CompositionIdentity, ContextBlock, ContextBudget, ContextProposal, DegradationPolicy, Effort,
    ExclusionReason, FollowUp, InsertedBlock, InsertionAck, InsertionReceipt, InventoryItem,
    PrepareContextRequest, ProposalStatus, SelectionManifest, TokenMethod,
};
pub use error::{Error, Result};
pub use llm::{
    parse_json_from_model, parse_writer_ops_from_model, CompletionRequest, CompletionResponse,
    LlmCompletion, MockLlm,
};
pub use policy::MemoryPolicy;
pub use reader::{
    hard_recall, hard_recall_pack, pack_context, ContextCard, ContextPack, HardRecallOptions,
    HardRecallResult, QueryRewriter, RuleQueryRewriter,
};
pub use recall::{parse_timestamp, recall, EnumerateOptions, RecallOptions};
pub use remember::{infer_category, prepare_put, remember, RememberInput};
pub use sleep::{
    applied_from_outcomes, forget_ops_from_proposals, sleep_pass, SleepApplied, SleepOptions,
    SleepProposal, SleepReport,
};
pub use trace_emit::{
    emit_apply_ops, emit_hard_recall, emit_inject, emit_outcome, emit_outcome_attributed,
    emit_remember, new_trace_id, Evaluator, OutcomeMeta,
};
pub use types::{CheckpointInput, CompactionIngest, CompactionReport, RememberOutcome};
#[allow(deprecated)]
pub use writer::apply_ops_atomic;
pub use writer::{
    apply_ops, op_scope, preflight_ops, supersede_remember, write_episode, ApplyOpOutcome,
    ApplyResult, MemoryWriter, RuleWriter, WriteEpisode, WriterOp,
};
