//! HTTP JSON API for Nomiso.

#![forbid(unsafe_code)]

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use nomiso_core::error::Error as CoreError;
use nomiso_core::ops::{
    ForgetRequest, PutRequest, ReadRequest, SearchQuery, SupersedeRequest, WriteResult,
};
use nomiso_core::types::{MemoryRecord, SearchHit};
use nomiso_service::NomisoClient;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::json;
use subtle::ConstantTimeEq;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;

/// Shared HTTP state.
#[derive(Clone)]
pub struct AppState {
    /// Memory client.
    pub client: NomisoClient,
    /// Optional API key (if set, required on all routes except health).
    pub api_key: Option<SecretString>,
}

/// Build the Axum router.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/put", post(put))
        .route("/v1/put_with_jobs", post(put_with_jobs))
        .route("/v1/supersede_with_jobs", post(supersede_with_jobs))
        .route("/v1/supersede", post(supersede))
        .route("/v1/search", post(search))
        .route("/v1/search_detailed", post(search_detailed))
        .route("/v1/read", post(read))
        .route("/v1/forget", post(forget))
        // Relationship plane (REL-001..004, RET-001 bounds).
        .route("/v1/entities", post(put_entity))
        .route("/v1/entities/get", post(get_entity))
        .route("/v1/entities/update", post(update_entity))
        .route("/v1/relationships", post(put_relationship))
        .route("/v1/relationships/update", post(update_relationship))
        .route("/v1/relationships/list", post(list_relationships))
        .route("/v1/traverse", post(traverse))
        // Embedding administration (MIG-004/005).
        .route("/v1/embed/state", post(embedding_state))
        .route("/v1/embed/attest", post(attest_embedding_identity))
        .route("/v1/embed/declare", post(declare_embedding_generation))
        .route("/v1/embed/stage", post(stage_embeddings))
        .route("/v1/embed/activate", post(activate_embedding_generation))
        .route("/v1/embed/provider_status", post(provider_status))
        // Durable job journal (JOB-001..007).
        .route("/v1/jobs", post(enqueue_job))
        .route("/v1/jobs/get", post(get_job))
        .route("/v1/jobs/list", post(list_jobs))
        .route("/v1/jobs/claim", post(claim_job))
        .route("/v1/jobs/renew", post(renew_job_lease))
        .route("/v1/jobs/checkpoint", post(checkpoint_job))
        .route("/v1/jobs/complete", post(complete_job))
        .route("/v1/jobs/fail", post(fail_job))
        .route("/v1/jobs/cancel", post(cancel_job))
        .route("/v1/jobs/supersede", post(supersede_job))
        .layer(TraceLayer::new_for_http())
        .layer(RequestBodyLimitLayer::new(256 * 1024))
        .with_state(Arc::new(state))
}

async fn health(State(state): State<Arc<AppState>>) -> Response {
    match state.client.health().await {
        Ok(()) => (StatusCode::OK, Json(json_ok("ok"))).into_response(),
        Err(e) => api_err(e),
    }
}

async fn put(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<PutRequest>,
) -> impl IntoResponse {
    if let Err(r) = authorize(&state, &headers) {
        return *r;
    }
    match state.client.put(body).await {
        Ok(w) => (StatusCode::OK, Json(w)).into_response(),
        Err(e) => api_err(e).into_response(),
    }
}

async fn put_with_jobs(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<PutWithJobsBody>,
) -> impl IntoResponse {
    if let Err(r) = authorize(&state, &headers) {
        return *r;
    }
    match state.client.put_with_jobs(body.put, body.jobs).await {
        Ok(w) => (StatusCode::OK, Json(w)).into_response(),
        Err(e) => api_err(e).into_response(),
    }
}

async fn supersede_with_jobs(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<SupersedeWithJobsBody>,
) -> impl IntoResponse {
    if let Err(r) = authorize(&state, &headers) {
        return *r;
    }
    match state
        .client
        .supersede_with_jobs(body.supersede, body.jobs)
        .await
    {
        Ok(w) => (StatusCode::OK, Json(w)).into_response(),
        Err(e) => api_err(e).into_response(),
    }
}

async fn supersede(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<SupersedeRequest>,
) -> impl IntoResponse {
    if let Err(r) = authorize(&state, &headers) {
        return *r;
    }
    match state.client.supersede(body).await {
        Ok(w) => (StatusCode::OK, Json(w)).into_response(),
        Err(e) => api_err(e).into_response(),
    }
}

async fn search(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<SearchQuery>,
) -> impl IntoResponse {
    if let Err(r) = authorize(&state, &headers) {
        return *r;
    }
    match state.client.search(body).await {
        Ok(hits) => (StatusCode::OK, Json(hits)).into_response(),
        Err(e) => api_err(e).into_response(),
    }
}

async fn search_detailed(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<SearchQuery>,
) -> impl IntoResponse {
    if let Err(r) = authorize(&state, &headers) {
        return *r;
    }
    match state.client.search_detailed(body).await {
        Ok(outcome) => (StatusCode::OK, Json(outcome)).into_response(),
        Err(e) => api_err(e).into_response(),
    }
}

async fn read(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<ReadRequest>,
) -> impl IntoResponse {
    if let Err(r) = authorize(&state, &headers) {
        return *r;
    }
    match state.client.read(body).await {
        Ok(rows) => (StatusCode::OK, Json(rows)).into_response(),
        Err(e) => api_err(e).into_response(),
    }
}

async fn forget(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<ForgetRequest>,
) -> impl IntoResponse {
    if let Err(r) = authorize(&state, &headers) {
        return *r;
    }
    match state.client.forget(body).await {
        Ok(()) => (StatusCode::OK, Json(json_ok("forgotten"))).into_response(),
        Err(e) => api_err(e).into_response(),
    }
}

// --- Relationship plane ---

async fn put_entity(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<nomiso_core::PutEntityRequest>,
) -> impl IntoResponse {
    if let Err(r) = authorize(&state, &headers) {
        return *r;
    }
    match state.client.put_entity(body).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => api_err(e).into_response(),
    }
}

async fn get_entity(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<ScopedIdBody>,
) -> impl IntoResponse {
    if let Err(r) = authorize(&state, &headers) {
        return *r;
    }
    match state.client.get_entity(&body.id, &body.scope).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => api_err(e).into_response(),
    }
}

async fn update_entity(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<nomiso_core::UpdateEntityRequest>,
) -> impl IntoResponse {
    if let Err(r) = authorize(&state, &headers) {
        return *r;
    }
    match state.client.update_entity(body).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => api_err(e).into_response(),
    }
}

async fn put_relationship(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<nomiso_core::PutRelationshipRequest>,
) -> impl IntoResponse {
    if let Err(r) = authorize(&state, &headers) {
        return *r;
    }
    match state.client.put_relationship(body).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => api_err(e).into_response(),
    }
}

async fn update_relationship(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<nomiso_core::UpdateRelationshipRequest>,
) -> impl IntoResponse {
    if let Err(r) = authorize(&state, &headers) {
        return *r;
    }
    match state.client.update_relationship(body).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => api_err(e).into_response(),
    }
}

async fn list_relationships(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<nomiso_core::ListRelationshipsRequest>,
) -> impl IntoResponse {
    if let Err(r) = authorize(&state, &headers) {
        return *r;
    }
    match state.client.list_relationships(body).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => api_err(e).into_response(),
    }
}

async fn traverse(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<nomiso_core::TraverseRequest>,
) -> impl IntoResponse {
    if let Err(r) = authorize(&state, &headers) {
        return *r;
    }
    match state.client.traverse(body).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => api_err(e).into_response(),
    }
}

// --- Embedding administration ---

async fn embedding_state(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(r) = authorize(&state, &headers) {
        return *r;
    }
    match state.client.embedding_state().await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => api_err(e).into_response(),
    }
}

async fn provider_status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(r) = authorize(&state, &headers) {
        return *r;
    }
    match state.client.provider_status().await {
        // null when no provider is configured — "no provider" is a status,
        // not an error, and never blocks metadata operations (ARCH-003).
        Ok(v) => (StatusCode::OK, Json(json!({ "provider": v }))).into_response(),
        Err(e) => api_err(e).into_response(),
    }
}

async fn attest_embedding_identity(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<nomiso_core::EmbeddingIdentity>,
) -> impl IntoResponse {
    if let Err(r) = authorize(&state, &headers) {
        return *r;
    }
    match state.client.attest_embedding_identity(body).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => api_err(e).into_response(),
    }
}

async fn declare_embedding_generation(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<nomiso_core::DeclareGenerationRequest>,
) -> impl IntoResponse {
    if let Err(r) = authorize(&state, &headers) {
        return *r;
    }
    match state.client.declare_embedding_generation(body).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => api_err(e).into_response(),
    }
}

async fn stage_embeddings(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<StageBody>,
) -> impl IntoResponse {
    if let Err(r) = authorize(&state, &headers) {
        return *r;
    }
    match state
        .client
        .stage_embeddings(body.generation, body.items)
        .await
    {
        Ok(n) => (StatusCode::OK, Json(json!({ "staged": n }))).into_response(),
        Err(e) => api_err(e).into_response(),
    }
}

async fn activate_embedding_generation(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<GenerationBody>,
) -> impl IntoResponse {
    if let Err(r) = authorize(&state, &headers) {
        return *r;
    }
    match state
        .client
        .activate_embedding_generation(body.generation)
        .await
    {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => api_err(e).into_response(),
    }
}

// --- Durable job journal ---

async fn enqueue_job(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<nomiso_core::EnqueueJobRequest>,
) -> impl IntoResponse {
    if let Err(r) = authorize(&state, &headers) {
        return *r;
    }
    match state.client.enqueue_job(body).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => api_err(e).into_response(),
    }
}

async fn get_job(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<ScopedIdBody>,
) -> impl IntoResponse {
    if let Err(r) = authorize(&state, &headers) {
        return *r;
    }
    match state.client.get_job(&body.scope, &body.id).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => api_err(e).into_response(),
    }
}

async fn list_jobs(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<nomiso_core::ListJobsRequest>,
) -> impl IntoResponse {
    if let Err(r) = authorize(&state, &headers) {
        return *r;
    }
    match state.client.list_jobs(body).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => api_err(e).into_response(),
    }
}

async fn claim_job(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<nomiso_core::ClaimJobRequest>,
) -> impl IntoResponse {
    if let Err(r) = authorize(&state, &headers) {
        return *r;
    }
    match state.client.claim_job(body).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => api_err(e).into_response(),
    }
}

async fn renew_job_lease(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<FencedBody>,
) -> impl IntoResponse {
    if let Err(r) = authorize(&state, &headers) {
        return *r;
    }
    match state
        .client
        .renew_job_lease(&body.id, body.fence, &body.worker)
        .await
    {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => api_err(e).into_response(),
    }
}

async fn checkpoint_job(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<JobValueBody>,
) -> impl IntoResponse {
    if let Err(r) = authorize(&state, &headers) {
        return *r;
    }
    match state
        .client
        .checkpoint_job(&body.id, body.fence, &body.worker, body.value)
        .await
    {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => api_err(e).into_response(),
    }
}

async fn complete_job(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<JobValueBody>,
) -> impl IntoResponse {
    if let Err(r) = authorize(&state, &headers) {
        return *r;
    }
    match state
        .client
        .complete_job(&body.id, body.fence, &body.worker, body.value)
        .await
    {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => api_err(e).into_response(),
    }
}

async fn fail_job(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<JobFailBody>,
) -> impl IntoResponse {
    if let Err(r) = authorize(&state, &headers) {
        return *r;
    }
    match state
        .client
        .fail_job(&body.id, body.fence, &body.worker, body.error)
        .await
    {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => api_err(e).into_response(),
    }
}

async fn cancel_job(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<JobReasonBody>,
) -> impl IntoResponse {
    if let Err(r) = authorize(&state, &headers) {
        return *r;
    }
    match state
        .client
        .cancel_job(&body.scope, &body.id, &body.reason)
        .await
    {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => api_err(e).into_response(),
    }
}

async fn supersede_job(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<JobSupersedeBody>,
) -> impl IntoResponse {
    if let Err(r) = authorize(&state, &headers) {
        return *r;
    }
    match state
        .client
        .supersede_job(&body.scope, &body.id, &body.replacement_id, &body.reason)
        .await
    {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => api_err(e).into_response(),
    }
}

// --- Shared request bodies for positional-parameter ops ---

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopedIdBody {
    scope: String,
    id: String,
}

/// Atomic write + durable job intents (JOB-001 commit-together).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PutWithJobsBody {
    put: PutRequest,
    #[serde(default)]
    jobs: Vec<nomiso_core::JobIntent>,
}

/// Atomic supersede + durable job intents (JOB-001).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SupersedeWithJobsBody {
    supersede: SupersedeRequest,
    #[serde(default)]
    jobs: Vec<nomiso_core::JobIntent>,
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
    items: Vec<nomiso_core::StagedEmbedding>,
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
    error: nomiso_core::JobError,
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

fn authorize(state: &AppState, headers: &HeaderMap) -> std::result::Result<(), Box<Response>> {
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
    if a.len() != b.len() || !bool::from(a.ct_eq(b)) {
        return Err(Box::new(
            (
                StatusCode::UNAUTHORIZED,
                Json(ErrorBody {
                    error: "unauthorized".into(),
                    code: "unauthorized".into(),
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
}

#[derive(Serialize)]
struct OkBody {
    status: String,
}

fn json_ok(s: &str) -> OkBody {
    OkBody {
        status: s.to_string(),
    }
}

fn api_err(e: CoreError) -> Response {
    let status = match &e {
        CoreError::InvalidOp(_) => StatusCode::BAD_REQUEST,
        CoreError::Conflict { .. }
        | CoreError::IdempotencyConflict
        | CoreError::IdempotencyUnavailable
        | CoreError::LeaseLost(_)
        | CoreError::StaleInput(_) => StatusCode::CONFLICT,
        CoreError::NotFound(_) => StatusCode::NOT_FOUND,
        CoreError::ScopeDenied(_) => StatusCode::FORBIDDEN,
        CoreError::DimensionMismatch { .. } => StatusCode::BAD_REQUEST,
        CoreError::PayloadTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
        CoreError::ProviderUnavailable(_)
        | CoreError::Store(_)
        | CoreError::IncompatibleStore(_) => StatusCode::SERVICE_UNAVAILABLE,
        CoreError::InvalidProviderResponse(_) => StatusCode::BAD_GATEWAY,
        CoreError::DeadlineExceeded => StatusCode::GATEWAY_TIMEOUT,
        // Caller-initiated abort — the closest standard status.
        CoreError::Cancelled => StatusCode::REQUEST_TIMEOUT,
        CoreError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (
        status,
        Json(ErrorBody {
            error: e.public_message(),
            code: e.code().into(),
        }),
    )
        .into_response()
}

// Keep OpenAPI-friendly re-exports for documentation consumers.
pub type PutBody = PutRequest;

pub type SearchBody = SearchQuery;
pub type SupersedeBody = SupersedeRequest;
pub type ReadBody = ReadRequest;
pub type ForgetBody = ForgetRequest;
pub type PutResponse = WriteResult;
pub type SearchResponse = Vec<SearchHit>;
pub type ReadResponse = Vec<MemoryRecord>;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use nomiso_store::StoreConfig;
    use tower::ServiceExt;

    #[tokio::test]
    async fn http_put_search_with_api_key() {
        let client = NomisoClient::connect(StoreConfig::memory_test(8))
            .await
            .unwrap();
        let app = router(AppState {
            client,
            api_key: Some(SecretString::from("test-key")),
        });

        let put_body = serde_json::json!({
            "scope": "org/http",
            "category": "semantic",
            "content": { "text": "HTTP prefers structured ops" }
        });
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/put")
                    .header("authorization", "Bearer test-key")
                    .header("content-type", "application/json")
                    .body(Body::from(put_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let search_body = serde_json::json!({
            "query": "structured ops",
            "scope": "org/http",
            "limit": 5
        });
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/search")
                    .header("authorization", "Bearer test-key")
                    .header("content-type", "application/json")
                    .body(Body::from(search_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let hits: Vec<SearchHit> = serde_json::from_slice(&bytes).unwrap();
        assert!(!hits.is_empty());
        assert!(hits[0].preview.contains("structured"));
    }

    #[tokio::test]
    async fn search_detailed_reports_expansion_stats() {
        let client = NomisoClient::connect(StoreConfig::memory_test(8))
            .await
            .unwrap();
        let app = router(AppState {
            client,
            api_key: Some(SecretString::from("test-key")),
        });

        for text in ["graph expansion seed", "graph expansion neighbor"] {
            let body = serde_json::json!({
                "scope": "org/http",
                "category": "semantic",
                "content": { "text": text }
            });
            let res = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/v1/put")
                        .header("authorization", "Bearer test-key")
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::OK);
        }

        let search_body = serde_json::json!({
            "query": "graph expansion",
            "scope": "org/http",
            "limit": 5,
            "graph_expand": { "max_depth": 1 }
        });
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/search_detailed")
                    .header("authorization", "Bearer test-key")
                    .header("content-type", "application/json")
                    .body(Body::from(search_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let out: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(!out["hits"].as_array().unwrap().is_empty());
        let exp = &out["stats"]["expansion"];
        assert_eq!(exp["seeds"].as_u64().unwrap(), 2);
        assert!(exp["elapsed_ms"].is_u64());
    }

    #[tokio::test]
    async fn provider_status_null_without_embedder() {
        // OPS-001: no provider configured → {"provider": null}, a status not
        // an error — metadata operations are never gated on provider config.
        let client = NomisoClient::connect(StoreConfig::memory_test(8))
            .await
            .unwrap();
        let app = router(AppState {
            client,
            api_key: Some(SecretString::from("test-key")),
        });
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/embed/provider_status")
                    .header("authorization", "Bearer test-key")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let out: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(out["provider"].is_null(), "{out}");
    }

    #[tokio::test]
    async fn rejects_bad_api_key() {
        let client = NomisoClient::connect(StoreConfig::memory_test(8))
            .await
            .unwrap();
        let app = router(AppState {
            client,
            api_key: Some(SecretString::from("secret")),
        });
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/search")
                    .header("authorization", "Bearer wrong")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"query":"x","scope":"org/a"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    fn authed(app: &Router, uri: &str, body: serde_json::Value) -> Request<Body> {
        let _ = app;
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("authorization", "Bearer test-key")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    #[tokio::test]
    async fn relationship_plane_over_http() {
        let client = NomisoClient::connect(StoreConfig::memory_test(8))
            .await
            .unwrap();
        let app = router(AppState {
            client,
            api_key: Some(SecretString::from("test-key")),
        });

        // Two entities in the same scope.
        let mut ids = vec![];
        for name in ["api", "db"] {
            let res = app
                .clone()
                .oneshot(authed(
                    &app,
                    "/v1/entities",
                    serde_json::json!({
                        "scope": "org/rel",
                        "kind": "service",
                        "name": name,
                    }),
                ))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::OK);
            let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
                .await
                .unwrap();
            let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            ids.push(v["id"].as_str().unwrap().to_string());
        }

        let res = app
            .clone()
            .oneshot(authed(
                &app,
                "/v1/relationships",
                serde_json::json!({
                    "scope": "org/rel",
                    "predicate": "depends_on",
                    "subject": { "kind": "entity", "id": ids[0] },
                    "object": { "kind": "entity", "id": ids[1] },
                }),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let res = app
            .clone()
            .oneshot(authed(
                &app,
                "/v1/relationships/list",
                serde_json::json!({ "scope": "org/rel" }),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        let rows: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(rows.as_array().unwrap().len(), 1);

        let res = app
            .clone()
            .oneshot(authed(
                &app,
                "/v1/traverse",
                serde_json::json!({
                    "scope": "org/rel",
                    "seeds": [{ "kind": "entity", "id": ids[1] }],
                    "direction": "both",
                }),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        let trav: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(trav["nodes"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn job_journal_over_http() {
        let client = NomisoClient::connect(StoreConfig::memory_test(8))
            .await
            .unwrap();
        let app = router(AppState {
            client,
            api_key: Some(SecretString::from("test-key")),
        });

        // Enqueue with a sensitive payload.
        let res = app
            .clone()
            .oneshot(authed(
                &app,
                "/v1/jobs",
                serde_json::json!({
                    "scope": "org/j",
                    "kind": "reindex",
                    "payload": { "secret": "sensitive-body" },
                    "budget": { "max_attempts": 3, "lease_ms": 5000 }
                }),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        let enq: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let job_id = enq["job"]["id"].as_str().unwrap().to_string();

        // Summaries never carry payload bodies (JOB-007).
        let res = app
            .clone()
            .oneshot(authed(
                &app,
                "/v1/jobs/list",
                serde_json::json!({ "scope": "org/j" }),
            ))
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(!text.contains("sensitive-body"), "summary leaked payload");
        let summaries: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(summaries[0].get("payload").is_none());

        // Claim → typed 409 on stale fence → complete under real fence.
        let res = app
            .clone()
            .oneshot(authed(
                &app,
                "/v1/jobs/claim",
                serde_json::json!({
                    "worker": "w1",
                    "scopes": ["org/j"],
                    "kinds": ["reindex"]
                }),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        let lease: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let fence = lease["fence"].as_u64().unwrap();

        let res = app
            .clone()
            .oneshot(authed(
                &app,
                "/v1/jobs/complete",
                serde_json::json!({
                    "id": job_id,
                    "fence": fence + 5,
                    "worker": "w1",
                    "value": {}
                }),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CONFLICT);
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        let err: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(err["code"], "lease_lost");

        let res = app
            .clone()
            .oneshot(authed(
                &app,
                "/v1/jobs/complete",
                serde_json::json!({
                    "id": job_id,
                    "fence": fence,
                    "worker": "w1",
                    "value": { "ok": true }
                }),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        let done: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(done["state"], "succeeded");

        // Scope isolation: a foreign claim grant sees nothing eligible.
        let res = app
            .clone()
            .oneshot(authed(
                &app,
                "/v1/jobs/claim",
                serde_json::json!({
                    "worker": "w2",
                    "scopes": ["org/other"],
                    "kinds": ["reindex"]
                }),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        let none_lease: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(none_lease.is_null());
    }

    #[tokio::test]
    async fn http_put_with_jobs_atomic() {
        let client = NomisoClient::connect(StoreConfig::memory_test(8))
            .await
            .unwrap();
        let app = router(AppState {
            client: client.clone(),
            api_key: Some(SecretString::from("test-key")),
        });

        // Bad intent (dangling input) must abort the canonical write.
        let bad = serde_json::json!({
            "put": {
                "scope": "org/pwj",
                "category": "semantic",
                "content": { "text": "must not commit" }
            },
            "jobs": [{
                "kind": "reindex",
                "inputs": [{ "kind": "memory", "id": "nonexistent", "revision": 1 }],
                "budget": { "max_attempts": 3, "lease_ms": 30000 }
            }]
        });
        let res = app
            .clone()
            .oneshot(authed(&app, "/v1/put_with_jobs", bad))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);

        // Good self-input intent commits write + job atomically.
        let good = serde_json::json!({
            "put": {
                "scope": "org/pwj",
                "category": "semantic",
                "content": { "text": "committed with job" }
            },
            "jobs": [{
                "kind": "reindex",
                "self_input": true,
                "budget": { "max_attempts": 3, "lease_ms": 30000 }
            }]
        });
        let res = app
            .clone()
            .oneshot(authed(&app, "/v1/put_with_jobs", good))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        let out: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(out["write"]["id"].is_string());
        assert_eq!(out["write"]["replayed"], false);
        let jobs = out["jobs"].as_array().unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0]["deduplicated"], false);

        // The aborted intent left no job row behind; only the good one exists.
        let list = serde_json::json!({ "scope": "org/pwj" });
        let res = app
            .clone()
            .oneshot(authed(&app, "/v1/jobs/list", list.clone()))
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        let rows: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(rows.as_array().unwrap().len(), 1);

        // Supersede + jobs: a bad intent aborts the supersede and the prior
        // stays open; a good intent commits successor + job together.
        let prior_id = out["write"]["id"].as_str().unwrap().to_string();
        let bad_sup = serde_json::json!({
            "supersede": {
                "prior_id": prior_id,
                "expected_version": 1,
                "new": {
                    "scope": "org/pwj",
                    "category": "semantic",
                    "content": { "text": "aborted successor" }
                }
            },
            "jobs": [{
                "kind": "reindex",
                "inputs": [{ "kind": "memory", "id": "nonexistent", "revision": 1 }],
                "budget": { "max_attempts": 3, "lease_ms": 30000 }
            }]
        });
        let res = app
            .clone()
            .oneshot(authed(&app, "/v1/supersede_with_jobs", bad_sup))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);

        let good_sup = serde_json::json!({
            "supersede": {
                "prior_id": prior_id,
                "expected_version": 1,
                "new": {
                    "scope": "org/pwj",
                    "category": "semantic",
                    "content": { "text": "committed successor" }
                }
            },
            "jobs": [{
                "kind": "reindex",
                "self_input": true,
                "budget": { "max_attempts": 3, "lease_ms": 30000 }
            }]
        });
        let res = app
            .clone()
            .oneshot(authed(&app, "/v1/supersede_with_jobs", good_sup))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        let sup: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(sup["write"]["id"].is_string());
        assert_eq!(sup["jobs"].as_array().unwrap().len(), 1);

        // Two jobs total in scope — the bad supersede intent added none.
        let res = app
            .clone()
            .oneshot(authed(&app, "/v1/jobs/list", list))
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        let rows: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(rows.as_array().unwrap().len(), 2);
    }
}
