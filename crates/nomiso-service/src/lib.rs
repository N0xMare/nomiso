//! Application service / client facade for Nomiso.

#![forbid(unsafe_code)]

pub mod worker;

use std::sync::Arc;

use async_trait::async_trait;
use nomiso_core::error::Result;
use nomiso_core::ops::{
    FindCandidatesRequest, ForgetRequest, PutRequest, ReadRequest, SearchQuery, SupersedeRequest,
    WriteResult,
};
use nomiso_core::store::MemoryStore;
use nomiso_core::types::{MemoryRecord, SearchHit};
use nomiso_store::{StoreConfig, SurrealMemoryStore};
use tracing::instrument;

/// Inspectable provider readiness report (OPS-001). Returned as data — a
/// probe that finds the provider down is a `ready: false` status, not an
/// `Err`, so operators can inspect the reason.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ProviderStatus {
    /// Provider accepted the probe within the deadline.
    pub ready: bool,
    /// Human-safe detail (never secrets/keys): e.g. `http status 503`,
    /// `probe timeout`, `missing api key`.
    pub detail: Option<String>,
    /// Observed probe latency.
    pub latency_ms: u64,
}

impl ProviderStatus {
    /// Ready with no detail.
    pub fn ok(latency_ms: u64) -> Self {
        Self {
            ready: true,
            detail: None,
            latency_ms,
        }
    }

    /// Not ready, with a caller-safe reason.
    pub fn down(detail: impl Into<String>, latency_ms: u64) -> Self {
        Self {
            ready: false,
            detail: Some(detail.into()),
            latency_ms,
        }
    }
}

/// Optional embedder supplied by the host (no HTTP OpenAI client in core).
#[async_trait]
pub trait Embedder: Send + Sync {
    /// Embed one or more texts into vectors of the store dimension.
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;

    /// Cancellation-aware embed (OPS-001). The default impl checks the token
    /// before delegating to `embed`; transports that can abort in-flight
    /// requests override this. `Error::Cancelled` is returned when the token
    /// fires — distinct from `DeadlineExceeded`.
    async fn embed_cancellable(
        &self,
        texts: &[String],
        cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<Vec<Vec<f32>>> {
        if cancel.is_some_and(|c| c.is_cancelled()) {
            return Err(nomiso_core::Error::Cancelled);
        }
        self.embed(texts).await
    }

    /// Inspectable readiness probe (OPS-001): is the provider reachable and
    /// configured *now*? Bounded by the provider's own deadline. Default is
    /// trivially-ready for in-process embedders; network providers override.
    async fn ready(&self) -> Result<ProviderStatus> {
        Ok(ProviderStatus::ok(0))
    }

    /// Declared model identity for vectors this embedder produces (MIG-004).
    /// `None` means unspecified — vectors inherit the store's active
    /// generation. A claimed identity incompatible with the active generation
    /// fails the write rather than silently mixing.
    fn identity(&self) -> Option<nomiso_core::EmbeddingIdentity> {
        None
    }
}

/// High-level client used by HTTP, MCP, and embedders.
#[derive(Clone)]
pub struct NomisoClient {
    store: Arc<dyn MemoryStore>,
    embedder: Option<Arc<dyn Embedder>>,
}

impl NomisoClient {
    /// Wrap an existing store.
    pub fn new(store: Arc<dyn MemoryStore>) -> Self {
        Self {
            store,
            embedder: None,
        }
    }

    /// Connect to SurrealDB using config and run migrate.
    pub async fn connect(config: StoreConfig) -> Result<Self> {
        let store = SurrealMemoryStore::connect(config).await?;
        store.migrate().await?;
        Ok(Self::new(Arc::new(store)))
    }

    /// Attach an optional embedder for write-time vectors when request has none.
    pub fn with_embedder(mut self, embedder: Arc<dyn Embedder>) -> Self {
        self.embedder = Some(embedder);
        self
    }

    /// Borrow the attached embedder, if any (e.g. for a `reindex` worker).
    pub fn embedder(&self) -> Option<&Arc<dyn Embedder>> {
        self.embedder.as_ref()
    }

    /// Underlying store.
    pub fn store(&self) -> &Arc<dyn MemoryStore> {
        &self.store
    }

    /// Health check.
    pub async fn health(&self) -> Result<()> {
        self.store.health().await
    }

    /// Per-table row counts — the declared consistent frontier (OPS-004).
    pub async fn table_counts(&self) -> Result<std::collections::BTreeMap<String, u64>> {
        self.store.table_counts().await
    }

    /// OPS-001: probe the configured embedding provider's readiness.
    /// `Ok(None)` = no provider configured (baseline ops unaffected);
    /// `Ok(Some(status))` = inspectable probe result — `ready: false` with
    /// safe detail when the provider is down, never an `Err` for a down probe.
    pub async fn provider_status(&self) -> Result<Option<ProviderStatus>> {
        match &self.embedder {
            None => Ok(None),
            Some(e) => e.ready().await.map(Some),
        }
    }

    /// Embed exactly one text; enforces the one-vector-per-text contract.
    async fn embed_one(&self, text: String) -> Result<Option<Vec<f32>>> {
        let Some(embedder) = &self.embedder else {
            return Ok(None);
        };
        let mut vectors = embedder.embed(&[text]).await?;
        if vectors.len() != 1 {
            return Err(nomiso_core::Error::InvalidProviderResponse(
                "embedder returned an unexpected vector count".into(),
            ));
        }
        Ok(vectors.pop())
    }

    /// Put with optional auto-embed.
    #[instrument(skip(self, req))]
    pub async fn put(&self, mut req: PutRequest) -> Result<WriteResult> {
        let scope = nomiso_core::validate::validate_put_shape(&req, &self.store.limits())?;
        req.scope = scope.as_str().to_string();
        // Attach the embedder's claimed identity BEFORE the receipt lookup so
        // a replaying process computes the same request_identity as the
        // original writer — the lookup must stay before embed_one (no provider
        // call on replay), but identity() is sync and free.
        if req.embedding.is_none() && req.embedding_identity.is_none() {
            req.embedding_identity = self.embedder.as_ref().and_then(|e| e.identity());
        }
        if let Some(receipt) = self.store.lookup_put_receipt(&req).await? {
            return Ok(receipt);
        }
        nomiso_core::validate::validate_put(&req, &self.store.limits())?;
        let generated = if req.embedding.is_none() {
            self.embed_one(req.content.text.clone()).await?
        } else {
            None
        };
        self.store.put_prepared(req, generated).await
    }

    /// Atomic write + durable job intents (JOB-001). Same embed/identity
    /// handling as [`put`](Self::put); the receipt lookup runs before any
    /// provider call so replays never re-embed.
    #[instrument(skip(self, req, intents))]
    pub async fn put_with_jobs(
        &self,
        mut req: PutRequest,
        intents: Vec<nomiso_core::JobIntent>,
    ) -> Result<nomiso_core::WriteWithJobsResult> {
        let scope = nomiso_core::validate::validate_put_shape(&req, &self.store.limits())?;
        req.scope = scope.as_str().to_string();
        if req.embedding.is_none() && req.embedding_identity.is_none() {
            req.embedding_identity = self.embedder.as_ref().and_then(|e| e.identity());
        }
        if let Some(hit) = self
            .store
            .lookup_put_receipt_with_intents(&req, &intents)
            .await?
        {
            return Ok(hit);
        }
        nomiso_core::validate::validate_put(&req, &self.store.limits())?;
        let generated = if req.embedding.is_none() {
            self.embed_one(req.content.text.clone()).await?
        } else {
            None
        };
        self.store.put_with_jobs(req, generated, intents).await
    }

    /// Atomic supersede + durable job intents (JOB-001) with the same
    /// auto-embed handling as [`supersede`](Self::supersede).
    #[instrument(skip(self, req, intents))]
    pub async fn supersede_with_jobs(
        &self,
        mut req: SupersedeRequest,
        intents: Vec<nomiso_core::JobIntent>,
    ) -> Result<nomiso_core::WriteWithJobsResult> {
        let scope = nomiso_core::validate::validate_supersede(&req, &self.store.limits())?;
        req.new.scope = scope.as_str().to_string();
        if req.new.embedding.is_none() {
            req.new.embedding = self.embed_one(req.new.content.text.clone()).await?;
            if req.new.embedding.is_some() && req.new.embedding_identity.is_none() {
                req.new.embedding_identity = self.embedder.as_ref().and_then(|e| e.identity());
            }
        }
        self.store.supersede_with_jobs(req, intents).await
    }

    /// Supersede with optional auto-embed on new body.
    #[instrument(skip(self, req))]
    pub async fn supersede(&self, mut req: SupersedeRequest) -> Result<WriteResult> {
        let scope = nomiso_core::validate::validate_supersede(&req, &self.store.limits())?;
        req.new.scope = scope.as_str().to_string();
        if req.new.embedding.is_none() {
            req.new.embedding = self.embed_one(req.new.content.text.clone()).await?;
            if req.new.embedding.is_some() && req.new.embedding_identity.is_none() {
                req.new.embedding_identity = self.embedder.as_ref().and_then(|e| e.identity());
            }
        }
        self.store.supersede(req).await
    }

    /// Hybrid search with optional read-path auto-embed.
    ///
    /// When an embedder is attached and the query has text but no embedding,
    /// the client embeds the query text so hybrid/vector paths can run.
    #[instrument(skip(self, query))]
    pub async fn search(&self, mut query: SearchQuery) -> Result<Vec<SearchHit>> {
        let scope = nomiso_core::validate::validate_search(&query, &self.store.limits())?;
        query.scope = scope.as_str().to_string();
        if query.embedding.is_none() && !query.query.trim().is_empty() {
            query.embedding = self.embed_one(query.query.clone()).await?;
        }
        self.store.search(query).await
    }

    /// Search plus retrieval-cost reporting (expansion stats when
    /// `query.graph_expand` is set). Same auto-embed handling as
    /// [`Self::search`].
    #[instrument(skip(self, query))]
    pub async fn search_detailed(
        &self,
        mut query: SearchQuery,
    ) -> Result<nomiso_core::ops::SearchOutcome> {
        let scope = nomiso_core::validate::validate_search(&query, &self.store.limits())?;
        query.scope = scope.as_str().to_string();
        if query.embedding.is_none() && !query.query.trim().is_empty() {
            query.embedding = self.embed_one(query.query.clone()).await?;
        }
        self.store.search_detailed(query).await
    }

    /// Lexical-only search: never calls the attached embedder.
    ///
    /// A supplied embedding is rejected; this opt-in path preserves BM25-only
    /// semantics (use [`Self::search`] for auto-embed hybrid).
    #[instrument(skip(self, query))]
    pub async fn search_lexical(&self, mut query: SearchQuery) -> Result<Vec<SearchHit>> {
        if query.embedding.is_some() {
            return Err(nomiso_core::Error::invalid(
                "search_lexical does not accept a supplied embedding",
            ));
        }
        let scope = nomiso_core::validate::validate_search(&query, &self.store.limits())?;
        query.scope = scope.as_str().to_string();
        self.store.search(query).await
    }

    /// Open-validity candidate priors (auto-embeds like [`Self::search`]).
    pub async fn find_candidates(&self, req: FindCandidatesRequest) -> Result<Vec<SearchHit>> {
        self.search(req.into_search()).await
    }

    /// Read by ids.
    pub async fn read(&self, req: ReadRequest) -> Result<Vec<MemoryRecord>> {
        self.store.read(req).await
    }

    /// Soft or hard forget.
    pub async fn forget(&self, req: ForgetRequest) -> Result<()> {
        self.store.forget(req).await
    }

    /// Append a trace event (Phase 2.5).
    pub async fn append_trace_event(
        &self,
        req: nomiso_core::trace::AppendTraceEvent,
    ) -> Result<nomiso_core::trace::TraceEventRecord> {
        self.store.append_trace_event(req).await
    }

    /// Load a trace bundle by id + scope.
    pub async fn get_trace(
        &self,
        trace_id: &str,
        scope: &str,
    ) -> Result<nomiso_core::trace::TraceBundle> {
        self.store.get_trace(trace_id, scope).await
    }

    /// List parent traces under a scope.
    pub async fn list_traces(
        &self,
        req: nomiso_core::trace::ListTracesRequest,
    ) -> Result<Vec<nomiso_core::trace::TraceSummary>> {
        self.store.list_traces(req).await
    }

    /// List traces that referenced a memory id (inject/search/write), plus outcome.
    pub async fn list_traces_for_memory(
        &self,
        req: nomiso_core::trace::TracesByMemoryRequest,
    ) -> Result<Vec<nomiso_core::trace::TraceByMemory>> {
        self.store.list_traces_for_memory(req).await
    }

    /// List memories (Phase 3a).
    pub async fn list(
        &self,
        req: nomiso_core::list::ListRequest,
    ) -> Result<nomiso_core::list::ListResponse> {
        self.store.list(req).await
    }

    /// Count memories matching filters.
    pub async fn count(&self, req: nomiso_core::list::CountRequest) -> Result<u64> {
        self.store.count(req).await
    }

    /// Register artifact metadata (bytes via BlobStore).
    pub async fn put_artifact(
        &self,
        req: nomiso_core::evidence::PutArtifactRequest,
    ) -> Result<nomiso_core::evidence::ArtifactRecord> {
        self.store.put_artifact(req).await
    }

    /// List artifact records under an exact scope (`None` = all scopes).
    pub async fn list_artifacts(
        &self,
        scope: Option<&str>,
    ) -> Result<Vec<nomiso_core::evidence::ArtifactRecord>> {
        self.store.list_artifacts(scope).await
    }

    /// Atomically rebind an artifact's blob location, guarded on the
    /// expected current value (OPS-002). `Ok(false)` = guard failed.
    pub async fn rebind_artifact_location(
        &self,
        id: &str,
        expected_location: &str,
        new_location: &str,
    ) -> Result<bool> {
        self.store
            .rebind_artifact_location(id, expected_location, new_location)
            .await
    }

    pub async fn put_span(
        &self,
        req: nomiso_core::evidence::PutSpanRequest,
    ) -> Result<nomiso_core::evidence::SpanRecord> {
        self.store.put_span(req).await
    }

    pub async fn link_derived_from(
        &self,
        req: nomiso_core::evidence::LinkDerivedFromRequest,
    ) -> Result<()> {
        self.store.link_derived_from(req).await
    }

    /// Create a scoped entity/resource record.
    pub async fn put_entity(
        &self,
        req: nomiso_core::relationship::PutEntityRequest,
    ) -> Result<nomiso_core::relationship::EntityRecord> {
        self.store.put_entity(req).await
    }

    /// Read an entity by id (scope-checked).
    pub async fn get_entity(
        &self,
        id: &str,
        scope: &str,
    ) -> Result<nomiso_core::relationship::EntityRecord> {
        self.store.get_entity(id, scope).await
    }

    /// Versioned entity update (CAS on expected_version).
    pub async fn update_entity(
        &self,
        req: nomiso_core::relationship::UpdateEntityRequest,
    ) -> Result<nomiso_core::relationship::EntityRecord> {
        self.store.update_entity(req).await
    }

    /// Create a typed relationship with registry/scope/endpoint validation.
    pub async fn put_relationship(
        &self,
        req: nomiso_core::relationship::PutRelationshipRequest,
    ) -> Result<nomiso_core::relationship::RelationshipWrite> {
        self.store.put_relationship(req).await
    }

    /// Read a relationship by id (scope-checked).
    pub async fn get_relationship(
        &self,
        id: &str,
        scope: &str,
    ) -> Result<nomiso_core::relationship::RelationshipRecord> {
        self.store.get_relationship(id, scope).await
    }

    /// Versioned relationship update (CAS); prior state is audited.
    pub async fn update_relationship(
        &self,
        req: nomiso_core::relationship::UpdateRelationshipRequest,
    ) -> Result<nomiso_core::relationship::RelationshipRecord> {
        self.store.update_relationship(req).await
    }

    /// List relationships under a scope.
    pub async fn list_relationships(
        &self,
        req: nomiso_core::relationship::ListRelationshipsRequest,
    ) -> Result<Vec<nomiso_core::relationship::RelationshipRecord>> {
        self.store.list_relationships(req).await
    }

    /// Bounded same-scope traversal with provenance and truncation reporting.
    pub async fn traverse(
        &self,
        req: nomiso_core::relationship::TraverseRequest,
    ) -> Result<nomiso_core::relationship::TraverseResult> {
        self.store.traverse(req).await
    }

    /// Recorded embedding generations and the active one (MIG-004/005).
    pub async fn embedding_state(&self) -> Result<nomiso_core::EmbeddingState> {
        self.store.embedding_state().await
    }

    /// Attest the active generation's unknown identity (legacy path).
    pub async fn attest_embedding_identity(
        &self,
        identity: nomiso_core::EmbeddingIdentity,
    ) -> Result<nomiso_core::EmbeddingGeneration> {
        self.store.attest_embedding_identity(identity).await
    }

    /// Declare a new staged generation built from a captured frontier.
    pub async fn declare_embedding_generation(
        &self,
        req: nomiso_core::DeclareGenerationRequest,
    ) -> Result<nomiso_core::EmbeddingGeneration> {
        self.store.declare_embedding_generation(req).await
    }

    /// Stage re-embedded vectors for a building generation (idempotent).
    pub async fn stage_embeddings(
        &self,
        generation: u64,
        items: Vec<nomiso_core::StagedEmbedding>,
    ) -> Result<u64> {
        self.store.stage_embeddings(generation, items).await
    }

    /// Validate coverage, then atomically activate a generation.
    pub async fn activate_embedding_generation(
        &self,
        generation: u64,
    ) -> Result<nomiso_core::EmbeddingGeneration> {
        self.store.activate_embedding_generation(generation).await
    }

    // ---- Durable job journal (JOB-001..007) ----

    /// Durably enqueue a job intent; identical live intents deduplicate.
    pub async fn enqueue_job(
        &self,
        req: nomiso_core::EnqueueJobRequest,
    ) -> Result<nomiso_core::EnqueueJobResult> {
        self.store.enqueue_job(req).await
    }

    /// Fetch a job record by id within its exact scope.
    pub async fn get_job(&self, scope: &str, id: &str) -> Result<nomiso_core::JobRecord> {
        self.store.get_job(scope, id).await
    }

    /// Lifecycle inspection without raw payload bodies (JOB-007).
    pub async fn list_jobs(
        &self,
        req: nomiso_core::ListJobsRequest,
    ) -> Result<Vec<nomiso_core::JobSummary>> {
        self.store.list_jobs(req).await
    }

    /// Acquire a fenced lease within a worker's scope/kind grant (JOB-003).
    pub async fn claim_job(
        &self,
        req: nomiso_core::ClaimJobRequest,
    ) -> Result<Option<nomiso_core::JobLease>> {
        self.store.claim_job(req).await
    }

    /// Extend the active lease under its fence.
    pub async fn renew_job_lease(
        &self,
        id: &str,
        fence: u64,
        worker: &str,
    ) -> Result<nomiso_core::JobLease> {
        self.store.renew_job_lease(id, fence, worker).await
    }

    /// Persist a bounded checkpoint under the active fence.
    pub async fn checkpoint_job(
        &self,
        id: &str,
        fence: u64,
        worker: &str,
        checkpoint: serde_json::Value,
    ) -> Result<nomiso_core::JobRecord> {
        self.store
            .checkpoint_job(id, fence, worker, checkpoint)
            .await
    }

    /// Commit final effects; pinned inputs revalidate in the same transaction.
    pub async fn complete_job(
        &self,
        id: &str,
        fence: u64,
        worker: &str,
        result: serde_json::Value,
    ) -> Result<nomiso_core::JobRecord> {
        self.store.complete_job(id, fence, worker, result).await
    }

    /// Record a failure; retryable failures within budget return to pending.
    pub async fn fail_job(
        &self,
        id: &str,
        fence: u64,
        worker: &str,
        error: nomiso_core::JobError,
    ) -> Result<nomiso_core::JobRecord> {
        self.store.fail_job(id, fence, worker, error).await
    }

    /// Authorized cancellation; committed checkpoint remains visible.
    pub async fn cancel_job(
        &self,
        scope: &str,
        id: &str,
        reason: &str,
    ) -> Result<nomiso_core::JobRecord> {
        self.store.cancel_job(scope, id, reason).await
    }

    /// Replace a pending/leased job with a newer desired job.
    pub async fn supersede_job(
        &self,
        scope: &str,
        id: &str,
        replacement_id: &str,
        reason: &str,
    ) -> Result<nomiso_core::JobRecord> {
        self.store
            .supersede_job(scope, id, replacement_id, reason)
            .await
    }

    pub async fn history(
        &self,
        id: &nomiso_core::types::MemoryId,
        scope: &str,
    ) -> Result<Vec<nomiso_core::evidence::HistoryEntry>> {
        self.store.history(id, scope).await
    }

    pub async fn annotate(
        &self,
        req: nomiso_core::evidence::AnnotateRequest,
    ) -> Result<WriteResult> {
        self.store.annotate(req).await
    }

    pub async fn put_task_state(
        &self,
        req: nomiso_core::task_state::PutTaskStateRequest,
    ) -> Result<nomiso_core::task_state::TaskStateRecord> {
        self.store.put_task_state(req).await
    }

    pub async fn get_task_state(
        &self,
        req: nomiso_core::task_state::GetTaskStateRequest,
    ) -> Result<Option<nomiso_core::task_state::TaskStateRecord>> {
        self.store.get_task_state(req).await
    }

    pub async fn list_belief_events(
        &self,
        memory_id: &nomiso_core::types::MemoryId,
        scope: &str,
    ) -> Result<Vec<nomiso_core::belief_event::BeliefEvent>> {
        self.store.list_belief_events(memory_id, scope).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nomiso_core::list::CountRequest;
    use nomiso_core::types::{Category, Content, Provenance};
    use nomiso_store::StoreConfig;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn client_put_search() {
        let client = NomisoClient::connect(StoreConfig::memory_test(8))
            .await
            .unwrap();
        client
            .put(PutRequest {
                scope: "org/demo".into(),
                category: Category::Semantic,
                content: Content::text("Nomiso stores agent memory"),
                valid_from: None,
                valid_until: None,
                known_at: None,
                confidence: None,
                provenance: Provenance::default(),
                entity_links: vec![],
                embedding: None,
                embedding_identity: None,
                idempotency_key: None,
                extractor_version: None,
                model_version: None,
                valid_rev_from: None,
                valid_rev_until: None,
            })
            .await
            .unwrap();
        let hits = client
            .search(SearchQuery {
                query: "agent memory".into(),
                scope: "org/demo".into(),
                scope_match: Default::default(),
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
                categories: None,
                limit: Some(5),
                embedding: None,
                graph_enrich: Some(false),
                graph_expand: None,
            })
            .await
            .unwrap();
        assert!(!hits.is_empty());
        assert!(hits[0].preview.contains("Nomiso"));
    }

    fn base_put(scope: &str, text: &str) -> PutRequest {
        PutRequest {
            scope: scope.into(),
            category: Category::Semantic,
            content: Content::text(text),
            valid_from: None,
            valid_until: None,
            known_at: None,
            confidence: None,
            provenance: Provenance::default(),
            entity_links: vec![],
            embedding: None,
            embedding_identity: None,
            idempotency_key: None,
            extractor_version: None,
            model_version: None,
            valid_rev_from: None,
            valid_rev_until: None,
        }
    }

    fn base_search(query: &str) -> SearchQuery {
        SearchQuery {
            query: query.into(),
            scope: "org/demo".into(),
            scope_match: Default::default(),
            as_of: None,
            known_as_of: None,
            sys_as_of: None,
            categories: None,
            limit: Some(5),
            embedding: None,
            graph_enrich: Some(false),
            graph_expand: None,
        }
    }

    async fn scope_count(client: &NomisoClient, scope: &str) -> u64 {
        client
            .count(CountRequest {
                scope: scope.into(),
                scope_match: Default::default(),
                categories: None,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
                text: None,
            })
            .await
            .unwrap()
    }

    /// Stub embedder that counts provider calls and returns a configurable
    /// number of fixed vectors per call.
    struct CountingEmbedder {
        calls: AtomicUsize,
        dim: usize,
        vectors_per_call: usize,
    }

    #[async_trait]
    impl Embedder for CountingEmbedder {
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok((0..self.vectors_per_call * texts.len())
                .map(|_| vec![0.0; self.dim])
                .collect())
        }
    }

    /// Embedder whose provider call always fails.
    struct FailingEmbedder(AtomicUsize);

    #[async_trait]
    impl Embedder for FailingEmbedder {
        async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(nomiso_core::Error::store("stub provider failure"))
        }
    }

    #[tokio::test]
    async fn put_and_search_validate_before_calling_provider() {
        let counter = Arc::new(CountingEmbedder {
            calls: AtomicUsize::new(0),
            dim: 8,
            vectors_per_call: 1,
        });
        let client = NomisoClient::connect(StoreConfig::memory_test(8))
            .await
            .unwrap()
            .with_embedder(counter.clone());
        let mut bad_scope = base_put("org/demo", "valid text");
        bad_scope.scope = "bad scope!".into();
        assert!(client.put(bad_scope).await.is_err(), "invalid scope");
        assert!(
            client.put(base_put("org/demo", "   ")).await.is_err(),
            "empty text"
        );
        let big = "x".repeat(9000);
        assert!(
            client.put(base_put("org/demo", &big)).await.is_err(),
            "oversize text"
        );
        let mut conf = base_put("org/demo", "conf out of range");
        conf.confidence = Some(1.5);
        assert!(client.put(conf).await.is_err(), "confidence > 1");
        let mut wrong_dim = base_put("org/demo", "explicit bad vector");
        wrong_dim.embedding = Some(vec![0.1, 0.2]);
        assert!(client.put(wrong_dim).await.is_err(), "wrong-dim vector");
        let mut over_limit = base_search("anything token");
        over_limit.limit = Some(999);
        assert!(
            client.search(over_limit).await.is_err(),
            "search over limit"
        );
        let mut bad_vec = base_search("anything token");
        bad_vec.embedding = Some(vec![1.0, 2.0]);
        assert!(client.search(bad_vec).await.is_err(), "bad search vector");
        let mut empty_vec = base_search("anything token");
        empty_vec.embedding = Some(vec![]);
        assert!(
            client.search(empty_vec).await.is_err(),
            "supplied empty vector must be invalid, not silently re-embedded"
        );
        assert_eq!(
            counter.calls.load(Ordering::SeqCst),
            0,
            "provider must never run for invalid input"
        );
        assert_eq!(scope_count(&client, "org/demo").await, 0);
    }

    #[tokio::test]
    async fn embedder_wrong_vector_count_fails_without_storing() {
        for vectors_per_call in [0usize, 2] {
            let counter = Arc::new(CountingEmbedder {
                calls: AtomicUsize::new(0),
                dim: 8,
                vectors_per_call,
            });
            let client = NomisoClient::connect(StoreConfig::memory_test(8))
                .await
                .unwrap()
                .with_embedder(counter.clone());
            let err = client
                .put(base_put("org/demo", "needs a vector"))
                .await
                .expect_err("wrong vector count must fail the write");
            assert!(
                matches!(err, nomiso_core::Error::InvalidProviderResponse(_)),
                "vectors_per_call={vectors_per_call}: {err:?}"
            );
            assert_eq!(counter.calls.load(Ordering::SeqCst), 1);
            assert_eq!(
                scope_count(&client, "org/demo").await,
                0,
                "nothing may be stored when the embedder misbehaves"
            );
        }
    }

    #[tokio::test]
    async fn keyed_put_replay_does_not_need_provider() {
        let client = NomisoClient::connect(StoreConfig::memory_test(8))
            .await
            .unwrap();
        let mut req = base_put("org/replay", "reusable fact");
        req.idempotency_key = Some("key-1".into());
        let first = client
            .clone()
            .with_embedder(Arc::new(FixedEmbedder(vec![1.0; 8])))
            .put(req.clone())
            .await
            .unwrap();
        let failing = Arc::new(FailingEmbedder(AtomicUsize::new(0)));
        let client = client.with_embedder(failing.clone());
        let replay = client.put(req.clone()).await.unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.id, first.id);
        assert_eq!(replay.record, first.record);
        req.content.text = "changed fact".into();
        assert!(matches!(
            client.put(req).await,
            Err(nomiso_core::Error::IdempotencyConflict)
        ));
        assert_eq!(failing.0.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn keyed_put_replay_preserves_expired_default_time_intent() {
        let client = NomisoClient::connect(StoreConfig::memory_test(8))
            .await
            .unwrap();
        let mut req = base_put("org/expiry", "expiring fact");
        req.idempotency_key = Some("expires".into());
        req.valid_until = Some(
            nomiso_core::Timestamp::from_second(nomiso_core::Timestamp::now().as_second() + 3)
                .unwrap(),
        );
        let first = client.put(req.clone()).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(4)).await;
        let replay = client.put(req).await.unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.record, first.record);
        assert_eq!(scope_count(&client, "org/expiry").await, 0);
    }

    #[tokio::test]
    async fn search_lexical_never_calls_provider() {
        let failing = Arc::new(FailingEmbedder(AtomicUsize::new(0)));
        let client = NomisoClient::connect(StoreConfig::memory_test(8))
            .await
            .unwrap()
            .with_embedder(failing.clone());
        // Seed through the store so the failing provider is never needed.
        client
            .store()
            .put(base_put("org/demo", "lexical needle phrase unique-q7"))
            .await
            .unwrap();
        let hits = client
            .search_lexical(base_search("needle unique-q7"))
            .await
            .unwrap();
        assert!(!hits.is_empty(), "lexical hits expected");
        assert!(
            hits.iter().all(|h| !h.signals.vector),
            "lexical path must not report vector signal: {hits:?}"
        );
        assert_eq!(
            failing.0.load(Ordering::SeqCst),
            0,
            "search_lexical must not call the embedder"
        );
        let mut q = base_search("needle unique-q7");
        q.embedding = Some(vec![0.0; 8]);
        assert!(
            client.search_lexical(q).await.is_err(),
            "supplied embedding must be rejected on the lexical path"
        );
    }

    /// Local stub embedder (avoids dual nomiso-service versions via nomiso-embed).
    struct FixedEmbedder(Vec<f32>);

    #[async_trait]
    impl Embedder for FixedEmbedder {
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| self.0.clone()).collect())
        }
    }

    /// Read-path auto-embed: attached embedder + embedding None must engage the **vector** path.
    ///
    /// Control: same put+search **without** embedder yields BM25-only (vector flag false).
    /// Treatment: with embedder, hits must report `signals.vector == true` (not merely bm25).
    #[tokio::test]
    async fn search_auto_embeds_when_embedder_attached() {
        let dim = 8usize;
        let vec = vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let text = "vector auto embed probe about quantum barbecue xyzzy99";

        // --- control: no embedder → text-only path, no vector signal ---
        let bare = NomisoClient::connect(StoreConfig::memory_test(dim))
            .await
            .unwrap();
        bare.put(PutRequest {
            scope: "org/embed-ctrl".into(),
            category: Category::Semantic,
            content: Content::text(text),
            valid_from: None,
            valid_until: None,
            known_at: None,
            confidence: None,
            provenance: Provenance::default(),
            entity_links: vec![],
            embedding: None,
            embedding_identity: None,
            idempotency_key: None,
            extractor_version: None,
            model_version: None,
            valid_rev_from: None,
            valid_rev_until: None,
        })
        .await
        .unwrap();
        let ctrl = bare
            .search(SearchQuery {
                query: "quantum barbecue xyzzy99".into(),
                scope: "org/embed-ctrl".into(),
                scope_match: Default::default(),
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
                categories: None,
                limit: Some(5),
                embedding: None,
                graph_enrich: Some(false),
                graph_expand: None,
            })
            .await
            .unwrap();
        assert!(!ctrl.is_empty(), "control BM25 search should hit");
        assert!(
            ctrl.iter().all(|h| !h.signals.vector),
            "without embedder, vector channel must stay false: {ctrl:?}"
        );
        assert!(
            ctrl.iter().any(|h| h.signals.bm25),
            "control should use BM25: {ctrl:?}"
        );

        // --- treatment: embedder attached → read path auto-embeds → vector (or hybrid) ---
        let emb = Arc::new(FixedEmbedder(vec.clone()));
        let client = NomisoClient::connect(StoreConfig::memory_test(dim))
            .await
            .unwrap()
            .with_embedder(emb);
        client
            .put(PutRequest {
                scope: "org/embed".into(),
                category: Category::Semantic,
                content: Content::text(text),
                valid_from: None,
                valid_until: None,
                known_at: None,
                confidence: None,
                provenance: Provenance::default(),
                entity_links: vec![],
                embedding: None, // write path auto-embeds via FixedEmbedder
                embedding_identity: None,
                idempotency_key: None,
                extractor_version: None,
                model_version: None,
                valid_rev_from: None,
                valid_rev_until: None,
            })
            .await
            .unwrap();
        let hits = client
            .search(SearchQuery {
                query: "quantum barbecue xyzzy99".into(),
                scope: "org/embed".into(),
                scope_match: Default::default(),
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
                categories: None,
                limit: Some(5),
                embedding: None, // read path MUST auto-embed
                graph_enrich: Some(false),
                graph_expand: None,
            })
            .await
            .unwrap();
        assert!(!hits.is_empty(), "auto-embed search should return hits");
        assert!(
            hits.iter().any(|h| h.signals.vector),
            "auto-embed must engage vector channel (signals.vector==true); got {hits:?}"
        );
    }
}
