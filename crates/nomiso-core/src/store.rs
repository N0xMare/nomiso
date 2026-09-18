//! Abstract memory store trait (no Surreal types).

use async_trait::async_trait;

use crate::belief_event::BeliefEvent;
use crate::embedding::{
    DeclareGenerationRequest, EmbeddingGeneration, EmbeddingIdentity, EmbeddingState,
    StagedEmbedding,
};
use crate::error::Result;
use crate::evidence::{
    AnnotateRequest, ArtifactRecord, HistoryEntry, LinkDerivedFromRequest, PutArtifactRequest,
    PutSpanRequest, SpanRecord,
};
use crate::job::{
    ClaimJobRequest, EnqueueJobRequest, EnqueueJobResult, JobError, JobIntent, JobLease, JobRecord,
    JobSummary, ListJobsRequest, WriteWithJobsResult,
};
use crate::list::{CountRequest, ListRequest, ListResponse};
use crate::ops::{
    FindCandidatesRequest, ForgetRequest, PutRequest, ReadRequest, SearchOutcome, SearchQuery,
    SupersedeRequest, WriteResult,
};
use crate::relationship::{
    EntityRecord, ListRelationshipsRequest, PutEntityRequest, PutRelationshipRequest,
    RelationshipRecord, RelationshipWrite, TraverseRequest, TraverseResult, UpdateEntityRequest,
    UpdateRelationshipRequest,
};
use crate::task_state::{GetTaskStateRequest, PutTaskStateRequest, TaskStateRecord};
use crate::trace::{
    AppendTraceEvent, ListTracesRequest, TraceBundle, TraceByMemory, TraceEventRecord,
    TraceSummary, TracesByMemoryRequest,
};
use crate::types::{MemoryId, MemoryRecord, SearchHit};

/// Async memory store contract implemented by `nomiso-store`.
#[async_trait]
pub trait MemoryStore: Send + Sync {
    /// Apply schema migrations if needed.
    async fn migrate(&self) -> Result<()>;

    /// Validation limits enforced by this store.
    fn limits(&self) -> crate::validate::Limits;

    /// Insert a structured memory.
    async fn put(&self, req: PutRequest) -> Result<WriteResult>;

    #[doc = "Look up a verified keyed-put receipt without contacting an embedder."]
    async fn lookup_put_receipt(&self, req: &PutRequest) -> Result<Option<WriteResult>>;

    #[doc = "Commit the original put intent with an optional separately generated embedding."]
    async fn put_prepared(
        &self,
        req: PutRequest,
        generated_embedding: Option<Vec<f32>>,
    ) -> Result<WriteResult>;

    /// Supersede a prior memory in one ACID transaction.
    async fn supersede(&self, req: SupersedeRequest) -> Result<WriteResult>;

    /// Receipt lookup for a keyed write+enqueue request: the slot identity
    /// covers the put AND the declared intents (JOB-001). On hit, the
    /// returned jobs are the already-durable intents (dedup-replayed) — no
    /// provider call needed.
    async fn lookup_put_receipt_with_intents(
        &self,
        req: &PutRequest,
        intents: &[JobIntent],
    ) -> Result<Option<WriteWithJobsResult>>;

    /// Commit a canonical put plus durable job intents in ONE transaction
    /// (JOB-001). Intents inherit the write's scope; `self_input` pins the
    /// created record at revision 1. If any intent is invalid the write
    /// rolls back — the promise is durable with the mutation or not at all.
    /// A live identical intent dedups rather than failing the write.
    async fn put_with_jobs(
        &self,
        req: PutRequest,
        generated_embedding: Option<Vec<f32>>,
        intents: Vec<JobIntent>,
    ) -> Result<WriteWithJobsResult>;

    /// Same atomic contract for supersede: close+successor commit together
    /// with durable job intents; `self_input` pins the successor at rev 1.
    async fn supersede_with_jobs(
        &self,
        req: SupersedeRequest,
        intents: Vec<JobIntent>,
    ) -> Result<WriteWithJobsResult>;

    /// Hybrid search under scope / validity filters.
    async fn search(&self, query: SearchQuery) -> Result<Vec<SearchHit>> {
        Ok(self.search_detailed(query).await?.hits)
    }

    /// Search plus retrieval-cost reporting (graph-expansion stats when
    /// `graph_expand` is set). `search` delegates to this.
    async fn search_detailed(&self, query: SearchQuery) -> Result<SearchOutcome>;

    /// Open-validity priors for a new claim (Put vs Supersede discovery).
    ///
    /// Default impl is valid-now [`search`]. Implementors may override.
    async fn find_candidates(&self, req: FindCandidatesRequest) -> Result<Vec<SearchHit>> {
        self.search(req.into_search()).await
    }

    /// Read full records by id (scope-checked).
    async fn read(&self, req: ReadRequest) -> Result<Vec<MemoryRecord>>;

    /// Soft-expire or hard-delete.
    async fn forget(&self, req: ForgetRequest) -> Result<()>;

    /// Health check.
    async fn health(&self) -> Result<()>;

    /// Per-table row counts — the declared consistent frontier recorded in
    /// snapshot manifests and checked after restore (OPS-004). Keys are the
    /// schema's canonical table names; absent tables report 0.
    async fn table_counts(&self) -> Result<std::collections::BTreeMap<String, u64>>;

    // --- Phase 2.5 traces ---

    /// Append a trace event (lazy-creates parent row).
    async fn append_trace_event(&self, req: AppendTraceEvent) -> Result<TraceEventRecord>;

    /// Load parent + events for a trace_id (scope-checked).
    async fn get_trace(&self, trace_id: &str, scope: &str) -> Result<TraceBundle>;

    /// List parent traces under a scope (optional created_at window).
    async fn list_traces(&self, req: ListTracesRequest) -> Result<Vec<TraceSummary>>;

    /// List traces that referenced a memory id, with latest host outcome.
    ///
    /// Scope is a tenant filter, not authorization. Unknown id and foreign
    /// scope return an empty list (do not leak).
    async fn list_traces_for_memory(
        &self,
        req: TracesByMemoryRequest,
    ) -> Result<Vec<TraceByMemory>>;

    // --- Phase 3a list ---

    /// Enumerate memories under scope (optional text filter + cursor).
    async fn list(&self, req: ListRequest) -> Result<ListResponse>;

    /// Count memories matching list filters.
    async fn count(&self, req: CountRequest) -> Result<u64>;

    // --- Phase 3b/3c evidence ---

    /// Register content-addressed artifact metadata (bytes outside Surreal).
    async fn put_artifact(&self, req: PutArtifactRequest) -> Result<ArtifactRecord>;

    /// List artifact records under an exact scope, or all scopes when
    /// `None` (operator/relocation use — callers filter, not the store).
    async fn list_artifacts(&self, scope: Option<&str>) -> Result<Vec<ArtifactRecord>>;

    /// Atomically rebind an artifact's blob location, guarded on the
    /// expected current value (OPS-002 relocation). `Ok(false)` means the
    /// guard failed — the row changed under us; re-read and retry, never
    /// overwrite blindly.
    async fn rebind_artifact_location(
        &self,
        id: &str,
        expected_location: &str,
        new_location: &str,
    ) -> Result<bool>;

    /// Register a span into an artifact.
    async fn put_span(&self, req: PutSpanRequest) -> Result<SpanRecord>;

    /// Link a memory as derived from artifact, span, or another memory.
    async fn link_derived_from(&self, req: LinkDerivedFromRequest) -> Result<()>;

    // --- Phase 3d ---

    /// Walk supersession chain for a memory id.
    async fn history(&self, id: &MemoryId, scope: &str) -> Result<Vec<HistoryEntry>>;

    /// Annotate metadata without forging valid time.
    async fn annotate(&self, req: AnnotateRequest) -> Result<WriteResult>;

    /// Put/replace working-state slot (Phase 3.5).
    async fn put_task_state(&self, req: PutTaskStateRequest) -> Result<TaskStateRecord>;

    /// Get working-state slot.
    async fn get_task_state(&self, req: GetTaskStateRequest) -> Result<Option<TaskStateRecord>>;

    /// List belief_event rows for a memory (Phase 3e audit journal).
    ///
    /// Required events commit in the same transaction as the mutation they
    /// describe; a failed mutation leaves no event.
    async fn list_belief_events(
        &self,
        memory_id: &MemoryId,
        scope: &str,
    ) -> Result<Vec<BeliefEvent>>;

    // --- Phase 4 relationships ---

    /// Create a scoped entity/resource record (opaque id, versioned).
    async fn put_entity(&self, req: PutEntityRequest) -> Result<EntityRecord>;

    /// Read an entity by id (scope-checked).
    async fn get_entity(&self, id: &str, scope: &str) -> Result<EntityRecord>;

    /// Versioned entity update (CAS on expected_version).
    async fn update_entity(&self, req: UpdateEntityRequest) -> Result<EntityRecord>;

    /// Create a typed relationship. Validates predicate registry, endpoint
    /// kinds, endpoint existence, and same-scope before mutation (REL-001).
    /// An identical active edge returns the existing record (`replayed`).
    async fn put_relationship(&self, req: PutRelationshipRequest) -> Result<RelationshipWrite>;

    /// Read a relationship by id (scope-checked).
    async fn get_relationship(&self, id: &str, scope: &str) -> Result<RelationshipRecord>;

    /// Versioned relationship update (CAS on expected_version); the prior
    /// revision's state and evidence are preserved in `relationship_event`
    /// in the same transaction (REL-002).
    async fn update_relationship(
        &self,
        req: UpdateRelationshipRequest,
    ) -> Result<RelationshipRecord>;

    /// List relationships under a scope (optional endpoint/predicate/state).
    async fn list_relationships(
        &self,
        req: ListRelationshipsRequest,
    ) -> Result<Vec<RelationshipRecord>>;

    /// Bounded same-scope BFS traversal with dedup, cycle handling, path
    /// provenance, and truncation reporting (REL-003).
    async fn traverse(&self, req: TraverseRequest) -> Result<TraverseResult>;

    // --- Embedding identity and generations (MIG-004/005) ---

    /// Recorded embedding generations and the active one.
    async fn embedding_state(&self) -> Result<EmbeddingState>;

    /// Attest that the active generation's unknown identity is `identity`
    /// (MIG-004 legacy path). Fails when the active generation already carries
    /// a declared identity — use `declare_embedding_generation` instead.
    async fn attest_embedding_identity(
        &self,
        identity: EmbeddingIdentity,
    ) -> Result<EmbeddingGeneration>;

    /// Declare a new staged generation built from a captured source frontier.
    /// Only one generation may be `building` at a time; the active generation
    /// keeps serving queries until explicit activation.
    async fn declare_embedding_generation(
        &self,
        req: DeclareGenerationRequest,
    ) -> Result<EmbeddingGeneration>;

    /// Stage re-embedded vectors for a building generation (idempotent per
    /// memory). Returns the generation's staged count.
    async fn stage_embeddings(&self, generation: u64, items: Vec<StagedEmbedding>) -> Result<u64>;

    /// Validate a building/retired generation covers every embedded memory,
    /// then atomically swap it active (retiring the old one). An incomplete
    /// generation is never silently treated as complete.
    async fn activate_embedding_generation(&self, generation: u64) -> Result<EmbeddingGeneration>;

    // ---- Durable job journal (JOB-001..007) ----

    /// Durably enqueue a job intent (JOB-001). The dedup key is computed
    /// from scope, kind, pinned inputs, composition, and the caller hint;
    /// an identical live intent replays the existing job with
    /// `deduplicated: true`. Inputs must exist in the job's scope.
    async fn enqueue_job(&self, req: EnqueueJobRequest) -> Result<EnqueueJobResult>;

    /// Fetch a job by id within its exact scope.
    async fn get_job(&self, scope: &str, id: &str) -> Result<JobRecord>;

    /// Inspect lifecycle without raw payload bodies (JOB-007).
    async fn list_jobs(&self, req: ListJobsRequest) -> Result<Vec<JobSummary>>;

    /// Acquire a lease on an eligible job (pending past `not_before`, or a
    /// leased job whose `lease_until` expired). Only jobs inside the
    /// worker's scope grant and kind allowlist are candidates. Each
    /// acquisition increments the fencing token (JOB-003).
    async fn claim_job(&self, req: ClaimJobRequest) -> Result<Option<JobLease>>;

    /// Extend the current lease. Fence-checked; a stale holder is rejected.
    async fn renew_job_lease(&self, id: &str, fence: u64, worker: &str) -> Result<JobLease>;

    /// Persist a bounded checkpoint under the active fence (JOB-003).
    async fn checkpoint_job(
        &self,
        id: &str,
        fence: u64,
        worker: &str,
        checkpoint: serde_json::Value,
    ) -> Result<JobRecord>;

    /// Commit final effects: fence-checked, and pinned input revisions are
    /// revalidated inside the same transaction (JOB-003/004). A stale or
    /// purged input rejects the commit and leaves the job leased.
    async fn complete_job(
        &self,
        id: &str,
        fence: u64,
        worker: &str,
        result: serde_json::Value,
    ) -> Result<JobRecord>;

    /// Record a failure under the active fence. Retryable failures within
    /// budget return the job to pending with backoff; exhausted budget or
    /// non-retryable failure is terminal (JOB-005).
    async fn fail_job(
        &self,
        id: &str,
        fence: u64,
        worker: &str,
        error: JobError,
    ) -> Result<JobRecord>;

    /// Authorized cancellation (JOB-006): pending or leased jobs become
    /// cancelled. The returned record exposes any committed checkpoint —
    /// cancellation is not rollback.
    async fn cancel_job(&self, scope: &str, id: &str, reason: &str) -> Result<JobRecord>;

    /// Replace a pending/leased job with a newer desired job.
    async fn supersede_job(
        &self,
        scope: &str,
        id: &str,
        replacement_id: &str,
        reason: &str,
    ) -> Result<JobRecord>;
}
