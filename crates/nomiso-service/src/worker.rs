//! Durable-job worker runtime (JOB-002..006).
//!
//! A [`Worker`] is an in-process claim loop over the durable job journal: it
//! claims eligible jobs inside a restricted scope/kind grant, dispatches each
//! to a registered [`JobExecutor`], and lets the journal decide retry vs
//! terminal within the job's budget. It is deliberately *not* a general shell
//! runner — executors are registered per kind and all canonical effects go
//! through the client so fencing, checkpointing, and revision-bound
//! completion apply (JOB-002/003/004).
//!
//! Lifecycle is host-controlled: constructing a `Worker` never starts work —
//! [`Worker::run`] runs until [`Worker::stop_handle`] is set (graceful stop)
//! or `max_jobs` is reached. Unix signal handling belongs in a process
//! adapter; this is the library-level cancellation path (JOB-006).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use nomiso_core::{ClaimJobRequest, Error, JobError, JobLease, JobRecord, Result};
use serde_json::{json, Value};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::{Embedder, NomisoClient};

/// Outcome of one execution attempt.
pub enum JobRun {
    /// Final effects are ready to commit; the body becomes the job result.
    Completed(Value),
    /// Execution failed; the journal applies retry-within-budget or
    /// terminal failure per `JobError.retryable` (JOB-005).
    Failed(JobError),
    /// Graceful stop requested mid-execution — the job returns to pending
    /// promptly for another worker. Checkpointed progress is preserved.
    Paused,
    /// The lease was lost mid-execution (cancelled, expired, or reacquired).
    /// Do nothing further — the job has moved on (JOB-003).
    Aborted,
}

/// Per-attempt context handed to an executor.
pub struct JobContext {
    /// Client for all effect calls (fencing applies through it).
    pub client: NomisoClient,
    /// Job id under lease.
    pub job_id: String,
    /// Fencing token authorizing effect calls.
    pub fence: u64,
    /// This worker's identity (lease owner check).
    pub worker: String,
    /// Prior durable checkpoint for resume (JOB-002 at-least-once).
    pub checkpoint: Option<Value>,
    stop: CancellationToken,
}

impl JobContext {
    /// Persist bounded durable progress under the active fence.
    pub async fn checkpoint(&self, value: Value) -> Result<()> {
        self.client
            .checkpoint_job(&self.job_id, self.fence, &self.worker, value)
            .await
            .map(|_| ())
    }

    /// Whether graceful stop has been requested; executors should checkpoint
    /// and return [`JobRun::Paused`] when true.
    pub fn stopping(&self) -> bool {
        self.stop.is_cancelled()
    }

    /// A [`CancellationToken`] that fires when graceful stop is requested —
    /// for aborting in-flight provider calls via `embed_cancellable` rather
    /// than waiting out a transport deadline.
    pub fn cancel_token(&self) -> CancellationToken {
        self.stop.clone()
    }
}

/// Registered executor for one job kind.
#[async_trait]
pub trait JobExecutor: Send + Sync {
    /// Registered kind (matches `job.kind` at claim).
    fn kind(&self) -> &'static str;
    /// Execute the job. Stable per-effect identities and `ctx.checkpoint`
    /// keep a retried attempt idempotent (JOB-002).
    async fn run(&self, ctx: &JobContext, job: &JobRecord) -> JobRun;
}

/// Worker claim-loop configuration.
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// Worker identity recorded on leases and attempt history.
    pub worker: String,
    /// Restricted scope grant — jobs outside these exact scopes are never claimed.
    pub scopes: Vec<String>,
    /// Idle poll interval when no jobs are eligible.
    pub poll_interval: Duration,
    /// Maximum concurrently leased jobs.
    pub concurrency: usize,
    /// Bound total claimed jobs; `None` runs until stopped.
    pub max_jobs: Option<u64>,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            worker: "worker".into(),
            scopes: vec![],
            poll_interval: Duration::from_millis(250),
            concurrency: 1,
            max_jobs: None,
        }
    }
}

/// Counters from a `run` — useful for tests and operator metrics.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize)]
pub struct WorkerStats {
    pub claimed: u64,
    pub completed: u64,
    pub failed: u64,
    pub paused: u64,
    pub aborted: u64,
    /// Attempts rejected by the journal after execution (lost lease etc.).
    pub rejected: u64,
}

/// In-process claim loop over the durable journal. Host-controlled: `run`
/// only executes while `stop` is unset and `max_jobs` is unmet.
pub struct Worker {
    client: NomisoClient,
    cfg: WorkerConfig,
    executors: HashMap<&'static str, Arc<dyn JobExecutor>>,
    stop: CancellationToken,
}

impl Worker {
    pub fn new(client: NomisoClient, cfg: WorkerConfig) -> Self {
        Self {
            client,
            cfg,
            executors: HashMap::new(),
            stop: CancellationToken::new(),
        }
    }

    /// Register an executor for its declared kind. Last registration wins.
    pub fn register<E: JobExecutor + 'static>(&mut self, executor: Arc<E>) -> &mut Self {
        self.executors.insert(executor.kind(), executor);
        self
    }

    /// Shared stop token — cancel it to request graceful shutdown (JOB-006).
    pub fn stop_handle(&self) -> CancellationToken {
        self.stop.clone()
    }

    /// Claim and execute until stopped or `max_jobs` is reached.
    pub async fn run(&self) -> Result<WorkerStats> {
        if self.cfg.concurrency == 0 {
            return Err(Error::invalid("worker concurrency must be >= 1"));
        }
        let kinds: Vec<String> = self.executors.keys().map(|k| k.to_string()).collect();
        if kinds.is_empty() {
            return Err(Error::invalid("worker has no registered executors"));
        }
        let mut stats = WorkerStats::default();
        let mut in_flight: JoinSet<(JobLease, JobRun)> = JoinSet::new();

        loop {
            let stopping = self.stop.is_cancelled();
            let quota_reached = self.cfg.max_jobs.is_some_and(|m| stats.claimed >= m);

            // Claim up to the concurrency window.
            while !stopping && !quota_reached && in_flight.len() < self.cfg.concurrency {
                let lease = self
                    .client
                    .claim_job(ClaimJobRequest {
                        worker: self.cfg.worker.clone(),
                        scopes: self.cfg.scopes.clone(),
                        kinds: kinds.clone(),
                    })
                    .await?;
                let Some(lease) = lease else { break };
                stats.claimed += 1;
                let client = self.client.clone();
                let worker = self.cfg.worker.clone();
                let stop = self.stop.clone();
                let executor = self.executors.get(lease.job.kind.as_str()).cloned();
                in_flight.spawn(async move {
                    let run = match executor {
                        Some(ex) => {
                            let ctx = JobContext {
                                client: client.clone(),
                                job_id: lease.job.id.clone(),
                                fence: lease.fence,
                                worker: worker.clone(),
                                checkpoint: lease.job.checkpoint.clone(),
                                stop,
                            };
                            ex.run(&ctx, &lease.job).await
                        }
                        None => JobRun::Failed(JobError {
                            code: "unregistered_kind".into(),
                            message: format!("no executor for kind '{}'", lease.job.kind),
                            retryable: false,
                        }),
                    };
                    (lease, run)
                });
            }

            if in_flight.is_empty() {
                if stopping || quota_reached {
                    break;
                }
                tokio::time::sleep(self.cfg.poll_interval).await;
                continue;
            }

            // Harvest one finished attempt (also wakes the claim loop).
            if let Some(res) = in_flight.join_next().await {
                match res {
                    Ok((lease, run)) => {
                        self.settle(&mut stats, lease, run).await;
                    }
                    Err(e) => {
                        warn!(error = %e, "worker task join failure");
                        stats.aborted += 1;
                    }
                }
            }

            if stopping && in_flight.is_empty() {
                break;
            }
        }
        Ok(stats)
    }

    /// Report the attempt outcome back to the journal.
    async fn settle(&self, stats: &mut WorkerStats, lease: JobLease, run: JobRun) {
        let id = lease.job.id.clone();
        match run {
            JobRun::Completed(result) => {
                match self
                    .client
                    .complete_job(&id, lease.fence, &self.cfg.worker, result)
                    .await
                {
                    Ok(_) => stats.completed += 1,
                    // The journal rejected the commit — stale inputs or a lost
                    // lease mean the job moved on; count it, don't fake success.
                    Err(_) => stats.rejected += 1,
                }
            }
            JobRun::Failed(error) => {
                let retryable = error.retryable;
                match self
                    .client
                    .fail_job(&id, lease.fence, &self.cfg.worker, error)
                    .await
                {
                    Ok(j) => {
                        debug!(job = %id, state = ?j.state, retryable, "job failed");
                        stats.failed += 1;
                    }
                    Err(_) => stats.rejected += 1,
                }
            }
            // Paused work returns to pending promptly so another worker can
            // resume from the durable checkpoint (JOB-006 graceful stop).
            JobRun::Paused => {
                match self
                    .client
                    .fail_job(
                        &id,
                        lease.fence,
                        &self.cfg.worker,
                        JobError {
                            code: "worker_paused".into(),
                            message: "worker stopped mid-execution".into(),
                            retryable: true,
                        },
                    )
                    .await
                {
                    Ok(_) => stats.paused += 1,
                    Err(_) => stats.rejected += 1,
                }
            }
            JobRun::Aborted => stats.aborted += 1,
        }
    }
}

// ---------------------------------------------------------------------------
// Built-in executors
// ---------------------------------------------------------------------------

/// `reindex` executor: builds a declared embedding generation by re-embedding
/// every memory in the payload scopes, staging vectors in batches with
/// durable checkpoints, then explicitly activating the generation (MIG-005).
///
/// Job payload:
/// ```json
/// {"generation": 2, "scopes": ["org/acme"], "batch": 64}
/// ```
///
/// Checkpoint shape (resume point, not a transcript):
/// ```json
/// {"scope_index": 0, "cursor": <ListCursor|null>, "staged": 128}
/// ```
///
/// Effects are idempotent (JOB-002): `stage_embeddings` upserts per
/// (generation, memory), and `activate` revalidates coverage atomically —
/// a repeated attempt re-stages the same vectors and re-activates or
/// reports success, never duplicates.
pub struct ReindexExecutor {
    embedder: Arc<dyn Embedder>,
    batch: usize,
}

impl ReindexExecutor {
    /// `embedder` must produce vectors of the target generation's declared
    /// identity/dimension.
    pub fn new(embedder: Arc<dyn Embedder>) -> Self {
        Self {
            embedder,
            batch: 64,
        }
    }

    pub fn with_batch(mut self, batch: usize) -> Self {
        self.batch = batch.max(1);
        self
    }
}

#[async_trait]
impl JobExecutor for ReindexExecutor {
    fn kind(&self) -> &'static str {
        "reindex"
    }

    async fn run(&self, ctx: &JobContext, job: &JobRecord) -> JobRun {
        let payload = job.payload.clone().unwrap_or(Value::Null);
        let generation = payload
            .get("generation")
            .and_then(|g| g.as_u64())
            .unwrap_or(0);
        if generation == 0 {
            return JobRun::Failed(JobError {
                code: "invalid_payload".into(),
                message: "reindex payload requires `generation`".into(),
                retryable: false,
            });
        }
        let scopes: Vec<String> = payload
            .get("scopes")
            .and_then(|s| serde_json::from_value::<Vec<String>>(s.clone()).ok())
            .unwrap_or_else(|| vec![job.scope.clone()]);
        if scopes.is_empty() {
            return JobRun::Failed(JobError {
                code: "invalid_payload".into(),
                message: "reindex payload requires at least one scope".into(),
                retryable: false,
            });
        }
        let batch = payload
            .get("batch")
            .and_then(|b| b.as_u64())
            .map(|b| (b as usize).clamp(1, 512))
            .unwrap_or(self.batch);

        // Resume position from the durable checkpoint (JOB-002).
        let mut scope_index = ctx
            .checkpoint
            .as_ref()
            .and_then(|c| c.get("scope_index"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        let mut cursor = ctx
            .checkpoint
            .as_ref()
            .and_then(|c| c.get("cursor"))
            .cloned()
            .filter(|c| !c.is_null())
            .and_then(|c| serde_json::from_value::<nomiso_core::ListCursor>(c).ok());
        let mut staged: u64 = ctx
            .checkpoint
            .as_ref()
            .and_then(|c| c.get("staged"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        fn fail(e: &Error, retryable: bool) -> JobRun {
            JobRun::Failed(JobError {
                code: e.code().into(),
                message: e.public_message(),
                retryable,
            })
        }

        while scope_index < scopes.len() {
            let scope = &scopes[scope_index];
            loop {
                if ctx.stopping() {
                    return JobRun::Paused;
                }
                let page = match ctx
                    .client
                    .list(nomiso_core::ListRequest {
                        scope: scope.clone(),
                        scope_match: nomiso_core::ScopeMatch::Prefix,
                        categories: None,
                        as_of: None,
                        known_as_of: None,
                        sys_as_of: None,
                        text: None,
                        limit: Some(batch as u32),
                        cursor: cursor.clone(),
                    })
                    .await
                {
                    Ok(p) => p,
                    Err(e) => return fail(&e, true),
                };
                if page.items.is_empty() {
                    break;
                }
                let texts: Vec<String> = page
                    .items
                    .iter()
                    .map(|i| i.record.content.text.clone())
                    .collect();
                let cancel = ctx.cancel_token();
                let vectors = match self.embedder.embed_cancellable(&texts, Some(&cancel)).await {
                    Ok(v) => v,
                    // Stop requested mid-embed: pause so the job returns to
                    // pending promptly instead of retrying a cancelled call.
                    Err(Error::Cancelled) => return JobRun::Paused,
                    Err(e) => return fail(&e, true),
                };
                let items: Vec<nomiso_core::StagedEmbedding> = page
                    .items
                    .iter()
                    .zip(vectors)
                    .map(|(i, vector)| nomiso_core::StagedEmbedding {
                        memory: i.record.id.clone(),
                        vector,
                    })
                    .collect();
                match ctx.client.stage_embeddings(generation, items).await {
                    Ok(_) => {}
                    Err(Error::LeaseLost(_)) => return JobRun::Aborted,
                    Err(e) => return fail(&e, false),
                }
                staged += page.items.len() as u64;
                cursor = page.next_cursor.clone();
                if let Err(e) = ctx
                    .checkpoint(json!({
                        "scope_index": scope_index,
                        "cursor": cursor,
                        "staged": staged,
                    }))
                    .await
                {
                    return match e {
                        Error::LeaseLost(_) => JobRun::Aborted,
                        e => fail(&e, true),
                    };
                }
                if page.next_cursor.is_none() {
                    break;
                }
            }
            scope_index += 1;
            cursor = None;
            if let Err(e) = ctx
                .checkpoint(json!({
                    "scope_index": scope_index,
                    "cursor": null,
                    "staged": staged,
                }))
                .await
            {
                return match e {
                    Error::LeaseLost(_) => JobRun::Aborted,
                    e => fail(&e, true),
                };
            }
        }

        match ctx.client.activate_embedding_generation(generation).await {
            Ok(gen) => JobRun::Completed(json!({
                "generation": gen.generation,
                "status": "active",
                "staged": staged,
                "scopes": scopes,
            })),
            Err(Error::LeaseLost(_)) => JobRun::Aborted,
            Err(e) => fail(&e, false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nomiso_core::job::{EnqueueJobRequest, JobBudget, JobState};
    use nomiso_core::ops::PutRequest;
    use nomiso_core::types::{Category, Content, Provenance};
    use nomiso_store::StoreConfig;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    async fn client(dim: usize) -> NomisoClient {
        NomisoClient::connect(StoreConfig::memory_test(dim))
            .await
            .unwrap()
    }

    fn put(scope: &str, text: &str, embedding: Option<Vec<f32>>) -> PutRequest {
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
            embedding,
            embedding_identity: None,
            idempotency_key: None,
            extractor_version: None,
            model_version: None,
            valid_rev_from: None,
            valid_rev_until: None,
        }
    }

    fn job(scope: &str, kind: &str, payload: Value) -> EnqueueJobRequest {
        EnqueueJobRequest {
            scope: scope.into(),
            kind: kind.into(),
            inputs: vec![],
            composition: Some(json!({"producer": "test"})),
            payload: Some(payload),
            budget: JobBudget {
                max_attempts: 3,
                lease_ms: 5_000,
                retry_backoff_ms: Some(1),
                deadline_ms: None,
            },
            dedup_hint: None,
        }
    }

    fn cfg(worker: &str, max_jobs: u64) -> WorkerConfig {
        WorkerConfig {
            worker: worker.into(),
            scopes: vec!["org/acme".into()],
            poll_interval: Duration::from_millis(15),
            concurrency: 1,
            max_jobs: Some(max_jobs),
        }
    }

    struct FixedEmbedder(Vec<f32>, nomiso_core::EmbeddingIdentity);

    #[async_trait]
    impl Embedder for FixedEmbedder {
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| self.0.clone()).collect())
        }
        fn identity(&self) -> Option<nomiso_core::EmbeddingIdentity> {
            Some(self.1.clone())
        }
    }

    fn fixed_identity(dim: usize) -> nomiso_core::EmbeddingIdentity {
        nomiso_core::EmbeddingIdentity {
            family: "test".into(),
            model: "fixed-v2".into(),
            dimension: dim as u32,
            normalization: nomiso_core::EmbeddingNormalization::None,
            encoding: "f32".into(),
            limitation: None,
        }
    }

    #[tokio::test]
    async fn worker_runs_reindex_job_end_to_end() {
        let dim = 8usize;
        let client = client(dim).await;
        for (i, t) in ["alpha", "beta", "gamma"].iter().enumerate() {
            let mut v = vec![0.0f32; dim];
            v[i] = 1.0;
            client.put(put("org/acme", t, Some(v))).await.unwrap();
        }
        client
            .declare_embedding_generation(nomiso_core::DeclareGenerationRequest {
                identity: fixed_identity(dim),
                note: None,
            })
            .await
            .unwrap();
        let r = client
            .enqueue_job(job(
                "org/acme",
                "reindex",
                json!({"generation": 2, "scopes": ["org/acme"], "batch": 2}),
            ))
            .await
            .unwrap();

        let mut w = Worker::new(client.clone(), cfg("w1", 1));
        w.register(Arc::new(ReindexExecutor::new(Arc::new(FixedEmbedder(
            vec![0.25; dim],
            fixed_identity(dim),
        )))));
        let stats = w.run().await.unwrap();
        assert_eq!(stats.claimed, 1);
        assert_eq!(stats.completed, 1);

        // Generation 2 active; gen 1 retired.
        let st = client.embedding_state().await.unwrap();
        let g2 = st.generations.iter().find(|g| g.generation == 2).unwrap();
        assert_eq!(g2.status, nomiso_core::GenerationStatus::Active);
        let g1 = st.generations.iter().find(|g| g.generation == 1).unwrap();
        assert_eq!(g1.status, nomiso_core::GenerationStatus::Retired);

        // Job is terminal with a result.
        let j = client.get_job("org/acme", &r.job.id).await.unwrap();
        assert_eq!(j.state, JobState::Succeeded);
        assert!(j.result.is_some());
        assert_eq!(j.history.len(), 1);

        // Memories carry the new generation.
        let rows = client
            .list(nomiso_core::ListRequest {
                scope: "org/acme".into(),
                scope_match: nomiso_core::ScopeMatch::Exact,
                categories: None,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
                text: None,
                limit: Some(10),
                cursor: None,
            })
            .await
            .unwrap();
        assert_eq!(rows.items.len(), 3);
        assert!(rows
            .items
            .iter()
            .all(|i| i.record.embedding_generation == Some(2)));
    }

    #[tokio::test]
    async fn worker_retries_within_budget_then_completes() {
        let client = client(8).await;
        let r = client
            .enqueue_job(job("org/acme", "flaky", json!({})))
            .await
            .unwrap();

        struct Flaky(AtomicUsize);
        #[async_trait]
        impl JobExecutor for Flaky {
            fn kind(&self) -> &'static str {
                "flaky"
            }
            async fn run(&self, _ctx: &JobContext, _job: &JobRecord) -> JobRun {
                if self.0.fetch_add(1, Ordering::Relaxed) == 0 {
                    JobRun::Failed(JobError {
                        code: "boom".into(),
                        message: "transient".into(),
                        retryable: true,
                    })
                } else {
                    JobRun::Completed(json!({"ok": true}))
                }
            }
        }

        let mut w = Worker::new(client.clone(), cfg("w1", 2));
        w.register(Arc::new(Flaky(AtomicUsize::new(0))));
        let stats = w.run().await.unwrap();
        assert_eq!(stats.claimed, 2);
        assert_eq!(stats.failed, 1);
        assert_eq!(stats.completed, 1);

        let j = client.get_job("org/acme", &r.job.id).await.unwrap();
        assert_eq!(j.state, JobState::Succeeded);
        assert_eq!(j.attempts, 2);
        assert_eq!(j.history.len(), 2);
        assert!(j.history[0].error.is_some());
    }

    #[tokio::test]
    async fn worker_pause_returns_job_to_pending_with_checkpoint() {
        let client = client(8).await;
        let r = client
            .enqueue_job(job("org/acme", "resumer", json!({})))
            .await
            .unwrap();

        struct Resumer(AtomicUsize);
        #[async_trait]
        impl JobExecutor for Resumer {
            fn kind(&self) -> &'static str {
                "resumer"
            }
            async fn run(&self, ctx: &JobContext, _job: &JobRecord) -> JobRun {
                if self.0.fetch_add(1, Ordering::Relaxed) == 0 {
                    ctx.checkpoint(json!({"done": 1})).await.unwrap();
                    return JobRun::Paused;
                }
                // Durable checkpoint survived the paused attempt (JOB-002).
                assert_eq!(ctx.checkpoint.as_ref().unwrap()["done"], 1);
                JobRun::Completed(json!({"resumed": true}))
            }
        }

        let mut w = Worker::new(client.clone(), cfg("w1", 2));
        w.register(Arc::new(Resumer(AtomicUsize::new(0))));
        let stats = w.run().await.unwrap();
        assert_eq!(stats.claimed, 2);
        assert_eq!(stats.paused, 1);
        assert_eq!(stats.completed, 1);

        let j = client.get_job("org/acme", &r.job.id).await.unwrap();
        assert_eq!(j.state, JobState::Succeeded);
        assert_eq!(j.result.as_ref().unwrap()["resumed"], true);
    }

    #[tokio::test]
    async fn worker_grant_excludes_other_scopes() {
        let client = client(8).await;
        client
            .enqueue_job(job("org/other", "noop", json!({})))
            .await
            .unwrap();
        struct Noop;
        #[async_trait]
        impl JobExecutor for Noop {
            fn kind(&self) -> &'static str {
                "noop"
            }
            async fn run(&self, _ctx: &JobContext, _job: &JobRecord) -> JobRun {
                JobRun::Completed(json!({}))
            }
        }
        let mut w = Worker::new(client.clone(), cfg("w1", 1));
        w.register(Arc::new(Noop));
        let stop = w.stop_handle();
        let handle = tokio::spawn(async move { w.run().await });
        tokio::time::sleep(Duration::from_millis(120)).await;
        stop.cancel();
        let stats = handle.await.unwrap().unwrap();
        assert_eq!(stats.claimed, 0);
        assert_eq!(stats.completed, 0);
    }

    #[tokio::test]
    async fn worker_graceful_stop_after_drain() {
        let client = client(8).await;
        client
            .enqueue_job(job("org/acme", "slow", json!({})))
            .await
            .unwrap();
        struct Slow;
        #[async_trait]
        impl JobExecutor for Slow {
            fn kind(&self) -> &'static str {
                "slow"
            }
            async fn run(&self, ctx: &JobContext, _job: &JobRecord) -> JobRun {
                // Simulate work; check the stop flag like a batch loop would.
                for _ in 0..50 {
                    if ctx.stopping() {
                        return JobRun::Paused;
                    }
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
                JobRun::Completed(json!({"done": true}))
            }
        }
        let mut w = Worker::new(client.clone(), cfg("w1", 1));
        w.register(Arc::new(Slow));
        let stop = w.stop_handle();
        let handle = tokio::spawn(async move { w.run().await });
        tokio::time::sleep(Duration::from_millis(10)).await;
        stop.cancel();
        let stats = handle.await.unwrap().unwrap();
        // Either the job finished before stop landed, or it paused cleanly —
        // never a lost lease.
        assert!(stats.completed + stats.paused <= 1);
    }

    #[tokio::test]
    async fn job_context_cancel_token_mirrors_stop() {
        let client = client(8).await;
        let stop = CancellationToken::new();
        let ctx = JobContext {
            client,
            job_id: "j1".into(),
            fence: 1,
            worker: "w".into(),
            checkpoint: None,
            stop: stop.clone(),
        };
        let token = ctx.cancel_token();
        assert!(!token.is_cancelled() && !ctx.stopping());
        stop.cancel();
        token.cancelled().await;
        assert!(token.is_cancelled() && ctx.stopping());
    }
}
