//! MCP stdio tools for Nomiso (`nomiso.op.v1` parity with HTTP + Phase 3e/3.5 surfaces).

#![forbid(unsafe_code)]

use nomiso_core::belief_event::ListBeliefEventsRequest;
use nomiso_core::list::{CountRequest, ListRequest};
use nomiso_core::ops::{ForgetRequest, PutRequest, ReadRequest, SearchQuery, SupersedeRequest};
use nomiso_core::task_state::{GetTaskStateRequest, PutTaskStateRequest};
use nomiso_service::NomisoClient;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ServerHandler};
use serde::Serialize;

/// MCP server holding a Nomiso client.
#[derive(Clone)]
pub struct NomisoMcp {
    client: NomisoClient,
    tool_router: ToolRouter<Self>,
}

impl NomisoMcp {
    /// Create MCP server from client.
    pub fn new(client: NomisoClient) -> Self {
        Self {
            client,
            tool_router: Self::tool_router(),
        }
    }

    /// Borrow client.
    pub fn client(&self) -> &NomisoClient {
        &self.client
    }
}

fn json_text(v: &impl Serialize) -> CallToolResult {
    match serde_json::to_value(v) {
        Ok(value) => {
            if value.is_object() {
                CallToolResult::structured(value)
            } else {
                CallToolResult::success(vec![ContentBlock::text(value.to_string())])
            }
        }
        Err(e) => CallToolResult::structured_error(serde_json::json!({
            "error": format!("serialize response: {e}"),
            "code": "internal_error",
        })),
    }
}

fn err_text(e: nomiso_core::Error) -> CallToolResult {
    CallToolResult::structured_error(serde_json::json!({
        "error": e.public_message(),
        "code": e.code(),
    }))
}

#[tool_router(router = tool_router)]
impl NomisoMcp {
    /// Hybrid memory search (supports as_of / known_as_of / sys_as_of temporal lenses).
    #[tool(
        name = "nomiso_search",
        description = "Hybrid recall (BM25 + optional vector RRF) under a scope. Body matches nomiso.op.v1 SearchQuery including as_of, known_as_of, sys_as_of."
    )]
    async fn nomiso_search(&self, Parameters(args): Parameters<SearchQuery>) -> CallToolResult {
        match self.client.search(args).await {
            Ok(hits) => json_text(&hits),
            Err(e) => err_text(e),
        }
    }

    /// Hybrid memory search with retrieval-cost reporting (expansion stats when
    /// `graph_expand` is set on the query).
    #[tool(
        name = "nomiso_search_detailed",
        description = "Hybrid recall returning hits plus SearchStats (graph expansion cost/truncation when graph_expand is set). Body matches nomiso.op.v1 SearchQuery."
    )]
    async fn nomiso_search_detailed(
        &self,
        Parameters(args): Parameters<SearchQuery>,
    ) -> CallToolResult {
        match self.client.search_detailed(args).await {
            Ok(outcome) => json_text(&outcome),
            Err(e) => err_text(e),
        }
    }

    /// Read full memory records by id (temporal lenses optional).
    #[tool(
        name = "nomiso_read",
        description = "Read full memory records by id with scope checks. Body matches nomiso.op.v1 ReadRequest (as_of/known_as_of/sys_as_of optional)."
    )]
    async fn nomiso_read(&self, Parameters(args): Parameters<ReadRequest>) -> CallToolResult {
        match self.client.read(args).await {
            Ok(rows) => json_text(&rows),
            Err(e) => err_text(e),
        }
    }

    /// Insert structured memory.
    #[tool(
        name = "nomiso_put",
        description = "Insert a structured memory fact. Body matches nomiso.op.v1 PutRequest."
    )]
    async fn nomiso_put(&self, Parameters(args): Parameters<PutRequest>) -> CallToolResult {
        match self.client.put(args).await {
            Ok(w) => json_text(&w),
            Err(e) => err_text(e),
        }
    }

    /// Atomic put + durable job intents (JOB-001 commit-together).
    #[tool(
        name = "nomiso_put_with_jobs",
        description = "Commit a memory put plus durable job intents in ONE transaction (JOB-001). Intents inherit the write scope; self_input pins the created record at rev 1. A bad intent aborts the write; a live identical intent dedups."
    )]
    async fn nomiso_put_with_jobs(
        &self,
        Parameters(args): Parameters<PutWithJobsArgs>,
    ) -> CallToolResult {
        match self.client.put_with_jobs(args.put, args.jobs).await {
            Ok(w) => json_text(&w),
            Err(e) => err_text(e),
        }
    }

    /// Atomic supersede + durable job intents (JOB-001 commit-together).
    #[tool(
        name = "nomiso_supersede_with_jobs",
        description = "Commit a supersede plus durable job intents in ONE transaction (JOB-001). Intents inherit the write scope; self_input pins the successor at rev 1. A bad intent leaves the prior open; a live identical intent dedups."
    )]
    async fn nomiso_supersede_with_jobs(
        &self,
        Parameters(args): Parameters<SupersedeWithJobsArgs>,
    ) -> CallToolResult {
        match self
            .client
            .supersede_with_jobs(args.supersede, args.jobs)
            .await
        {
            Ok(w) => json_text(&w),
            Err(e) => err_text(e),
        }
    }

    /// Supersede a prior fact.
    #[tool(
        name = "nomiso_supersede",
        description = "Assert a new fact and close prior by prior_id + expected_version (one-shot close)."
    )]
    async fn nomiso_supersede(
        &self,
        Parameters(args): Parameters<SupersedeRequest>,
    ) -> CallToolResult {
        match self.client.supersede(args).await {
            Ok(w) => json_text(&w),
            Err(e) => err_text(e),
        }
    }

    /// Soft-expire or hard-delete memory.
    #[tool(
        name = "nomiso_forget",
        description = "Soft-expire (default; one-shot close) or hard-delete a memory by id."
    )]
    async fn nomiso_forget(&self, Parameters(args): Parameters<ForgetRequest>) -> CallToolResult {
        match self.client.forget(args).await {
            Ok(()) => json_text(&serde_json::json!({ "status": "forgotten" })),
            Err(e) => err_text(e),
        }
    }

    /// Enumerate memories under scope (list ≠ search).
    #[tool(
        name = "nomiso_list",
        description = "Enumerate memories under scope (optional text filter + temporal lenses). Not ranked hybrid search."
    )]
    async fn nomiso_list(&self, Parameters(args): Parameters<ListRequest>) -> CallToolResult {
        match self.client.list(args).await {
            Ok(page) => json_text(&page),
            Err(e) => err_text(e),
        }
    }

    /// Count memories under scope.
    #[tool(
        name = "nomiso_count",
        description = "Count memories matching list filters under a scope."
    )]
    async fn nomiso_count(&self, Parameters(args): Parameters<CountRequest>) -> CallToolResult {
        match self.client.count(args).await {
            Ok(n) => json_text(&serde_json::json!({ "count": n })),
            Err(e) => err_text(e),
        }
    }

    /// Put/replace a task_state working-memory slot (Phase 3.5).
    #[tool(
        name = "nomiso_put_task_state",
        description = "Put or replace a scoped working-state slot (task_state). Optimistic version when expected_version set."
    )]
    async fn nomiso_put_task_state(
        &self,
        Parameters(args): Parameters<PutTaskStateRequest>,
    ) -> CallToolResult {
        match self.client.put_task_state(args).await {
            Ok(rec) => json_text(&rec),
            Err(e) => err_text(e),
        }
    }

    /// Get a task_state working-memory slot.
    #[tool(
        name = "nomiso_get_task_state",
        description = "Get a scoped working-state slot (task_state) by scope + slot name."
    )]
    async fn nomiso_get_task_state(
        &self,
        Parameters(args): Parameters<GetTaskStateRequest>,
    ) -> CallToolResult {
        match self.client.get_task_state(args).await {
            Ok(rec) => json_text(&rec),
            Err(e) => err_text(e),
        }
    }

    /// List belief_event audit journal rows for a memory (Phase 3e).
    #[tool(
        name = "nomiso_belief_events",
        description = "List append-only belief_event audit journal for a memory id (distinct from trace_event use flywheel). Best-effort durability after mutations."
    )]
    async fn nomiso_belief_events(
        &self,
        Parameters(args): Parameters<ListBeliefEventsRequest>,
    ) -> CallToolResult {
        match self
            .client
            .list_belief_events(&args.memory_id, &args.scope)
            .await
        {
            Ok(ev) => json_text(&ev),
            Err(e) => err_text(e),
        }
    }

    // --- Entities ---

    /// Create a scoped, versioned entity record (REL-001).
    #[tool(
        name = "nomiso_put_entity",
        description = "Create a scoped entity record (kind, name, aliases, attrs). Returns versioned EntityRecord."
    )]
    async fn nomiso_put_entity(
        &self,
        Parameters(args): Parameters<nomiso_core::PutEntityRequest>,
    ) -> CallToolResult {
        match self.client.put_entity(args).await {
            Ok(r) => json_text(&r),
            Err(e) => err_text(e),
        }
    }

    /// Get an entity by scope + id.
    #[tool(
        name = "nomiso_get_entity",
        description = "Get a scoped entity record by id."
    )]
    async fn nomiso_get_entity(&self, Parameters(args): Parameters<ScopedId>) -> CallToolResult {
        match self.client.get_entity(&args.id, &args.scope).await {
            Ok(r) => json_text(&r),
            Err(e) => err_text(e),
        }
    }

    /// CAS update of an entity (expected_version required).
    #[tool(
        name = "nomiso_update_entity",
        description = "CAS-update an entity record (expected_version guards against lost updates)."
    )]
    async fn nomiso_update_entity(
        &self,
        Parameters(args): Parameters<nomiso_core::UpdateEntityRequest>,
    ) -> CallToolResult {
        match self.client.update_entity(args).await {
            Ok(r) => json_text(&r),
            Err(e) => err_text(e),
        }
    }

    // --- Relationships ---

    /// Assert a typed relationship between same-scope endpoints (REL-001/002).
    #[tool(
        name = "nomiso_put_relationship",
        description = "Assert a typed relationship (closed predicate registry, same-scope endpoints, optional revision pins + evidence). Idempotent on identical intent."
    )]
    async fn nomiso_put_relationship(
        &self,
        Parameters(args): Parameters<nomiso_core::PutRelationshipRequest>,
    ) -> CallToolResult {
        match self.client.put_relationship(args).await {
            Ok(w) => json_text(&w),
            Err(e) => err_text(e),
        }
    }

    /// CAS-update a relationship (state/epistemic/validity).
    #[tool(
        name = "nomiso_update_relationship",
        description = "CAS-update a relationship: epistemic status, state transitions (closed/stale need reason), validity bounds."
    )]
    async fn nomiso_update_relationship(
        &self,
        Parameters(args): Parameters<nomiso_core::UpdateRelationshipRequest>,
    ) -> CallToolResult {
        match self.client.update_relationship(args).await {
            Ok(r) => json_text(&r),
            Err(e) => err_text(e),
        }
    }

    /// List relationships with endpoint/predicate/state filters.
    #[tool(
        name = "nomiso_list_relationships",
        description = "List relationships in a scope with optional endpoint, predicate, and state filters."
    )]
    async fn nomiso_list_relationships(
        &self,
        Parameters(args): Parameters<nomiso_core::ListRelationshipsRequest>,
    ) -> CallToolResult {
        match self.client.list_relationships(args).await {
            Ok(r) => json_text(&r),
            Err(e) => err_text(e),
        }
    }

    /// Bounded BFS over the relationship plane (RET-001 bounds).
    #[tool(
        name = "nomiso_traverse",
        description = "Bounded graph traversal from seed endpoints: depth/visited/edge/deadline budgets, direction + predicate + state filters, path provenance + truncation reasons."
    )]
    async fn nomiso_traverse(
        &self,
        Parameters(args): Parameters<nomiso_core::TraverseRequest>,
    ) -> CallToolResult {
        match self.client.traverse(args).await {
            Ok(r) => json_text(&r),
            Err(e) => err_text(e),
        }
    }

    // --- Embedding administration (MIG-004/005) ---

    /// Inspect embedding generations and coverage.
    #[tool(
        name = "nomiso_embedding_state",
        description = "Inspect all embedding generations: identity, status, frontier, expected/embedded counts."
    )]
    async fn nomiso_embedding_state(
        &self,
        Parameters(_args): Parameters<EmptyArgs>,
    ) -> CallToolResult {
        match self.client.embedding_state().await {
            Ok(s) => json_text(&s),
            Err(e) => err_text(e),
        }
    }

    /// Attest the active generation's unknown identity (legacy vectors).
    #[tool(
        name = "nomiso_attest_embedding_identity",
        description = "Attest the legacy (unknown-identity) active generation's model identity. Fails if identity already known."
    )]
    async fn nomiso_attest_embedding_identity(
        &self,
        Parameters(args): Parameters<nomiso_core::EmbeddingIdentity>,
    ) -> CallToolResult {
        match self.client.attest_embedding_identity(args).await {
            Ok(g) => json_text(&g),
            Err(e) => err_text(e),
        }
    }

    /// Declare a new staging generation with a producer identity.
    #[tool(
        name = "nomiso_declare_embedding_generation",
        description = "Declare a new embedding generation (staging): captures source frontier + expected coverage for staged reindex."
    )]
    async fn nomiso_declare_embedding_generation(
        &self,
        Parameters(args): Parameters<nomiso_core::DeclareGenerationRequest>,
    ) -> CallToolResult {
        match self.client.declare_embedding_generation(args).await {
            Ok(g) => json_text(&g),
            Err(e) => err_text(e),
        }
    }

    /// Stage vectors into a declared generation (idempotent per memory).
    #[tool(
        name = "nomiso_stage_embeddings",
        description = "Stage vectors into a declared generation; idempotent upserts per (generation, memory), dimension-checked."
    )]
    async fn nomiso_stage_embeddings(
        &self,
        Parameters(args): Parameters<StageArgs>,
    ) -> CallToolResult {
        match self
            .client
            .stage_embeddings(args.generation, args.items)
            .await
        {
            Ok(n) => json_text(&serde_json::json!({ "staged": n })),
            Err(e) => err_text(e),
        }
    }

    /// Activate a staged generation once coverage is complete.
    #[tool(
        name = "nomiso_activate_embedding_generation",
        description = "Atomically activate a staged generation: revalidates coverage inside the transaction, swaps vectors, retires the prior generation."
    )]
    async fn nomiso_activate_embedding_generation(
        &self,
        Parameters(args): Parameters<GenerationArgs>,
    ) -> CallToolResult {
        match self
            .client
            .activate_embedding_generation(args.generation)
            .await
        {
            Ok(g) => json_text(&g),
            Err(e) => err_text(e),
        }
    }

    // --- Durable job journal (JOB-001..007) ---

    /// Enqueue a durable job (dedup-safe).
    #[tool(
        name = "nomiso_enqueue_job",
        description = "Enqueue a durable job: typed pinned inputs, composition identity, budget. Identical live intent deduplicates."
    )]
    async fn nomiso_enqueue_job(
        &self,
        Parameters(args): Parameters<nomiso_core::EnqueueJobRequest>,
    ) -> CallToolResult {
        match self.client.enqueue_job(args).await {
            Ok(r) => json_text(&r),
            Err(e) => err_text(e),
        }
    }

    /// Get a job record by scope + id.
    #[tool(
        name = "nomiso_get_job",
        description = "Get a full job record (includes checkpoint/result) by scope + id."
    )]
    async fn nomiso_get_job(&self, Parameters(args): Parameters<ScopedId>) -> CallToolResult {
        match self.client.get_job(&args.scope, &args.id).await {
            Ok(j) => json_text(&j),
            Err(e) => err_text(e),
        }
    }

    /// List job summaries (payload bodies omitted).
    #[tool(
        name = "nomiso_list_jobs",
        description = "List job summaries for lifecycle inspection; raw payload/checkpoint/result bodies are never included (JOB-007)."
    )]
    async fn nomiso_list_jobs(
        &self,
        Parameters(args): Parameters<nomiso_core::ListJobsRequest>,
    ) -> CallToolResult {
        match self.client.list_jobs(args).await {
            Ok(v) => json_text(&v),
            Err(e) => err_text(e),
        }
    }

    /// Claim the next eligible job inside a scope/kind grant.
    #[tool(
        name = "nomiso_claim_job",
        description = "Claim the next eligible pending/expired-lease job under a fenced lease, restricted to scope + kind grants."
    )]
    async fn nomiso_claim_job(
        &self,
        Parameters(args): Parameters<nomiso_core::ClaimJobRequest>,
    ) -> CallToolResult {
        match self.client.claim_job(args).await {
            Ok(l) => json_text(&l),
            Err(e) => err_text(e),
        }
    }

    /// Extend the active lease under its fence.
    #[tool(
        name = "nomiso_renew_job_lease",
        description = "Renew a job's lease under its fencing token (worker liveness signal)."
    )]
    async fn nomiso_renew_job_lease(
        &self,
        Parameters(args): Parameters<FencedArgs>,
    ) -> CallToolResult {
        match self
            .client
            .renew_job_lease(&args.id, args.fence, &args.worker)
            .await
        {
            Ok(l) => json_text(&l),
            Err(e) => err_text(e),
        }
    }

    /// Persist bounded progress under the active fence.
    #[tool(
        name = "nomiso_checkpoint_job",
        description = "Persist a bounded durable checkpoint under the active lease fence (resumable progress, not a transcript)."
    )]
    async fn nomiso_checkpoint_job(
        &self,
        Parameters(args): Parameters<JobValueArgs>,
    ) -> CallToolResult {
        match self
            .client
            .checkpoint_job(&args.id, args.fence, &args.worker, args.value)
            .await
        {
            Ok(j) => json_text(&j),
            Err(e) => err_text(e),
        }
    }

    /// Commit final effects; pinned inputs revalidate in the same transaction.
    #[tool(
        name = "nomiso_complete_job",
        description = "Complete a job under its fence; pinned input revisions are revalidated in the same transaction (stale/purged inputs fail closed)."
    )]
    async fn nomiso_complete_job(
        &self,
        Parameters(args): Parameters<JobValueArgs>,
    ) -> CallToolResult {
        match self
            .client
            .complete_job(&args.id, args.fence, &args.worker, args.value)
            .await
        {
            Ok(j) => json_text(&j),
            Err(e) => err_text(e),
        }
    }

    /// Record a failure; retryable failures within budget return to pending.
    #[tool(
        name = "nomiso_fail_job",
        description = "Record a job failure under its fence; retryable failures reschedule with exponential backoff within budget."
    )]
    async fn nomiso_fail_job(&self, Parameters(args): Parameters<JobFailArgs>) -> CallToolResult {
        match self
            .client
            .fail_job(&args.id, args.fence, &args.worker, args.error)
            .await
        {
            Ok(j) => json_text(&j),
            Err(e) => err_text(e),
        }
    }

    /// Cancel a job (cancellation is not rollback).
    #[tool(
        name = "nomiso_cancel_job",
        description = "Cancel a pending or leased job; committed checkpoints remain visible (cancellation is not rollback)."
    )]
    async fn nomiso_cancel_job(
        &self,
        Parameters(args): Parameters<JobReasonArgs>,
    ) -> CallToolResult {
        match self
            .client
            .cancel_job(&args.scope, &args.id, &args.reason)
            .await
        {
            Ok(j) => json_text(&j),
            Err(e) => err_text(e),
        }
    }

    /// Supersede a pending/leased job with a newer one.
    #[tool(
        name = "nomiso_supersede_job",
        description = "Supersede a pending/leased job by its replacement's id (planned work moves forward without duplicate execution)."
    )]
    async fn nomiso_supersede_job(
        &self,
        Parameters(args): Parameters<JobSupersedeArgs>,
    ) -> CallToolResult {
        match self
            .client
            .supersede_job(&args.scope, &args.id, &args.replacement_id, &args.reason)
            .await
        {
            Ok(j) => json_text(&j),
            Err(e) => err_text(e),
        }
    }
}

/// Shared arg shapes for positional-parameter ops.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ScopedId {
    scope: String,
    id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct EmptyArgs {}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct PutWithJobsArgs {
    put: PutRequest,
    #[serde(default)]
    jobs: Vec<nomiso_core::JobIntent>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SupersedeWithJobsArgs {
    supersede: SupersedeRequest,
    #[serde(default)]
    jobs: Vec<nomiso_core::JobIntent>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct GenerationArgs {
    generation: u64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct StageArgs {
    generation: u64,
    items: Vec<nomiso_core::StagedEmbedding>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct FencedArgs {
    id: String,
    fence: u64,
    worker: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct JobValueArgs {
    id: String,
    fence: u64,
    worker: String,
    value: serde_json::Value,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct JobFailArgs {
    id: String,
    fence: u64,
    worker: String,
    error: nomiso_core::JobError,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct JobReasonArgs {
    scope: String,
    id: String,
    reason: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct JobSupersedeArgs {
    scope: String,
    id: String,
    replacement_id: String,
    reason: String,
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for NomisoMcp {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.server_info = Implementation::new("nomisod", env!("CARGO_PKG_VERSION"));
        info.instructions = Some(
            "Nomiso memory plane tools. Structured ops only; no LLM inside Nomiso. ABI: nomiso.op.v1. Temporal lenses: as_of/known_as_of/sys_as_of on search/read/list."
                .into(),
        );
        info
    }
}

/// List tool names exported by this server (for tests / docs).
pub fn tool_names() -> &'static [&'static str] {
    &[
        "nomiso_search",
        "nomiso_search_detailed",
        "nomiso_read",
        "nomiso_put",
        "nomiso_put_with_jobs",
        "nomiso_supersede",
        "nomiso_supersede_with_jobs",
        "nomiso_forget",
        "nomiso_list",
        "nomiso_count",
        "nomiso_put_task_state",
        "nomiso_get_task_state",
        "nomiso_belief_events",
        "nomiso_put_entity",
        "nomiso_get_entity",
        "nomiso_update_entity",
        "nomiso_put_relationship",
        "nomiso_update_relationship",
        "nomiso_list_relationships",
        "nomiso_traverse",
        "nomiso_embedding_state",
        "nomiso_attest_embedding_identity",
        "nomiso_declare_embedding_generation",
        "nomiso_stage_embeddings",
        "nomiso_activate_embedding_generation",
        "nomiso_enqueue_job",
        "nomiso_get_job",
        "nomiso_list_jobs",
        "nomiso_claim_job",
        "nomiso_renew_job_lease",
        "nomiso_checkpoint_job",
        "nomiso_complete_job",
        "nomiso_fail_job",
        "nomiso_cancel_job",
        "nomiso_supersede_job",
    ]
}

/// Serve MCP over stdio until the client disconnects.
pub async fn serve_stdio(client: NomisoClient) -> anyhow::Result<()> {
    use rmcp::ServiceExt;
    let server = NomisoMcp::new(client);
    let transport = rmcp::transport::stdio();
    let service = server.serve(transport).await?;
    service.waiting().await?;
    Ok(())
}

/// Alias used by the facade binary.
pub use serve_stdio as run_stdio;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_expected_tools() {
        let names = tool_names();
        assert!(names.contains(&"nomiso_search"));
        assert!(names.contains(&"nomiso_search_detailed"));
        assert!(names.contains(&"nomiso_put"));
        assert!(names.contains(&"nomiso_put_with_jobs"));
        assert!(names.contains(&"nomiso_supersede"));
        assert!(names.contains(&"nomiso_supersede_with_jobs"));
        assert!(names.contains(&"nomiso_list"));
        assert!(names.contains(&"nomiso_put_task_state"));
        assert!(names.contains(&"nomiso_belief_events"));
        for t in [
            "nomiso_put_entity",
            "nomiso_get_entity",
            "nomiso_update_entity",
            "nomiso_put_relationship",
            "nomiso_update_relationship",
            "nomiso_list_relationships",
            "nomiso_traverse",
            "nomiso_embedding_state",
            "nomiso_attest_embedding_identity",
            "nomiso_declare_embedding_generation",
            "nomiso_stage_embeddings",
            "nomiso_activate_embedding_generation",
            "nomiso_enqueue_job",
            "nomiso_get_job",
            "nomiso_list_jobs",
            "nomiso_claim_job",
            "nomiso_renew_job_lease",
            "nomiso_checkpoint_job",
            "nomiso_complete_job",
            "nomiso_fail_job",
            "nomiso_cancel_job",
            "nomiso_supersede_job",
        ] {
            assert!(names.contains(&t), "missing tool {t}");
        }
        assert_eq!(names.len(), 35);
    }

    #[tokio::test]
    async fn handlers_return_typed_call_tool_results() {
        let client = NomisoClient::connect(nomiso_store::StoreConfig::memory_test(8))
            .await
            .unwrap();
        let server = NomisoMcp::new(client);

        let bad = server
            .nomiso_list(Parameters(ListRequest {
                scope: "!!bad scope".into(),
                scope_match: Default::default(),
                categories: None,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
                text: None,
                limit: None,
                cursor: None,
            }))
            .await;
        assert_eq!(bad.is_error, Some(true));
        let err = bad.structured_content.expect("structured error");
        assert_eq!(err["code"], "invalid_request");

        let ok = server
            .nomiso_count(Parameters(CountRequest {
                scope: "org/t".into(),
                scope_match: Default::default(),
                categories: None,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
                text: None,
            }))
            .await;
        assert_eq!(ok.is_error, Some(false));
        let v = ok.structured_content.expect("structured ok");
        assert_eq!(v["count"], 0);
    }

    #[tokio::test]
    async fn job_tools_roundtrip_and_summary_privacy() {
        let client = NomisoClient::connect(nomiso_store::StoreConfig::memory_test(8))
            .await
            .unwrap();
        let server = NomisoMcp::new(client);

        let enq = server
            .nomiso_enqueue_job(Parameters(nomiso_core::EnqueueJobRequest {
                scope: "org/t".into(),
                kind: "reindex".into(),
                inputs: vec![],
                composition: None,
                payload: Some(serde_json::json!({"secret_payload": "sensitive"})),
                budget: nomiso_core::JobBudget {
                    max_attempts: 3,
                    lease_ms: 5_000,
                    retry_backoff_ms: None,
                    deadline_ms: None,
                },
                dedup_hint: None,
            }))
            .await;
        assert_eq!(enq.is_error, Some(false));
        let job_id = enq.structured_content.unwrap()["job"]["id"]
            .as_str()
            .unwrap()
            .to_string();

        // Summaries expose lifecycle without raw payload bodies (JOB-007).
        let list = server
            .nomiso_list_jobs(Parameters(nomiso_core::ListJobsRequest {
                scope: "org/t".into(),
                state: None,
                kind: None,
                limit: None,
            }))
            .await;
        assert_eq!(list.is_error, Some(false));
        // Array results are returned as a text JSON block (non-object).
        let text = list.content[0].as_text().unwrap().text.clone();
        assert!(!text.contains("sensitive"), "summary leaked payload");
        let summaries: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(summaries.as_array().unwrap().len(), 1);
        assert!(summaries[0].get("payload").is_none());

        let claim = server
            .nomiso_claim_job(Parameters(nomiso_core::ClaimJobRequest {
                worker: "mcp-w".into(),
                scopes: vec!["org/t".into()],
                kinds: vec!["reindex".into()],
            }))
            .await;
        assert_eq!(claim.is_error, Some(false));
        let lease = claim.structured_content.unwrap();
        let fence = lease["fence"].as_u64().unwrap();

        // A stale fence must surface a typed lease_lost error, not silently write.
        let stale = server
            .nomiso_complete_job(Parameters(JobValueArgs {
                id: job_id.clone(),
                fence: fence + 99,
                worker: "mcp-w".into(),
                value: serde_json::json!({}),
            }))
            .await;
        assert_eq!(stale.is_error, Some(true));
        assert_eq!(stale.structured_content.unwrap()["code"], "lease_lost");

        let done = server
            .nomiso_complete_job(Parameters(JobValueArgs {
                id: job_id.clone(),
                fence,
                worker: "mcp-w".into(),
                value: serde_json::json!({"ok": true}),
            }))
            .await;
        assert_eq!(done.is_error, Some(false));
        assert_eq!(done.structured_content.unwrap()["state"], "succeeded");
    }
}
