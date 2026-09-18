//! Nomiso — SurrealDB-native agent memory foundation.
//!
//! # Quick start (embedded)
//!
//! ```no_run
//! use nomiso::{NomisoClient, PutRequest, SearchQuery, StoreConfig};
//! use nomiso::types::{Category, Content};
//!
//! # async fn demo() -> nomiso::Result<()> {
//! let client = NomisoClient::connect(StoreConfig::memory_test(8)).await?;
//! client
//!     .put(PutRequest {
//!         scope: "org/demo".into(),
//!         category: Category::Semantic,
//!         content: Content::text("Alice prefers TypeScript"),
//!         ..Default::default()
//!     })
//!     .await?;
//! let hits = client
//!     .search(SearchQuery {
//!         query: "TypeScript".into(),
//!         scope: "org/demo".into(),
//!         limit: Some(5),
//!         graph_enrich: Some(false),
//!         ..Default::default()
//!     })
//!     .await?;
//! assert!(!hits.is_empty());
//! # Ok(())
//! # }
//! ```
//!
//! No LLM API key is required for core put/search.

#![forbid(unsafe_code)]
#![doc(html_root_url = "https://docs.rs/nomiso")]

pub use nomiso_core::belief_event::{BeliefEvent, BeliefEventKind, ListBeliefEventsRequest};
pub use nomiso_core::embedding::{
    DeclareGenerationRequest, EmbeddingGeneration, EmbeddingIdentity, EmbeddingNormalization,
    EmbeddingState, GenerationStatus, SourceFrontier, StagedEmbedding,
};
pub use nomiso_core::error::{Error, Result};
pub use nomiso_core::evidence::{
    AnnotateRequest, ArtifactRecord, DerivedFromTarget, HistoryEntry, LinkDerivedFromRequest,
    PutArtifactRequest, PutSpanRequest, SpanRecord, SpanUnit,
};
pub use nomiso_core::job::{
    ClaimJobRequest, EnqueueJobRequest, EnqueueJobResult, JobAttempt, JobBudget, JobError,
    JobInput, JobIntent, JobLease, JobRecord, JobState, JobSummary, ListJobsRequest,
    WriteWithJobsResult,
};
pub use nomiso_core::list::{
    CountRequest, ListCursor, ListCursorQuery, ListItem, ListRequest, ListResponse,
};
pub use nomiso_core::ops::{
    ExpansionStats, FindCandidatesRequest, ForgetRequest, GraphExpand, PutRequest, ReadRequest,
    SearchOutcome, SearchQuery, SearchStats, SupersedeRequest, WriteResult,
};
pub use nomiso_core::relationship::{
    EndpointKind, EndpointRef, EntityRecord, EpistemicStatus, EvidenceRef,
    ListRelationshipsRequest, ProducerMeta, PutEntityRequest, PutRelationshipRequest,
    RelationPredicate, RelationshipRecord, RelationshipState, RelationshipWrite, TraverseBudget,
    TraverseDirection, TraverseNode, TraverseRequest, TraverseResult, UpdateEntityRequest,
    UpdateRelationshipRequest, RELATION_REGISTRY_VERSION,
};
pub use nomiso_core::scope::{ScopeMatch, ScopePath};
pub use nomiso_core::store::MemoryStore;
pub use nomiso_core::task_state::{GetTaskStateRequest, PutTaskStateRequest, TaskStateRecord};
pub use nomiso_core::trace::{
    cap_preview, AppendTraceEvent, ListTracesRequest, TraceBundle, TraceByMemory, TraceEventKind,
    TraceEventRecord, TraceHitCard, TraceOutcome, TraceSummary, TracesByMemoryRequest,
    TRACE_PREVIEW_MAX,
};
pub use nomiso_core::types;
pub use nomiso_core::types::{
    Category, Content, MemoryId, MemoryRecord, Provenance, ScoreKind, SearchHit, SearchSignals,
    Timestamp,
};
pub use nomiso_core::validate::{self, Limits};
pub use nomiso_core::{rrf_fuse, OP_ABI_VERSION};
pub use nomiso_schema::{self, MigrateOptions, SCHEMA_VERSION};
pub use nomiso_service::worker::{
    JobContext, JobExecutor, JobRun, ReindexExecutor, Worker, WorkerConfig, WorkerStats,
};
pub use nomiso_service::{Embedder, NomisoClient};
pub use nomiso_store::{SearchConfig, StoreConfig, SurrealMemoryStore};

#[cfg(feature = "blob")]
pub use nomiso_blob::{blake3_hex, BlobPut, BlobStore, FsBlobStore, S3BlobStore};

#[cfg(feature = "http")]
pub use nomiso_http::{router as http_router, AppState as HttpAppState};

#[cfg(feature = "mcp")]
pub use nomiso_mcp::{serve_stdio as mcp_serve_stdio, tool_names as mcp_tool_names, NomisoMcp};

#[cfg(feature = "embed")]
pub use nomiso_embed::{Embedder as EmbedderTrait, HashingEmbedder};
