//! Nomiso core: domain types, op ABI (`nomiso.op.v1`), validation, and store traits.
//!
//! This crate has **no** SurrealDB, HTTP, MCP, or LLM dependencies.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod belief_event;
pub mod embedding;
pub mod error;
pub mod evidence;
pub mod fusion;
pub mod job;
pub mod list;
pub mod ops;
pub mod relationship;
pub mod scope;
pub mod store;
pub mod task_state;
pub mod trace;
pub mod types;
pub mod validate;

pub use belief_event::{BeliefEvent, BeliefEventKind, ListBeliefEventsRequest};
pub use embedding::{
    DeclareGenerationRequest, EmbeddingGeneration, EmbeddingIdentity, EmbeddingNormalization,
    EmbeddingState, GenerationStatus, SourceFrontier, StagedEmbedding,
};
pub use error::{Error, Result};
pub use evidence::{
    AnnotateRequest, ArtifactRecord, DerivedFromTarget, HistoryEntry, LinkDerivedFromRequest,
    PutArtifactRequest, PutSpanRequest, SpanRecord, SpanUnit,
};
pub use fusion::rrf_fuse;
pub use job::{
    ClaimJobRequest, EnqueueJobRequest, EnqueueJobResult, JobAttempt, JobBudget, JobError,
    JobInput, JobIntent, JobLease, JobRecord, JobState, JobSummary, ListJobsRequest,
    WriteWithJobsResult,
};
pub use list::{CountRequest, ListCursor, ListCursorQuery, ListItem, ListRequest, ListResponse};
pub use ops::{
    ExpansionStats, FindCandidatesRequest, ForgetRequest, GraphExpand, PutRequest, ReadRequest,
    SearchOutcome, SearchQuery, SearchStats, SupersedeRequest, WriteResult,
};
pub use relationship::{
    EndpointKind, EndpointRef, EntityRecord, EpistemicStatus, EvidenceRef,
    ListRelationshipsRequest, ProducerMeta, PutEntityRequest, PutRelationshipRequest,
    RelationPredicate, RelationshipRecord, RelationshipState, RelationshipWrite, TraverseBudget,
    TraverseDirection, TraverseNode, TraverseRequest, TraverseResult, UpdateEntityRequest,
    UpdateRelationshipRequest, RELATION_REGISTRY_VERSION,
};
pub use scope::{ScopeMatch, ScopePath};
pub use store::MemoryStore;
pub use task_state::{GetTaskStateRequest, PutTaskStateRequest, TaskStateRecord};
pub use trace::{
    cap_preview, AppendTraceEvent, ListTracesRequest, TraceBundle, TraceByMemory, TraceEventKind,
    TraceEventRecord, TraceHitCard, TraceOutcome, TraceSummary, TracesByMemoryRequest,
    TRACE_PREVIEW_MAX,
};
pub use types::{
    Category, Content, ExpansionInfo, MemoryId, MemoryRecord, Provenance, ScoreKind, SearchHit,
    SearchSignals, Timestamp,
};

/// Structured memory op contract version string (`nomiso.op.v1`).
///
/// This labels the current op surface; it is **not** a frozen public ABI guarantee.
/// Breaking changes may still land under the same tag until an explicit freeze gate.
pub const OP_ABI_VERSION: &str = "nomiso.op.v1";
