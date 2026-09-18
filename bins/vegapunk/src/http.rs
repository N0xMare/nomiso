//! Thin HTTP product API for Vegapunk (`vegapunk serve`).
//!
//! Mirrors the lib — no model endpoints. Optional Bearer API key.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;
use vegapunk::{CheckpointInput, RememberInput, Vegapunk, WriterOp};

/// Shared serve state.
#[derive(Clone)]
pub struct ServeState {
    pub vp: Vegapunk,
    pub api_key: Option<SecretString>,
}

/// Build product router.
pub fn router(state: ServeState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/remember", post(remember))
        .route("/v1/supersede", post(supersede))
        .route("/v1/apply_ops", post(apply_ops))
        .route("/v1/recall", post(recall))
        .route("/v1/hard_recall", post(hard_recall))
        .route("/v1/read", post(read))
        .route("/v1/checkpoint", post(checkpoint))
        .route("/v1/sleep", post(sleep))
        // Relationship plane (REL-*).
        .route("/v1/entities", post(put_entity))
        .route("/v1/entities/get", post(get_entity))
        .route("/v1/entities/update", post(update_entity))
        .route("/v1/relationships", post(put_relationship))
        .route("/v1/relationships/update", post(update_relationship))
        .route("/v1/relationships/list", post(list_relationships))
        .route("/v1/traverse", post(traverse))
        // Embedding administration (MIG-004/005).
        .route("/v1/embed/state", post(embed_state))
        .route("/v1/embed/attest", post(embed_attest))
        .route("/v1/embed/declare", post(embed_declare))
        .route("/v1/embed/stage", post(embed_stage))
        .route("/v1/embed/activate", post(embed_activate))
        // Durable job journal (JOB-*).
        .route("/v1/jobs", post(job_enqueue))
        .route("/v1/jobs/get", post(job_get))
        .route("/v1/jobs/list", post(job_list))
        .route("/v1/jobs/claim", post(job_claim))
        .route("/v1/jobs/renew", post(job_renew))
        .route("/v1/jobs/checkpoint", post(job_checkpoint))
        .route("/v1/jobs/complete", post(job_complete))
        .route("/v1/jobs/fail", post(job_fail))
        .route("/v1/jobs/cancel", post(job_cancel))
        .route("/v1/jobs/supersede", post(job_supersede))
        // T4 controller surface (CTX-*): typed proposals + verified
        // insertion acks. The host inserts; Vegapunk never does.
        .route("/v1/prepare_context", post(prepare_context))
        .route("/v1/record_insertion", post(record_insertion))
        .route("/v1/trace_outcome", post(trace_outcome))
        .route("/v1/working_state", post(working_state))
        .layer(TraceLayer::new_for_http())
        .layer(RequestBodyLimitLayer::new(512 * 1024))
        .with_state(Arc::new(state))
}

async fn health(State(st): State<Arc<ServeState>>) -> Response {
    match st.vp.health().await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"status":"ok"}))).into_response(),
        Err(e) => err_resp(StatusCode::SERVICE_UNAVAILABLE, "health_failed", e),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RememberBody {
    scope: String,
    text: String,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    confidence: Option<f64>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    episodic: bool,
    #[serde(default)]
    idempotency_key: Option<String>,
}

async fn remember(
    State(st): State<Arc<ServeState>>,
    headers: HeaderMap,
    Json(body): Json<RememberBody>,
) -> Response {
    if let Err(r) = authorize(&st, &headers) {
        return *r;
    }
    let category = match parse_category(body.category.as_deref()) {
        Ok(c) => c,
        Err(r) => return r,
    };
    match st
        .vp
        .remember(RememberInput {
            scope: body.scope,
            text: body.text,
            category,
            confidence: body.confidence,
            source: body.source,
            embedding: None,
            episodic: body.episodic,
            idempotency_key: body.idempotency_key,
        })
        .await
    {
        Ok(o) => (StatusCode::OK, Json(o)).into_response(),
        Err(e) => domain_error(e),
    }
}

#[derive(Debug, Deserialize)]
struct SupersedeBody {
    scope: String,
    prior_id: String,
    expected_version: u64,
    text: String,
}

async fn supersede(
    State(st): State<Arc<ServeState>>,
    headers: HeaderMap,
    Json(body): Json<SupersedeBody>,
) -> Response {
    if let Err(r) = authorize(&st, &headers) {
        return *r;
    }
    match st
        .vp
        .supersede(
            nomiso::MemoryId::new(body.prior_id),
            body.expected_version,
            RememberInput::fact(body.scope, body.text),
        )
        .await
    {
        Ok(o) => (StatusCode::OK, Json(o)).into_response(),
        Err(e) => domain_error(e),
    }
}

#[derive(Debug, Deserialize)]
struct ApplyOpsBody {
    /// Active scope pin (required). All ops must target this scope.
    scope: String,
    /// Preferred: structured WriterOp array.
    #[serde(default)]
    ops: Option<Vec<WriterOp>>,
    /// Alternate: raw JSON string (fence-tolerant host extract).
    #[serde(default)]
    ops_json: Option<String>,
}

async fn apply_ops(
    State(st): State<Arc<ServeState>>,
    headers: HeaderMap,
    Json(body): Json<ApplyOpsBody>,
) -> Response {
    if let Err(r) = authorize(&st, &headers) {
        return *r;
    }
    let ops = if let Some(ops) = body.ops {
        ops
    } else if let Some(raw) = body.ops_json {
        match vegapunk::parse_writer_ops_from_model(&raw) {
            Ok(o) => o,
            Err(e) => return err_resp(StatusCode::BAD_REQUEST, "ops_parse_failed", e),
        }
    } else {
        return err_resp(
            StatusCode::BAD_REQUEST,
            "ops_required",
            "provide ops array or ops_json string",
        );
    };
    match st.vp.apply_writer_ops(&body.scope, &ops).await {
        Ok(report) => {
            let status = if report.is_ok() {
                StatusCode::OK
            } else {
                report
                    .outcomes
                    .iter()
                    .find_map(|o| match o {
                        vegapunk::ApplyOpOutcome::Err { code, .. } if code != "not_attempted" => {
                            Some(status_for_code(code))
                        }
                        _ => None,
                    })
                    .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
            };
            (status, Json(report)).into_response()
        }
        Err(e) => domain_error(e),
    }
}

#[derive(Debug, Deserialize)]
struct RecallBody {
    scope: String,
    query: String,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    as_of: Option<String>,
    #[serde(default)]
    known_as_of: Option<String>,
    #[serde(default)]
    sys_as_of: Option<String>,
    #[serde(default)]
    category: Option<String>,
}

async fn recall(
    State(st): State<Arc<ServeState>>,
    headers: HeaderMap,
    Json(body): Json<RecallBody>,
) -> Response {
    if let Err(r) = authorize(&st, &headers) {
        return *r;
    }
    let mut opts = vegapunk::RecallOptions::from_policy(st.vp.policy());
    if let Some(l) = body.limit {
        opts.limit = l;
    }
    if let Some(s) = body.as_of.as_deref() {
        match vegapunk::parse_timestamp(s) {
            Ok(t) => opts.as_of = Some(t),
            Err(e) => return err_resp(StatusCode::BAD_REQUEST, "bad_as_of", e),
        }
    }
    if let Some(s) = body.known_as_of.as_deref() {
        match vegapunk::parse_timestamp(s) {
            Ok(t) => opts.known_as_of = Some(t),
            Err(e) => return err_resp(StatusCode::BAD_REQUEST, "bad_known_as_of", e),
        }
    }
    if let Some(s) = body.sys_as_of.as_deref() {
        match vegapunk::parse_timestamp(s) {
            Ok(t) => opts.sys_as_of = Some(t),
            Err(e) => return err_resp(StatusCode::BAD_REQUEST, "bad_sys_as_of", e),
        }
    }
    match parse_category(body.category.as_deref()) {
        Ok(c) => opts.categories = c.map(|c| vec![c]),
        Err(r) => return r,
    }
    match st.vp.recall_with(&body.scope, &body.query, opts).await {
        Ok(hits) => {
            let cards: Vec<_> = hits
                .iter()
                .map(|h| {
                    serde_json::json!({
                        "id": h.id.to_string(),
                        "score": h.score,
                        "category": h.category.as_str(),
                        "preview": h.preview,
                        "version": h.version,
                    })
                })
                .collect();
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "count": cards.len(),
                    "scope": body.scope,
                    "hits": cards,
                })),
            )
                .into_response()
        }
        Err(e) => domain_error(e),
    }
}

#[derive(Debug, Deserialize)]
struct ReadBody {
    scope: String,
    ids: Vec<String>,
}

async fn read(
    State(st): State<Arc<ServeState>>,
    headers: HeaderMap,
    Json(body): Json<ReadBody>,
) -> Response {
    if let Err(r) = authorize(&st, &headers) {
        return *r;
    }
    match st
        .vp
        .client()
        .read(nomiso::ReadRequest {
            ids: body.ids.into_iter().map(nomiso::MemoryId::new).collect(),
            scope: body.scope,
            scope_match: nomiso::ScopeMatch::Exact,
            as_of: None,
            known_as_of: None,
            sys_as_of: None,
        })
        .await
    {
        Ok(rows) => (StatusCode::OK, Json(rows)).into_response(),
        Err(e) => domain_error(e.into()),
    }
}

#[derive(Debug, Deserialize)]
struct HardRecallBody {
    scope: String,
    query: String,
    #[serde(default)]
    pack: bool,
    #[serde(default)]
    min_score: Option<f64>,
    #[serde(default)]
    as_of: Option<String>,
    #[serde(default)]
    known_as_of: Option<String>,
    #[serde(default)]
    sys_as_of: Option<String>,
    #[serde(default)]
    category: Option<String>,
}

async fn hard_recall(
    State(st): State<Arc<ServeState>>,
    headers: HeaderMap,
    Json(body): Json<HardRecallBody>,
) -> Response {
    if let Err(r) = authorize(&st, &headers) {
        return *r;
    }
    let mut opts = vegapunk::HardRecallOptions::from_policy(st.vp.policy());
    if let Some(floor) = body.min_score {
        opts.min_score = Some(floor);
    }
    if let Some(s) = body.as_of.as_deref() {
        match vegapunk::parse_timestamp(s) {
            Ok(t) => opts.as_of = Some(t),
            Err(e) => return err_resp(StatusCode::BAD_REQUEST, "bad_as_of", e),
        }
    }
    if let Some(s) = body.known_as_of.as_deref() {
        match vegapunk::parse_timestamp(s) {
            Ok(t) => opts.known_as_of = Some(t),
            Err(e) => return err_resp(StatusCode::BAD_REQUEST, "bad_known_as_of", e),
        }
    }
    if let Some(s) = body.sys_as_of.as_deref() {
        match vegapunk::parse_timestamp(s) {
            Ok(t) => opts.sys_as_of = Some(t),
            Err(e) => return err_resp(StatusCode::BAD_REQUEST, "bad_sys_as_of", e),
        }
    }
    match parse_category(body.category.as_deref()) {
        Ok(c) => opts.categories = c.map(|c| vec![c]),
        Err(r) => return r,
    }
    if body.pack {
        match st
            .vp
            .hard_recall_pack_with(&body.scope, &body.query, opts)
            .await
        {
            Ok((hr, ctx)) => (
                StatusCode::OK,
                Json(serde_json::json!({
                    "abstained": hr.abstained || ctx.abstained,
                    "trace_id": hr.trace_id,
                    "queries": hr.queries,
                    "hit_count": hr.hits.len(),
                    "pack": ctx,
                    "inject_note": "Host injects pack.block only if non-empty; never auto-injected",
                })),
            )
                .into_response(),
            Err(e) => domain_error(e),
        }
    } else {
        match st.vp.hard_recall_with(&body.scope, &body.query, opts).await {
            Ok(hr) => (StatusCode::OK, Json(hr)).into_response(),
            Err(e) => domain_error(e),
        }
    }
}

async fn checkpoint(
    State(st): State<Arc<ServeState>>,
    headers: HeaderMap,
    Json(body): Json<CheckpointInput>,
) -> Response {
    if let Err(r) = authorize(&st, &headers) {
        return *r;
    }
    match st.vp.checkpoint(body).await {
        Ok(o) => (StatusCode::OK, Json(o)).into_response(),
        Err(e) => domain_error(e),
    }
}

#[derive(Debug, Deserialize)]
struct SleepBody {
    scope: String,
    /// HTTP sleep is dry-run only. `apply: true` is rejected (CLI/MCP).
    #[serde(default)]
    apply: Option<bool>,
}

async fn sleep(
    State(st): State<Arc<ServeState>>,
    headers: HeaderMap,
    Json(body): Json<SleepBody>,
) -> Response {
    if let Err(r) = authorize(&st, &headers) {
        return *r;
    }
    if body.apply == Some(true) {
        return err_resp(
            StatusCode::BAD_REQUEST,
            "sleep_apply_http_deferred",
            "HTTP /v1/sleep is dry-run only; apply via CLI `vegapunk sleep --apply` or product MCP",
        );
    }
    match st.vp.sleep(&body.scope).await {
        Ok(o) => (StatusCode::OK, Json(o)).into_response(),
        Err(e) => domain_error(e),
    }
}

// --- Relationship plane ---

async fn put_entity(
    State(st): State<Arc<ServeState>>,
    headers: HeaderMap,
    Json(body): Json<nomiso::PutEntityRequest>,
) -> Response {
    if let Err(r) = authorize(&st, &headers) {
        return *r;
    }
    match st.vp.client().put_entity(body).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => domain_error(e.into()),
    }
}

async fn get_entity(
    State(st): State<Arc<ServeState>>,
    headers: HeaderMap,
    Json(body): Json<ScopedIdBody>,
) -> Response {
    if let Err(r) = authorize(&st, &headers) {
        return *r;
    }
    match st.vp.client().get_entity(&body.id, &body.scope).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => domain_error(e.into()),
    }
}

async fn update_entity(
    State(st): State<Arc<ServeState>>,
    headers: HeaderMap,
    Json(body): Json<nomiso::UpdateEntityRequest>,
) -> Response {
    if let Err(r) = authorize(&st, &headers) {
        return *r;
    }
    match st.vp.client().update_entity(body).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => domain_error(e.into()),
    }
}

async fn put_relationship(
    State(st): State<Arc<ServeState>>,
    headers: HeaderMap,
    Json(body): Json<nomiso::PutRelationshipRequest>,
) -> Response {
    if let Err(r) = authorize(&st, &headers) {
        return *r;
    }
    match st.vp.client().put_relationship(body).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => domain_error(e.into()),
    }
}

async fn update_relationship(
    State(st): State<Arc<ServeState>>,
    headers: HeaderMap,
    Json(body): Json<nomiso::UpdateRelationshipRequest>,
) -> Response {
    if let Err(r) = authorize(&st, &headers) {
        return *r;
    }
    match st.vp.client().update_relationship(body).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => domain_error(e.into()),
    }
}

async fn list_relationships(
    State(st): State<Arc<ServeState>>,
    headers: HeaderMap,
    Json(body): Json<nomiso::ListRelationshipsRequest>,
) -> Response {
    if let Err(r) = authorize(&st, &headers) {
        return *r;
    }
    match st.vp.client().list_relationships(body).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => domain_error(e.into()),
    }
}

async fn traverse(
    State(st): State<Arc<ServeState>>,
    headers: HeaderMap,
    Json(body): Json<nomiso::TraverseRequest>,
) -> Response {
    if let Err(r) = authorize(&st, &headers) {
        return *r;
    }
    match st.vp.client().traverse(body).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => domain_error(e.into()),
    }
}

// --- Embedding administration ---

async fn embed_state(State(st): State<Arc<ServeState>>, headers: HeaderMap) -> Response {
    if let Err(r) = authorize(&st, &headers) {
        return *r;
    }
    match st.vp.client().embedding_state().await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => domain_error(e.into()),
    }
}

async fn embed_attest(
    State(st): State<Arc<ServeState>>,
    headers: HeaderMap,
    Json(body): Json<nomiso::EmbeddingIdentity>,
) -> Response {
    if let Err(r) = authorize(&st, &headers) {
        return *r;
    }
    match st.vp.client().attest_embedding_identity(body).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => domain_error(e.into()),
    }
}

async fn embed_declare(
    State(st): State<Arc<ServeState>>,
    headers: HeaderMap,
    Json(body): Json<nomiso::DeclareGenerationRequest>,
) -> Response {
    if let Err(r) = authorize(&st, &headers) {
        return *r;
    }
    match st.vp.client().declare_embedding_generation(body).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => domain_error(e.into()),
    }
}

async fn embed_stage(
    State(st): State<Arc<ServeState>>,
    headers: HeaderMap,
    Json(body): Json<StageBody>,
) -> Response {
    if let Err(r) = authorize(&st, &headers) {
        return *r;
    }
    match st
        .vp
        .client()
        .stage_embeddings(body.generation, body.items)
        .await
    {
        Ok(n) => (StatusCode::OK, Json(serde_json::json!({ "staged": n }))).into_response(),
        Err(e) => domain_error(e.into()),
    }
}

async fn embed_activate(
    State(st): State<Arc<ServeState>>,
    headers: HeaderMap,
    Json(body): Json<GenerationBody>,
) -> Response {
    if let Err(r) = authorize(&st, &headers) {
        return *r;
    }
    match st
        .vp
        .client()
        .activate_embedding_generation(body.generation)
        .await
    {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => domain_error(e.into()),
    }
}

// --- Durable job journal ---

async fn job_enqueue(
    State(st): State<Arc<ServeState>>,
    headers: HeaderMap,
    Json(body): Json<nomiso::EnqueueJobRequest>,
) -> Response {
    if let Err(r) = authorize(&st, &headers) {
        return *r;
    }
    match st.vp.client().enqueue_job(body).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => domain_error(e.into()),
    }
}

async fn job_get(
    State(st): State<Arc<ServeState>>,
    headers: HeaderMap,
    Json(body): Json<ScopedIdBody>,
) -> Response {
    if let Err(r) = authorize(&st, &headers) {
        return *r;
    }
    match st.vp.client().get_job(&body.scope, &body.id).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => domain_error(e.into()),
    }
}

async fn job_list(
    State(st): State<Arc<ServeState>>,
    headers: HeaderMap,
    Json(body): Json<nomiso::ListJobsRequest>,
) -> Response {
    if let Err(r) = authorize(&st, &headers) {
        return *r;
    }
    match st.vp.client().list_jobs(body).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => domain_error(e.into()),
    }
}

async fn job_claim(
    State(st): State<Arc<ServeState>>,
    headers: HeaderMap,
    Json(body): Json<nomiso::ClaimJobRequest>,
) -> Response {
    if let Err(r) = authorize(&st, &headers) {
        return *r;
    }
    match st.vp.client().claim_job(body).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => domain_error(e.into()),
    }
}

async fn job_renew(
    State(st): State<Arc<ServeState>>,
    headers: HeaderMap,
    Json(body): Json<FencedBody>,
) -> Response {
    if let Err(r) = authorize(&st, &headers) {
        return *r;
    }
    match st
        .vp
        .client()
        .renew_job_lease(&body.id, body.fence, &body.worker)
        .await
    {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => domain_error(e.into()),
    }
}

async fn job_checkpoint(
    State(st): State<Arc<ServeState>>,
    headers: HeaderMap,
    Json(body): Json<JobValueBody>,
) -> Response {
    if let Err(r) = authorize(&st, &headers) {
        return *r;
    }
    match st
        .vp
        .client()
        .checkpoint_job(&body.id, body.fence, &body.worker, body.value)
        .await
    {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => domain_error(e.into()),
    }
}

async fn job_complete(
    State(st): State<Arc<ServeState>>,
    headers: HeaderMap,
    Json(body): Json<JobValueBody>,
) -> Response {
    if let Err(r) = authorize(&st, &headers) {
        return *r;
    }
    match st
        .vp
        .client()
        .complete_job(&body.id, body.fence, &body.worker, body.value)
        .await
    {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => domain_error(e.into()),
    }
}

async fn job_fail(
    State(st): State<Arc<ServeState>>,
    headers: HeaderMap,
    Json(body): Json<JobFailBody>,
) -> Response {
    if let Err(r) = authorize(&st, &headers) {
        return *r;
    }
    match st
        .vp
        .client()
        .fail_job(&body.id, body.fence, &body.worker, body.error)
        .await
    {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => domain_error(e.into()),
    }
}

async fn job_cancel(
    State(st): State<Arc<ServeState>>,
    headers: HeaderMap,
    Json(body): Json<JobReasonBody>,
) -> Response {
    if let Err(r) = authorize(&st, &headers) {
        return *r;
    }
    match st
        .vp
        .client()
        .cancel_job(&body.scope, &body.id, &body.reason)
        .await
    {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => domain_error(e.into()),
    }
}

async fn job_supersede(
    State(st): State<Arc<ServeState>>,
    headers: HeaderMap,
    Json(body): Json<JobSupersedeBody>,
) -> Response {
    if let Err(r) = authorize(&st, &headers) {
        return *r;
    }
    match st
        .vp
        .client()
        .supersede_job(&body.scope, &body.id, &body.replacement_id, &body.reason)
        .await
    {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => domain_error(e.into()),
    }
}

// --- Shared request bodies for positional-parameter ops ---

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopedIdBody {
    scope: String,
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GenerationBody {
    generation: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StageBody {
    generation: u64,
    items: Vec<nomiso::StagedEmbedding>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FencedBody {
    id: String,
    fence: u64,
    worker: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JobValueBody {
    id: String,
    fence: u64,
    worker: String,
    value: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JobFailBody {
    id: String,
    fence: u64,
    worker: String,
    error: nomiso::JobError,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JobReasonBody {
    scope: String,
    id: String,
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JobSupersedeBody {
    scope: String,
    id: String,
    replacement_id: String,
    reason: String,
}

fn authorize(state: &ServeState, headers: &HeaderMap) -> Result<(), Box<Response>> {
    let Some(expected) = &state.api_key else {
        return Ok(());
    };
    let provided = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let token = provided
        .strip_prefix("Bearer ")
        .or_else(|| provided.strip_prefix("bearer "))
        .unwrap_or(provided);
    let a = token.as_bytes();
    let b = expected.expose_secret().as_bytes();
    // Length check then constant-time compare (local API key model).
    if a.len() != b.len() || !bool::from(a.ct_eq(b)) {
        return Err(Box::new(
            (
                StatusCode::UNAUTHORIZED,
                Json(ErrorBody {
                    error: "unauthorized".into(),
                    code: "unauthorized".into(),
                    help: vec!["pass Authorization: Bearer <api_key>".into()],
                }),
            )
                .into_response(),
        ));
    }
    Ok(())
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
    code: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    help: Vec<String>,
}

/// Same contract as MCP `parse_category`: unknown and `trace` are errors.
#[allow(clippy::result_large_err)]
fn parse_category(raw: Option<&str>) -> Result<Option<nomiso::Category>, Response> {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) => match nomiso::Category::parse(s) {
            Some(nomiso::Category::Trace) | None => Err(err_resp(
                StatusCode::BAD_REQUEST,
                "bad_category",
                format!(
                    "category must be semantic|episodic|identity|procedural|uncertainty, got {s}"
                ),
            )),
            Some(c) => Ok(Some(c)),
        },
    }
}

fn err_resp(status: StatusCode, code: &str, e: impl std::fmt::Display) -> Response {
    (
        status,
        Json(ErrorBody {
            error: e.to_string(),
            code: code.into(),
            help: vec![],
        }),
    )
        .into_response()
}

/// Stable code → HTTP status for domain errors.
fn status_for_code(code: &str) -> StatusCode {
    match code {
        "invalid_request" | "invalid" | "policy_rejected" | "policy" | "dimension_mismatch" => {
            StatusCode::BAD_REQUEST
        }
        "conflict"
        | "idempotency_conflict"
        | "idempotency_unavailable"
        | "lease_lost"
        | "stale_input" => StatusCode::CONFLICT,
        "not_found" => StatusCode::NOT_FOUND,
        "scope_denied" => StatusCode::FORBIDDEN,
        "payload_too_large" => StatusCode::PAYLOAD_TOO_LARGE,
        "provider_unavailable" | "store_error" | "incompatible_store" => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        "invalid_provider_response" => StatusCode::BAD_GATEWAY,
        "deadline_exceeded" => StatusCode::GATEWAY_TIMEOUT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// Map a library error to a typed HTTP error response (code + safe message).
fn domain_error(e: vegapunk::Error) -> Response {
    (
        status_for_code(e.code()),
        Json(ErrorBody {
            error: e.public_message(),
            code: e.code().into(),
            help: vec![],
        }),
    )
        .into_response()
}

/// T4: typed prepare-context → proposal + selection manifest (CTX-*).
/// Request/response bodies are the toolkit contract types verbatim.
async fn prepare_context(
    State(st): State<Arc<ServeState>>,
    headers: HeaderMap,
    Json(body): Json<vegapunk::PrepareContextRequest>,
) -> Response {
    if let Err(r) = authorize(&st, &headers) {
        return *r;
    }
    match vegapunk::prepare_context(st.vp.client(), body).await {
        Ok(p) => (StatusCode::OK, Json(p)).into_response(),
        Err(e) => domain_error(e),
    }
}

/// T4: verified, idempotent actual-insertion acknowledgment (CTX-009).
async fn record_insertion(
    State(st): State<Arc<ServeState>>,
    headers: HeaderMap,
    Json(body): Json<vegapunk::InsertionAck>,
) -> Response {
    if let Err(r) = authorize(&st, &headers) {
        return *r;
    }
    match vegapunk::record_insertion(st.vp.client(), body).await {
        Ok(r) => (StatusCode::OK, Json(r)).into_response(),
        Err(e) => domain_error(e),
    }
}

/// T4/CTX-010: attributed outcome on a trace (helped|harmed|unknown|skipped).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TraceOutcomeBody {
    scope: String,
    trace_id: String,
    outcome: String,
    /// host | model | execution.
    #[serde(default = "default_evaluator")]
    evaluator: String,
    #[serde(default)]
    note: Option<String>,
}

fn default_evaluator() -> String {
    "host".to_string()
}

async fn trace_outcome(
    State(st): State<Arc<ServeState>>,
    headers: HeaderMap,
    Json(body): Json<TraceOutcomeBody>,
) -> Response {
    if let Err(r) = authorize(&st, &headers) {
        return *r;
    }
    let outcome = match body.outcome.as_str() {
        "helped" => vegapunk::TraceOutcome::Helped,
        "harmed" => vegapunk::TraceOutcome::Harmed,
        "unknown" => vegapunk::TraceOutcome::Unknown,
        "skipped" => vegapunk::TraceOutcome::Skipped,
        other => {
            return err_resp(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                format!("outcome must be helped|harmed|unknown|skipped, got {other}"),
            )
        }
    };
    let evaluator = match body.evaluator.as_str() {
        "host" => vegapunk::Evaluator::Host,
        "model" => vegapunk::Evaluator::Model,
        "execution" => vegapunk::Evaluator::Execution,
        other => {
            return err_resp(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                format!("evaluator must be host|model|execution, got {other}"),
            )
        }
    };
    match st
        .vp
        .record_trace_outcome_attributed(
            &body.scope,
            &body.trace_id,
            outcome,
            evaluator,
            body.note.as_deref(),
        )
        .await
    {
        Ok(()) => (
            StatusCode::OK,
            Json(serde_json::json!({"status":"ok","trace_id":body.trace_id})),
        )
            .into_response(),
        Err(e) => domain_error(e),
    }
}

/// Coding working-state slot (task-state handling for harnesses).
/// `put_json` absent → get; present → create-only or CAS on
/// `expected_version`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkingStateBody {
    scope: String,
    #[serde(default)]
    put_json: Option<serde_json::Value>,
    #[serde(default)]
    expected_version: Option<u64>,
}

async fn working_state(
    State(st): State<Arc<ServeState>>,
    headers: HeaderMap,
    Json(body): Json<WorkingStateBody>,
) -> Response {
    if let Err(r) = authorize(&st, &headers) {
        return *r;
    }
    match body.put_json {
        Some(v) => match st
            .vp
            .put_working_state(&body.scope, v, body.expected_version)
            .await
        {
            Ok(rec) => (StatusCode::OK, Json(rec)).into_response(),
            Err(e) => domain_error(e),
        },
        None => match st.vp.get_working_state(&body.scope).await {
            Ok(rec) => (StatusCode::OK, Json(serde_json::json!({"state": rec}))).into_response(),
            Err(e) => domain_error(e),
        },
    }
}

/// Product HTTP route list (docs/tests).
pub fn route_names() -> &'static [&'static str] {
    &[
        "GET /health",
        "POST /v1/remember",
        "POST /v1/supersede",
        "POST /v1/apply_ops",
        "POST /v1/recall",
        "POST /v1/hard_recall",
        "POST /v1/read",
        "POST /v1/checkpoint",
        "POST /v1/sleep (dry-run)",
        "POST /v1/entities",
        "POST /v1/entities/get",
        "POST /v1/entities/update",
        "POST /v1/relationships",
        "POST /v1/relationships/update",
        "POST /v1/relationships/list",
        "POST /v1/traverse",
        "POST /v1/embed/state",
        "POST /v1/embed/attest",
        "POST /v1/embed/declare",
        "POST /v1/embed/stage",
        "POST /v1/embed/activate",
        "POST /v1/jobs",
        "POST /v1/jobs/get",
        "POST /v1/jobs/list",
        "POST /v1/jobs/claim",
        "POST /v1/jobs/renew",
        "POST /v1/jobs/checkpoint",
        "POST /v1/jobs/complete",
        "POST /v1/jobs/fail",
        "POST /v1/jobs/cancel",
        "POST /v1/jobs/supersede",
        "POST /v1/prepare_context",
        "POST /v1/record_insertion",
        "POST /v1/trace_outcome",
        "POST /v1/working_state",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connect::connect_vegapunk;
    use vegapunk::Profile;

    #[test]
    fn routes_documented() {
        assert!(route_names().len() >= 7);
        assert!(route_names().contains(&"POST /v1/hard_recall"));
        assert!(route_names().contains(&"POST /v1/apply_ops"));
        assert!(route_names().contains(&"POST /v1/sleep (dry-run)"));
    }

    #[tokio::test]
    async fn remember_and_hard_recall_http() {
        let vp = connect_vegapunk(
            "memory",
            16,
            Profile::CodingAgent,
            crate::connect::EmbedPlan {
                hash: true,
                dim: 16,
                ..Default::default()
            },
        )
        .await
        .expect("connect");
        let app = router(ServeState { vp, api_key: None });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        let client = reqwest::Client::new();
        let base = format!("http://{addr}");
        let mut ready = false;
        for _ in 0..50 {
            if let Ok(r) = client.get(format!("{base}/health")).send().await {
                if r.status().is_success() {
                    ready = true;
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(ready, "server health not ready");
        let put = client
            .post(format!("{base}/v1/remember"))
            .json(&serde_json::json!({
                "scope": "org/http",
                "text": "Alice prefers TypeScript for agent tooling."
            }))
            .send()
            .await
            .unwrap();
        assert!(put.status().is_success(), "{}", put.text().await.unwrap());
        let hr = client
            .post(format!("{base}/v1/hard_recall"))
            .json(&serde_json::json!({
                "scope": "org/http",
                "query": "TypeScript",
                "pack": true
            }))
            .send()
            .await
            .unwrap();
        assert!(hr.status().is_success());
        let body: serde_json::Value = hr.json().await.unwrap();
        assert_eq!(body["abstained"], false);

        let nope = client
            .post(format!("{base}/v1/remember"))
            .json(&serde_json::json!({
                "scope": "org/http",
                "text": "should not infer on bad category",
                "category": "nope"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(nope.status(), StatusCode::BAD_REQUEST);
        let nope_body: serde_json::Value = nope.json().await.unwrap();
        assert_eq!(nope_body["code"], "bad_category");

        let sleep_apply = client
            .post(format!("{base}/v1/sleep"))
            .json(&serde_json::json!({
                "scope": "org/http",
                "apply": true
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(sleep_apply.status(), StatusCode::BAD_REQUEST);
        let sleep_body: serde_json::Value = sleep_apply.json().await.unwrap();
        assert_eq!(sleep_body["code"], "sleep_apply_http_deferred");
    }

    async fn test_server(vp: Vegapunk) -> (reqwest::Client, String) {
        let app = router(ServeState { vp, api_key: None });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        let client = reqwest::Client::new();
        let base = format!("http://{addr}");
        for _ in 0..50 {
            if client
                .get(format!("{base}/health"))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                return (client, base);
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("server health not ready");
    }

    async fn memory_vp() -> Vegapunk {
        connect_vegapunk(
            "memory",
            16,
            Profile::CodingAgent,
            crate::connect::EmbedPlan {
                hash: true,
                dim: 16,
                ..Default::default()
            },
        )
        .await
        .expect("connect")
    }

    #[tokio::test]
    async fn auth_required_and_conflict_typed() {
        let vp = memory_vp().await;
        let app = router(ServeState {
            vp: vp.clone(),
            api_key: Some(secrecy::SecretString::from("test-key".to_string())),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        let client = reqwest::Client::new();
        let base = format!("http://{addr}");

        let denied = client
            .post(format!("{base}/v1/remember"))
            .json(&serde_json::json!({"scope":"org/a","text":"x"}))
            .send()
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

        let put = client
            .post(format!("{base}/v1/remember"))
            .bearer_auth("test-key")
            .json(&serde_json::json!({"scope":"org/a","text":"typed conflict fact"}))
            .send()
            .await
            .unwrap();
        assert!(put.status().is_success());
        let put_body: serde_json::Value = put.json().await.unwrap();
        let id = put_body["id"].as_str().unwrap().to_string();

        let conflict = client
            .post(format!("{base}/v1/supersede"))
            .bearer_auth("test-key")
            .json(&serde_json::json!({
                "scope":"org/a",
                "prior_id": id,
                "expected_version": 99,
                "text": "replacement"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(conflict.status(), StatusCode::CONFLICT);
        let body: serde_json::Value = conflict.json().await.unwrap();
        assert_eq!(body["code"], "conflict");
    }

    #[tokio::test]
    async fn partial_apply_report_status_and_read_full() {
        let (client, base) = test_server(memory_vp().await).await;
        let ops = serde_json::json!([
            {"op":"put","scope":"org/p","text":"first durable","category":"semantic"},
            {"op":"forget","id":"memory:does-not-exist","scope":"org/p","expected_version":1},
            {"op":"put","scope":"org/p","text":"never attempted","category":"semantic"}
        ]);
        let res = client
            .post(format!("{base}/v1/apply_ops"))
            .json(&serde_json::json!({"scope":"org/p","ops":ops}))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let report: serde_json::Value = res.json().await.unwrap();
        assert_eq!(report["status"], "partial");
        assert_eq!(report["outcomes"].as_array().unwrap().len(), 3);
        assert_eq!(report["outcomes"][1]["code"], "not_found");
        assert_eq!(report["outcomes"][2]["code"], "not_attempted");

        let long_text = format!("{}tail", "durable long body ".repeat(400));
        let put = client
            .post(format!("{base}/v1/remember"))
            .json(&serde_json::json!({"scope":"org/p","text":long_text}))
            .send()
            .await
            .unwrap();
        assert!(put.status().is_success());
        let id = put.json::<serde_json::Value>().await.unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let read = client
            .post(format!("{base}/v1/read"))
            .json(&serde_json::json!({"scope":"org/p","ids":[id]}))
            .send()
            .await
            .unwrap();
        assert!(read.status().is_success());
        let rows: serde_json::Value = read.json().await.unwrap();
        assert_eq!(rows[0]["content"]["text"].as_str().unwrap(), long_text);
    }

    #[tokio::test]
    async fn provider_failure_maps_503() {
        struct Failing;
        #[async_trait::async_trait]
        impl nomiso::Embedder for Failing {
            async fn embed(&self, _t: &[String]) -> nomiso::Result<Vec<Vec<f32>>> {
                Err(nomiso::Error::ProviderUnavailable(
                    "test provider down".into(),
                ))
            }
        }
        let vp = memory_vp().await.with_embedder(Arc::new(Failing));
        let (client, base) = test_server(vp).await;
        let res = client
            .post(format!("{base}/v1/remember"))
            .json(&serde_json::json!({"scope":"org/e","text":"needs embedding"}))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body: serde_json::Value = res.json().await.unwrap();
        assert_eq!(body["code"], "provider_unavailable");
    }

    #[tokio::test]
    async fn job_and_relationship_surfaces_roundtrip() {
        let vp = memory_vp().await;
        let (client, base) = test_server(vp).await;

        // --- entities + relationship ---
        let mut ids = vec![];
        for name in ["api", "db"] {
            let res = client
                .post(format!("{base}/v1/entities"))
                .json(&serde_json::json!({
                    "scope": "org/http", "kind": "service", "name": name
                }))
                .send()
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::OK);
            let v: serde_json::Value = res.json().await.unwrap();
            ids.push(v["id"].as_str().unwrap().to_string());
        }
        let res = client
            .post(format!("{base}/v1/relationships"))
            .json(&serde_json::json!({
                "scope": "org/http",
                "predicate": "depends_on",
                "subject": {"kind": "entity", "id": ids[0]},
                "object": {"kind": "entity", "id": ids[1]}
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let res = client
            .post(format!("{base}/v1/traverse"))
            .json(&serde_json::json!({
                "scope": "org/http",
                "seeds": [{"kind": "entity", "id": ids[1]}],
                "direction": "both"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let trav: serde_json::Value = res.json().await.unwrap();
        assert_eq!(trav["nodes"].as_array().unwrap().len(), 1);

        // --- embedding state ---
        let res = client
            .post(format!("{base}/v1/embed/state"))
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let st: serde_json::Value = res.json().await.unwrap();
        assert_eq!(st["generations"].as_array().unwrap()[0]["generation"], 1);

        // --- job lifecycle: enqueue → summary privacy → claim → stale fence 409 → complete ---
        let res = client
            .post(format!("{base}/v1/jobs"))
            .json(&serde_json::json!({
                "scope": "org/http",
                "kind": "reindex",
                "payload": {"secret": "sensitive-body"},
                "budget": {"max_attempts": 3, "lease_ms": 5000}
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let enq: serde_json::Value = res.json().await.unwrap();
        let job_id = enq["job"]["id"].as_str().unwrap().to_string();

        let res = client
            .post(format!("{base}/v1/jobs/list"))
            .json(&serde_json::json!({"scope": "org/http"}))
            .send()
            .await
            .unwrap();
        let text = res.text().await.unwrap();
        assert!(!text.contains("sensitive-body"), "summary leaked payload");

        let res = client
            .post(format!("{base}/v1/jobs/claim"))
            .json(&serde_json::json!({
                "worker": "w1", "scopes": ["org/http"], "kinds": ["reindex"]
            }))
            .send()
            .await
            .unwrap();
        let lease: serde_json::Value = res.json().await.unwrap();
        let fence = lease["fence"].as_u64().unwrap();

        let res = client
            .post(format!("{base}/v1/jobs/complete"))
            .json(&serde_json::json!({
                "id": job_id, "fence": fence + 7, "worker": "w1", "value": {}
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CONFLICT);
        let err: serde_json::Value = res.json().await.unwrap();
        assert_eq!(err["code"], "lease_lost");

        let res = client
            .post(format!("{base}/v1/jobs/complete"))
            .json(&serde_json::json!({
                "id": job_id, "fence": fence, "worker": "w1", "value": {"ok": true}
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let done: serde_json::Value = res.json().await.unwrap();
        assert_eq!(done["state"], "succeeded");
    }
}
