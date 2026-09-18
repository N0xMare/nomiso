//! Vegapunk — opinionated agent memory system on the Nomiso plane.
//!
//! - **Nomiso** owns storage physics (SurrealDB only).
//! - **Writer plane** (BYOM): extract → typed ops → commit.
//! - **Reader plane** (BYOM rewrite optional): hard multi-pass recall → pack → host injects.
//! - Core paths work **without** an LLM; models attach via traits.

#![forbid(unsafe_code)]

mod eval_coding;
mod eval_coding_system;
mod eval_offline;
mod profile;

// Reusable mechanics live in `nomiso-memory` (T3 toolkit); Vegapunk supplies
// profile policy (`ProfilePolicy` → `MemoryPolicy`) and product composition.
// Module re-exports keep `vegapunk::writer::…` / `crate::error::…` paths working.
pub use nomiso_memory::{
    artifact, checkpoint, cli_writer, context, error, llm, reader, recall, remember, sleep,
    trace_emit, types, writer,
};

pub use nomiso_memory::artifact::{BlobConfig, StoreArtifactInput, StoreArtifactJson};
pub use nomiso_memory::checkpoint::CheckpointOutcome;
pub use nomiso_memory::cli_writer::{
    CliChatWriter, CliQueryRewriter, REWRITER_SYSTEM_PROMPT, WRITER_SYSTEM_PROMPT,
};
pub use nomiso_memory::context::{
    prepare_context, record_insertion, BlockProvenance, BudgetUsage, CandidateRecord,
    CompositionIdentity, ContextBlock, ContextBudget, ContextProposal, DegradationPolicy, Effort,
    ExclusionReason, FollowUp, InsertedBlock, InsertionAck, InsertionReceipt, InventoryItem,
    PrepareContextRequest, ProposalStatus, SelectionManifest, TokenMethod,
};
pub use nomiso_memory::error::{Error, Result};
pub use nomiso_memory::llm::{
    parse_json_from_model, parse_writer_ops_from_model, CompletionRequest, CompletionResponse,
    LlmCompletion, MockLlm,
};
pub use nomiso_memory::reader::{
    pack_context, ContextCard, ContextPack, HardRecallOptions, HardRecallResult, QueryRewriter,
    RuleQueryRewriter,
};
pub use nomiso_memory::recall::{parse_timestamp, EnumerateOptions, RecallOptions};
pub use nomiso_memory::remember::RememberInput;
pub use nomiso_memory::sleep::{
    sleep_pass, SleepApplied, SleepOptions, SleepProposal, SleepReport,
};
pub use nomiso_memory::trace_emit::{
    emit_apply_ops, emit_hard_recall, emit_inject, emit_outcome, emit_outcome_attributed,
    emit_remember, new_trace_id, Evaluator,
};
pub use nomiso_memory::types::{
    ApplyOpsReport, CheckpointInput, CompactionIngest, CompactionReport, RememberOutcome,
};
#[allow(deprecated)]
pub use nomiso_memory::writer::apply_ops_atomic;
pub use nomiso_memory::writer::{
    apply_ops, op_scope, preflight_ops, ApplyOpOutcome, ApplyResult, MemoryWriter, RuleWriter,
    WriteEpisode, WriterOp,
};

#[cfg(feature = "cli-llm")]
mod cli_llm;

pub use eval_coding::{
    episode_text, extract_gold_ops, run_gold_apply, run_live_extract, run_live_from_dir,
    run_rule_extract, CodingSkillReport, SkillExtractor, SkillRow,
};
pub use eval_coding_system::{
    run_coding_system, run_system_from_dir, run_system_grok, skip_system, CodingSystemReport,
    SystemProbeRow,
};

#[cfg(feature = "cli-llm")]
pub use eval_coding::run_grok_extract;
pub use eval_offline::{
    load_suite_json, load_write_jsonl, run_skill_suite, skill_suite_c0, skill_suite_c0_case_names,
    validate_ops_json_batch, CaseResult, HardRecallCase, SkillEvalSuite, SkillSuiteResult,
    WriteCase,
};
pub use nomiso::{TraceByMemory, TraceEventKind, TraceOutcome, TracesByMemoryRequest};
pub use nomiso_memory::MemoryPolicy;
pub use profile::{Profile, ProfilePolicy};

#[cfg(feature = "cli-llm")]
pub use cli_llm::{CliBackendKind, CliLlm, CliLlmConfig};

use std::sync::Arc;

use nomiso::{Embedder, FindCandidatesRequest, MemoryId, NomisoClient, SearchHit, StoreConfig};
use nomiso_blob::BlobStore;
use tracing::instrument;

/// Vegapunk system handle: policy + Nomiso client + optional BYOM writer.
#[derive(Clone)]
pub struct Vegapunk {
    client: NomisoClient,
    profile: Profile,
    writer: Option<Arc<dyn MemoryWriter>>,
    rewriter: Arc<dyn QueryRewriter>,
    /// Optional host correlation (stamped on emitted traces).
    session_id: Option<String>,
    turn_id: Option<String>,
    blob: Option<Arc<dyn BlobStore>>,
}

impl Vegapunk {
    /// Connect with an existing Nomiso client.
    pub fn new(client: NomisoClient) -> Self {
        Self {
            client,
            profile: Profile::CodingAgent,
            writer: None,
            rewriter: Arc::new(RuleQueryRewriter),
            session_id: None,
            turn_id: None,
            blob: None,
        }
    }

    /// Connect to Surreal via store config (migrates schema).
    pub async fn connect(config: StoreConfig) -> Result<Self> {
        let client = NomisoClient::connect(config).await?;
        Ok(Self::new(client))
    }

    /// In-memory Nomiso store for tests/demos.
    pub async fn connect_memory(embedding_dim: usize) -> Result<Self> {
        Self::connect(StoreConfig::memory_test(embedding_dim)).await
    }

    /// Set policy profile (default: CodingAgent).
    pub fn with_profile(mut self, profile: Profile) -> Self {
        self.profile = profile;
        self
    }

    /// Attach a Nomiso embedder (vectors on write when not supplied).
    pub fn with_embedder(mut self, embedder: Arc<dyn Embedder>) -> Self {
        self.client = self.client.with_embedder(embedder);
        self
    }

    /// Attach a BYOM memory writer (extract → ops).
    pub fn with_writer(mut self, writer: Arc<dyn MemoryWriter>) -> Self {
        self.writer = Some(writer);
        self
    }

    /// Attach a blob store for [`Self::store_artifact`].
    pub fn with_blob_store(mut self, store: Arc<dyn BlobStore>) -> Self {
        self.blob = Some(store);
        self
    }

    /// Open-validity priors for Put vs Supersede.
    pub async fn find_candidates(
        &self,
        scope: &str,
        query: &str,
        limit: Option<u32>,
    ) -> Result<Vec<SearchHit>> {
        Ok(self
            .client
            .find_candidates(FindCandidatesRequest {
                query: query.into(),
                scope: scope.into(),
                scope_match: nomiso::ScopeMatch::Exact,
                categories: None,
                limit,
                embedding: None,
            })
            .await?)
    }

    /// Bytes first, then artifact metadata. Requires [`Self::with_blob_store`].
    pub async fn store_artifact(
        &self,
        input: StoreArtifactInput,
    ) -> Result<nomiso::ArtifactRecord> {
        let Some(blobs) = &self.blob else {
            return Err(Error::invalid(
                "no blob store; attach with with_blob_store \
                 (CLI attaches from VEGAPUNK_BLOB_ROOT / .nomiso-blobs)",
            ));
        };
        artifact::store_artifact(&self.client, blobs.as_ref(), input).await
    }

    /// Compaction hook: extract durable lines → prefix-preserving apply; checkpoint only if every op is `Ok`.
    ///
    /// A checkpoint `Err` after a successful apply does not fail this call:
    /// apply outcomes stay on the report and [`CompactionReport::checkpoint_error`]
    /// records the checkpoint failure.
    pub async fn ingest_compaction(&self, input: CompactionIngest) -> Result<CompactionReport> {
        let writer: Arc<dyn MemoryWriter> =
            self.writer.clone().unwrap_or_else(|| Arc::new(RuleWriter));
        let episode = WriteEpisode {
            scope: input.scope.clone(),
            text: input.transcript,
            source: Some("vegapunk:compaction".into()),
        };
        let ops = writer.extract(&episode).await?;
        let outcomes = apply_ops(&self.client, self.profile.policy(), &input.scope, &ops).await?;
        let apply_ok = !outcomes.is_empty() && outcomes.iter().all(|o| o.is_ok());
        let tid = trace_emit::new_trace_id();
        let emit_ok = trace_emit::emit_apply_ops(
            &self.client,
            &input.scope,
            &tid,
            self.session_id.as_deref(),
            self.turn_id.as_deref(),
            &outcomes,
        )
        .await
        .is_ok();
        let apply = ApplyOpsReport {
            outcomes,
            trace_id: emit_ok.then_some(tid),
        };
        let (checkpoint, checkpoint_error) = if apply_ok {
            if let Some(summary) = input.summary {
                match self
                    .checkpoint(CheckpointInput {
                        scope: input.scope,
                        summary,
                        force: false,
                    })
                    .await
                {
                    Ok(c) => (Some(c), None),
                    Err(e) => (None, Some(e.to_string())),
                }
            } else {
                (None, None)
            }
        } else {
            (None, None)
        };
        Ok(CompactionReport {
            apply,
            checkpoint,
            checkpoint_error,
        })
    }

    /// Attach a query rewriter for hard-recall (default: rule rewriter).
    pub fn with_rewriter(mut self, rewriter: Arc<dyn QueryRewriter>) -> Self {
        self.rewriter = rewriter;
        self
    }

    /// Stamp session/turn on subsequent trace emits (host correlation).
    pub fn with_correlation(mut self, session_id: Option<String>, turn_id: Option<String>) -> Self {
        self.session_id = nonempty_corr(session_id);
        self.turn_id = nonempty_corr(turn_id);
        self
    }

    /// Overlay per-call session/turn, keeping existing handle values when the overlay is empty.
    pub fn overlay_correlation(&self, session_id: Option<String>, turn_id: Option<String>) -> Self {
        self.clone().with_correlation(
            nonempty_corr(session_id).or_else(|| self.session_id.clone()),
            nonempty_corr(turn_id).or_else(|| self.turn_id.clone()),
        )
    }

    /// Borrow underlying Nomiso client.
    pub fn client(&self) -> &NomisoClient {
        &self.client
    }

    pub fn profile(&self) -> Profile {
        self.profile
    }

    pub fn policy(&self) -> ProfilePolicy {
        self.profile.policy()
    }

    pub async fn health(&self) -> Result<()> {
        self.client.health().await.map_err(Error::from)
    }

    /// Store one memory under profile policy.
    #[instrument(skip(self, input))]
    pub async fn remember(&self, input: RememberInput) -> Result<RememberOutcome> {
        remember::remember(&self.client, self.profile.policy(), input).await
    }

    /// Supersede a prior fact (product API; plane physics via Nomiso).
    #[instrument(skip(self, new))]
    pub async fn supersede(
        &self,
        prior_id: MemoryId,
        expected_version: u64,
        new: RememberInput,
    ) -> Result<RememberOutcome> {
        writer::supersede_remember(
            &self.client,
            self.profile.policy(),
            &prior_id,
            expected_version,
            new,
        )
        .await
    }

    /// Run a BYOM writer on an episode and commit via [`apply_ops`].
    ///
    /// Requires [`with_writer`]. Without a writer, returns an error.
    /// Ops are pinned to `episode.scope` (lib-level active-scope pin).
    #[instrument(skip(self, episode))]
    pub async fn write_episode(&self, episode: WriteEpisode) -> Result<Vec<ApplyOpOutcome>> {
        let Some(w) = &self.writer else {
            return Err(Error::invalid(
                "no MemoryWriter attached; use with_writer(RuleWriter) or a BYOM impl",
            ));
        };
        writer::write_episode(&self.client, self.profile.policy(), w.as_ref(), &episode).await
    }

    /// Apply pre-built writer ops (e.g. from an external skill JSON array).
    ///
    /// `active_scope` is required (lib-level pin for CLI/HTTP/MCP).
    /// Returns per-op outcomes plus a write `trace_id` when emit succeeds.
    /// Inspect `outcomes` for mid-batch failures.
    pub async fn apply_writer_ops(
        &self,
        active_scope: &str,
        ops: &[WriterOp],
    ) -> Result<ApplyOpsReport> {
        let outcomes = apply_ops(&self.client, self.profile.policy(), active_scope, ops).await?;
        let tid = trace_emit::new_trace_id();
        // Best-effort plane trace; failures do not fail the apply.
        let emit_ok = trace_emit::emit_apply_ops(
            &self.client,
            active_scope,
            &tid,
            self.session_id.as_deref(),
            self.turn_id.as_deref(),
            &outcomes,
        )
        .await
        .is_ok();
        Ok(ApplyOpsReport {
            outcomes,
            trace_id: emit_ok.then_some(tid),
        })
    }

    /// Hybrid recall (single pass; never auto-injects).
    #[instrument(skip(self))]
    pub async fn recall(&self, scope: &str, query: &str) -> Result<Vec<SearchHit>> {
        self.recall_with(
            scope,
            query,
            RecallOptions::from_policy(self.profile.policy()),
        )
        .await
    }

    pub async fn recall_with(
        &self,
        scope: &str,
        query: &str,
        opts: RecallOptions,
    ) -> Result<Vec<SearchHit>> {
        recall::recall(&self.client, scope, query, opts).await
    }

    /// Multi-pass hard recall (reader plane). Host decides inject.
    #[instrument(skip(self))]
    pub async fn hard_recall(&self, scope: &str, query: &str) -> Result<HardRecallResult> {
        self.hard_recall_with(
            scope,
            query,
            HardRecallOptions::from_policy(self.profile.policy()),
        )
        .await
    }

    /// Multi-pass hard recall with explicit options (min_score floor, pass limit, …).
    pub async fn hard_recall_with(
        &self,
        scope: &str,
        query: &str,
        opts: HardRecallOptions,
    ) -> Result<HardRecallResult> {
        reader::hard_recall(&self.client, scope, query, opts, self.rewriter.as_ref()).await
    }

    /// Hard recall + context pack (still **not** auto-injected).
    ///
    /// Emits plane `search` + `pack` trace events under a new `trace_id` (returned
    /// on [`HardRecallResult::trace_id`] when present).
    pub async fn hard_recall_pack(
        &self,
        scope: &str,
        query: &str,
    ) -> Result<(HardRecallResult, ContextPack)> {
        self.hard_recall_pack_with(
            scope,
            query,
            HardRecallOptions::from_policy(self.profile.policy()),
        )
        .await
    }

    /// Hard recall + pack with explicit options (score-floor abstention, etc.).
    pub async fn hard_recall_pack_with(
        &self,
        scope: &str,
        query: &str,
        opts: HardRecallOptions,
    ) -> Result<(HardRecallResult, ContextPack)> {
        let policy = self.profile.policy();
        let max_cards = if policy.allow_soft_inject {
            policy.soft_inject_max_cards as usize
        } else {
            policy.default_recall_limit as usize
        };
        let (mut hr, pack) = reader::hard_recall_pack(
            &self.client,
            scope,
            query,
            opts,
            self.rewriter.as_ref(),
            max_cards.max(1),
            4000,
        )
        .await?;
        let tid = trace_emit::new_trace_id();
        // Best-effort plane trace; recall/pack still return if emit fails.
        if trace_emit::emit_hard_recall(
            &self.client,
            scope,
            &tid,
            self.session_id.as_deref(),
            self.turn_id.as_deref(),
            &hr,
            Some(&pack),
        )
        .await
        .is_ok()
        {
            hr.trace_id = Some(tid);
        }
        Ok((hr, pack))
    }

    /// Attach a host-reported outcome to an existing trace.
    pub async fn record_trace_outcome(
        &self,
        scope: &str,
        trace_id: &str,
        outcome: TraceOutcome,
        note: Option<&str>,
    ) -> Result<()> {
        self.record_trace_outcome_attributed(
            scope,
            trace_id,
            outcome,
            trace_emit::Evaluator::Host,
            note,
        )
        .await
    }

    /// Attach an attributed outcome — distinguishes host feedback, model
    /// judgment, and observed execution results (CTX-010).
    pub async fn record_trace_outcome_attributed(
        &self,
        scope: &str,
        trace_id: &str,
        outcome: TraceOutcome,
        evaluator: trace_emit::Evaluator,
        note: Option<&str>,
    ) -> Result<()> {
        trace_emit::emit_outcome_attributed(
            &self.client,
            scope,
            trace_id,
            outcome,
            trace_emit::OutcomeMeta {
                evaluator,
                note,
                session_id: self.session_id.as_deref(),
                turn_id: self.turn_id.as_deref(),
            },
        )
        .await
    }

    /// Host reports that a pack was injected (never auto-injected by Vegapunk).
    pub async fn record_inject(
        &self,
        scope: &str,
        trace_id: &str,
        memory_ids: &[nomiso::MemoryId],
        note: Option<&str>,
    ) -> Result<()> {
        if memory_ids.is_empty() {
            return Err(Error::invalid(
                "trace-inject requires at least one memory id",
            ));
        }
        trace_emit::emit_inject(
            &self.client,
            scope,
            trace_id,
            memory_ids,
            note,
            self.session_id.as_deref(),
            self.turn_id.as_deref(),
        )
        .await
    }

    /// Load a plane trace bundle (events for `trace_id` under scope).
    pub async fn get_trace(&self, scope: &str, trace_id: &str) -> Result<nomiso::TraceBundle> {
        Ok(self.client.get_trace(trace_id, scope).await?)
    }

    /// List memories under scope (optional text filter). Never auto-injects.
    pub async fn list(
        &self,
        scope: &str,
        text: Option<&str>,
        limit: Option<u32>,
    ) -> Result<nomiso::ListResponse> {
        self.list_with(
            scope,
            EnumerateOptions {
                text: text.map(str::to_string),
                limit,
                ..Default::default()
            },
        )
        .await
    }

    /// List with optional tri-temporal lenses (plane `as_of` / `known_as_of` / `sys_as_of`).
    pub async fn list_with(
        &self,
        scope: &str,
        opts: EnumerateOptions,
    ) -> Result<nomiso::ListResponse> {
        Ok(self
            .client
            .list(nomiso::ListRequest {
                scope: scope.into(),
                scope_match: Default::default(),
                categories: opts.categories,
                as_of: opts.as_of,
                known_as_of: opts.known_as_of,
                sys_as_of: opts.sys_as_of,
                text: opts.text,
                limit: opts.limit,
                cursor: opts.cursor,
            })
            .await?)
    }

    /// Count memories under scope (optional text filter).
    pub async fn count(&self, scope: &str, text: Option<&str>) -> Result<u64> {
        self.count_with(
            scope,
            EnumerateOptions {
                text: text.map(str::to_string),
                ..Default::default()
            },
        )
        .await
    }

    /// Count with optional tri-temporal lenses.
    pub async fn count_with(&self, scope: &str, opts: EnumerateOptions) -> Result<u64> {
        if opts.cursor.is_some() {
            return Err(Error::invalid("cursor is not supported for count"));
        }
        Ok(self
            .client
            .count(nomiso::CountRequest {
                scope: scope.into(),
                scope_match: Default::default(),
                categories: opts.categories,
                as_of: opts.as_of,
                known_as_of: opts.known_as_of,
                sys_as_of: opts.sys_as_of,
                text: opts.text,
            })
            .await?)
    }

    /// Inventory traces in a scope (get-by-id remains `get_trace`).
    pub async fn list_traces(
        &self,
        scope: &str,
        opts: nomiso::ListTracesRequest,
    ) -> Result<Vec<nomiso::TraceSummary>> {
        let mut req = opts;
        req.scope = scope.into();
        Ok(self.client.list_traces(req).await?)
    }

    /// Traces that referenced a memory id, with latest host outcome if any.
    pub async fn list_traces_for_memory(
        &self,
        scope: &str,
        memory_id: &nomiso::MemoryId,
        opts: nomiso::TracesByMemoryRequest,
    ) -> Result<Vec<nomiso::TraceByMemory>> {
        let mut req = opts;
        req.scope = scope.into();
        req.memory_id = memory_id.clone();
        Ok(self.client.list_traces_for_memory(req).await?)
    }

    /// Register artifact metadata (bytes already stored via BlobStore; plane holds hash+location).
    pub async fn put_artifact(
        &self,
        req: nomiso::PutArtifactRequest,
    ) -> Result<nomiso::ArtifactRecord> {
        Ok(self.client.put_artifact(req).await?)
    }

    /// Register a span into an artifact.
    pub async fn put_span(&self, req: nomiso::PutSpanRequest) -> Result<nomiso::SpanRecord> {
        Ok(self.client.put_span(req).await?)
    }

    /// Link a memory as derived_from artifact/span/memory.
    pub async fn link_derived_from(&self, req: nomiso::LinkDerivedFromRequest) -> Result<()> {
        Ok(self.client.link_derived_from(req).await?)
    }

    /// Supersession history for a memory id.
    pub async fn history(
        &self,
        scope: &str,
        id: nomiso::MemoryId,
    ) -> Result<Vec<nomiso::HistoryEntry>> {
        Ok(self.client.history(&id, scope).await?)
    }

    /// Annotate metadata without forging valid time.
    pub async fn annotate(&self, req: nomiso::AnnotateRequest) -> Result<nomiso::WriteResult> {
        Ok(self.client.annotate(req).await?)
    }

    /// Pack existing hits (utility).
    pub fn pack(&self, hits: &[SearchHit]) -> ContextPack {
        let n = self.profile.policy().default_recall_limit as usize;
        pack_context(hits, n.max(1), 4000)
    }

    /// Checkpoint a durable unit of work (Tact-like).
    #[instrument(skip(self, input))]
    pub async fn checkpoint(&self, input: CheckpointInput) -> Result<CheckpointOutcome> {
        checkpoint::checkpoint(&self.client, self.profile.policy(), input).await
    }

    /// Sleep v1: deterministic dry-run consolidation proposals (no writes).
    pub async fn sleep(&self, scope: &str) -> Result<SleepReport> {
        self.sleep_with(scope, SleepOptions::default()).await
    }

    /// Sleep with explicit options (`dry_run=false` applies only guarded episodic
    /// age-out soft-forgets via [`apply_ops`]; near-dup proposals are review-only
    /// and never auto-applied).
    /// Age-out apply requires `apply_age_out` **and** `older_than_secs`.
    pub async fn sleep_with(&self, scope: &str, opts: SleepOptions) -> Result<SleepReport> {
        let mut report = sleep_pass(&self.client, scope, opts.clone()).await?;
        if opts.dry_run {
            return Ok(report);
        }
        let apply_age = opts.apply_age_out && opts.older_than_secs.is_some();
        let planned = sleep::forget_ops_from_proposals(scope, &report.proposals, apply_age);
        if planned.is_empty() {
            report.dry_run = false;
            report.message = format!(
                "sleep v1 apply: scanned {}, {} proposals, 0 applied",
                report.scanned,
                report.proposals.len()
            );
            return Ok(report);
        }
        let ops: Vec<WriterOp> = planned.iter().map(|(op, _)| op.clone()).collect();
        let outcomes = apply_ops(&self.client, self.profile.policy(), scope, &ops).await?;
        report.applied = sleep::applied_from_outcomes(&planned, &outcomes);
        let ok_n = report.applied.iter().filter(|a| a.ok).count();
        report.consolidated = ok_n > 0;
        report.dry_run = false;
        report.message = format!(
            "sleep v1 apply: scanned {}, {} proposals, {ok_n} applied",
            report.scanned,
            report.proposals.len()
        );
        Ok(report)
    }

    /// Session bootstrap: if coding profile has restore_working_state ON, load coding-wm.
    ///
    /// Never soft-injects semantic packs — returns WM slot only for host to apply.
    pub async fn restore_session_state(
        &self,
        scope: &str,
    ) -> Result<Option<nomiso::TaskStateRecord>> {
        if !self.profile.policy().restore_working_state {
            return Ok(None);
        }
        self.get_working_state(scope).await
    }

    /// Restore / put coding working-state slot (Phase 3.5). Product default: restore ON.
    pub async fn put_working_state(
        &self,
        scope: &str,
        body: serde_json::Value,
        expected_version: Option<u64>,
    ) -> Result<nomiso::TaskStateRecord> {
        Ok(self
            .client
            .put_task_state(nomiso::PutTaskStateRequest {
                scope: scope.into(),
                slot: "coding-wm".into(),
                body,
                expected_version,
                // None means "first write wins": map to create-only; Some(v) is CAS.
                create_only: expected_version.is_none(),
            })
            .await?)
    }

    /// Get coding working-state for restore at session start (never soft-inject of search packs).
    pub async fn get_working_state(&self, scope: &str) -> Result<Option<nomiso::TaskStateRecord>> {
        Ok(self
            .client
            .get_task_state(nomiso::GetTaskStateRequest {
                scope: scope.into(),
                slot: "coding-wm".into(),
            })
            .await?)
    }
}

fn nonempty_corr(s: Option<String>) -> Option<String> {
    s.map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nomiso::Category;
    use std::sync::Arc;

    #[tokio::test]
    async fn remember_recall_roundtrip() {
        let vp = Vegapunk::connect_memory(16)
            .await
            .unwrap()
            .with_profile(Profile::CodingAgent);

        let out = vp
            .remember(RememberInput::fact(
                "org/demo/user/alice",
                "Alice prefers TypeScript for agent tooling.",
            ))
            .await
            .unwrap();
        assert_eq!(out.category, Category::Semantic);

        let hits = vp
            .recall("org/demo/user/alice", "TypeScript agent tooling")
            .await
            .unwrap();
        assert!(hits.iter().any(|h| h.preview.contains("TypeScript")));
    }

    #[tokio::test]
    async fn supersede_product_api() {
        let vp = Vegapunk::connect_memory(8).await.unwrap();
        let first = vp
            .remember(RememberInput::fact(
                "org/demo",
                "Alice prefers JavaScript for tooling.",
            ))
            .await
            .unwrap();
        let second = vp
            .supersede(
                first.id.clone(),
                first.version,
                RememberInput::fact("org/demo", "Alice prefers TypeScript for tooling."),
            )
            .await
            .unwrap();
        assert_ne!(first.id.as_str(), second.id.as_str());
        let hits = vp.recall("org/demo", "TypeScript").await.unwrap();
        assert!(hits.iter().any(|h| h.preview.contains("TypeScript")));
    }

    #[tokio::test]
    async fn rule_writer_episode() {
        let vp = Vegapunk::connect_memory(8)
            .await
            .unwrap()
            .with_writer(Arc::new(RuleWriter));
        let results = vp
            .write_episode(WriteEpisode {
                scope: "org/w".into(),
                text: "Alice prefers Rust for systems.\nRan the test suite today.".into(),
                source: Some("test".into()),
            })
            .await
            .unwrap();
        assert!(results
            .iter()
            .any(|r| matches!(r.ok_result(), Some(ApplyResult::Stored(_)))));
        let hits = vp.recall("org/w", "prefers Rust").await.unwrap();
        assert!(!hits.is_empty());
    }

    #[tokio::test]
    async fn mock_cli_writer_episode() {
        let llm = Arc::new(MockLlm {
            text: r#"[{"op":"put","scope":"org/mock","text":"Alice prefers Zig for tooling.","category":"semantic","confidence":0.95}]"#.into(),
        });
        let vp = Vegapunk::connect_memory(8)
            .await
            .unwrap()
            .with_writer(Arc::new(CliChatWriter::new(llm)));
        let results = vp
            .write_episode(WriteEpisode {
                scope: "org/mock".into(),
                text: "she said she likes Zig for tooling".into(),
                source: Some("test".into()),
            })
            .await
            .unwrap();
        assert!(results
            .iter()
            .any(|r| matches!(r.ok_result(), Some(ApplyResult::Stored(_)))));
        let hits = vp.recall("org/mock", "Zig tooling").await.unwrap();
        assert!(hits.iter().any(|h| h.preview.contains("Zig")));
    }

    #[tokio::test]
    async fn hard_recall_and_pack() {
        let vp = Vegapunk::connect_memory(8).await.unwrap();
        vp.remember(RememberInput::fact(
            "org/hr",
            "Build failed with NOMI_SCOPE_DENIED_42 on project foobar.",
        ))
        .await
        .unwrap();
        let (hr, pack) = vp
            .hard_recall_pack("org/hr", "What is NOMI_SCOPE_DENIED_42?")
            .await
            .unwrap();
        assert!(!hr.queries.is_empty());
        assert!(!hr.abstained, "should find error code: {hr:?}");
        assert!(!pack.abstained);
        assert!(pack.block.contains("NOMI_SCOPE_DENIED_42") || !pack.cards.is_empty());
    }

    #[tokio::test]
    async fn checkpoint_skips_empty() {
        let vp = Vegapunk::connect_memory(8).await.unwrap();
        let out = vp
            .checkpoint(CheckpointInput {
                scope: "org/demo".into(),
                summary: "   ".into(),
                force: false,
            })
            .await
            .unwrap();
        assert!(matches!(out, CheckpointOutcome::Skipped { .. }));
    }

    #[tokio::test]
    async fn sleep_dry_run_proposes() {
        let vp = Vegapunk::connect_memory(8).await.unwrap();
        vp.remember(RememberInput::fact(
            "org/demo",
            "Alice prefers Rust for systems",
        ))
        .await
        .unwrap();
        vp.remember(RememberInput {
            scope: "org/demo".into(),
            text: "ran tests today on the PR".into(),
            category: Some(Category::Episodic),
            confidence: Some(0.5),
            source: None,
            embedding: None,
            episodic: true,
            idempotency_key: None,
        })
        .await
        .unwrap();
        let r = vp.sleep("org/demo").await.unwrap();
        assert!(!r.consolidated);
        assert!(r.dry_run);
        assert!(r.scanned >= 1);
    }

    #[tokio::test]
    async fn sleep_apply_preserves_review_only_near_dups() {
        let vp = Vegapunk::connect_memory(8).await.unwrap();
        // Same category, high Jaccard paraphrases: proposed for review, never applied.
        vp.remember(RememberInput::fact(
            "org/slp",
            "Alice prefers Rust for systems programming and compiler work",
        ))
        .await
        .unwrap();
        vp.remember(RememberInput::fact(
            "org/slp",
            "Alice prefers Rust for systems programming and compiler tasks",
        ))
        .await
        .unwrap();
        let r = vp
            .sleep_with(
                "org/slp",
                SleepOptions {
                    dry_run: false,
                    apply_age_out: false,
                    older_than_secs: None,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(!r.dry_run);
        assert!(r.scanned >= 2);
        assert!(
            r.proposals
                .iter()
                .any(|p| matches!(p, SleepProposal::NearDuplicate { .. })),
            "near-dup must still be proposed for review: {r:?}"
        );
        assert!(
            !r.applied.iter().any(|a| a.ok),
            "near-dup apply is review-only, nothing may be forgotten: {r:?}"
        );
        assert_eq!(vp.count("org/slp", None).await.unwrap(), 2);
        let hits = vp.recall("org/slp", "compiler work").await.unwrap();
        assert!(
            hits.iter().any(|h| h.preview.contains("compiler work")),
            "both records must remain valid-now: {hits:?}"
        );
        let kept = vp.recall("org/slp", "compiler tasks").await.unwrap();
        assert!(
            kept.iter().any(|h| h.preview.contains("compiler tasks")),
            "both records must remain valid-now: {kept:?}"
        );
    }

    #[tokio::test]
    async fn sleep_apply_preserves_conflicting_and_versioned_facts() {
        let pairs: &[(&str, &str)] = &[
            (
                "Signed artifacts must be deployed to production clusters",
                "Signed artifacts must not be deployed to production clusters",
            ),
            (
                "Use protocol v1 for service sync traffic",
                "Use protocol v2 for service sync traffic",
            ),
            (
                "The service listens on port 80 for health checks",
                "The service listens on port 81 for health checks",
            ),
            (
                "Alice prefers Rust for systems programming and compiler work",
                "Alice prefers Rust for systems programming and compiler tasks",
            ),
        ];
        for (i, (a, b)) in pairs.iter().enumerate() {
            let scope = format!("org/slp-pair-{i}");
            let vp = Vegapunk::connect_memory(8).await.unwrap();
            vp.remember(RememberInput::fact(scope.as_str(), *a))
                .await
                .unwrap();
            vp.remember(RememberInput::fact(scope.as_str(), *b))
                .await
                .unwrap();
            let r = vp
                .sleep_with(
                    &scope,
                    SleepOptions {
                        dry_run: false,
                        apply_age_out: false,
                        older_than_secs: None,
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            assert!(
                r.proposals
                    .iter()
                    .any(|p| matches!(p, SleepProposal::NearDuplicate { .. })),
                "pair {i} must surface a review-only near-dup proposal: {r:?}"
            );
            assert!(
                !r.applied.iter().any(|x| x.ok),
                "pair {i}: no near-dup may be applied: {r:?}"
            );
            assert!(
                !r.consolidated,
                "pair {i}: review-only apply must not report consolidated: {r:?}"
            );
            assert_eq!(
                vp.count(&scope, None).await.unwrap(),
                2,
                "pair {i}: both records must be preserved"
            );
        }
    }

    #[tokio::test]
    async fn sleep_rejects_negative_older_than_secs() {
        let vp = Vegapunk::connect_memory(8).await.unwrap();
        let err = vp
            .sleep_with(
                "org/slp-neg",
                SleepOptions {
                    dry_run: false,
                    apply_age_out: true,
                    older_than_secs: Some(-5),
                    ..Default::default()
                },
            )
            .await
            .expect_err("negative older_than_secs must be rejected");
        assert!(
            matches!(err, Error::Invalid(_)),
            "expected Invalid, got {err:?}"
        );
    }

    #[tokio::test]
    async fn sleep_does_not_near_dup_uncertainty() {
        let vp = Vegapunk::connect_memory(8).await.unwrap();
        let text_a =
            "Conflict: Alice prefers TypeScript vs Rust for agent tooling; no trusted prior.";
        let text_b =
            "Conflict: Alice prefers TypeScript vs Rust for agent tooling; no winner chosen.";
        for text in [text_a, text_b] {
            vp.remember(RememberInput {
                scope: "org/slp-unc".into(),
                text: text.into(),
                category: Some(nomiso::Category::Uncertainty),
                confidence: Some(0.5),
                source: None,
                embedding: None,
                episodic: false,
                idempotency_key: None,
            })
            .await
            .unwrap();
        }
        let r = vp
            .sleep_with(
                "org/slp-unc",
                SleepOptions {
                    dry_run: false,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(
            r.proposals
                .iter()
                .all(|p| !matches!(p, SleepProposal::NearDuplicate { .. })),
            "uncertainty rows must not be near-dup losers: {r:?}"
        );
        assert!(r.applied.is_empty(), "{r:?}");
        let n = vp.count("org/slp-unc", None).await.unwrap();
        assert_eq!(n, 2, "both conflict rows must remain");
    }

    #[tokio::test]
    async fn sleep_unrelated_sentences_do_not_propose_near_dup() {
        let vp = Vegapunk::connect_memory(8).await.unwrap();
        vp.remember(RememberInput::fact(
            "org/slp-u",
            "The weather report said rain is expected downtown this evening.",
        ))
        .await
        .unwrap();
        vp.remember(RememberInput::fact(
            "org/slp-u",
            "Please remember to buy milk and bread at the grocery store.",
        ))
        .await
        .unwrap();
        let r = vp.sleep("org/slp-u").await.unwrap();
        assert!(
            !r.proposals
                .iter()
                .any(|p| matches!(p, SleepProposal::NearDuplicate { .. })),
            "unrelated sentences must not propose near-dup: {r:?}"
        );
    }

    #[tokio::test]
    async fn sleep_apply_age_out_without_older_than_does_not_forget() {
        let vp = Vegapunk::connect_memory(8).await.unwrap();
        vp.remember(RememberInput::episode(
            "org/slp-age",
            "ran the integration suite on the PR this afternoon unique-ageout-zzz",
        ))
        .await
        .unwrap();
        let r = vp
            .sleep_with(
                "org/slp-age",
                SleepOptions {
                    dry_run: false,
                    apply_age_out: true,
                    older_than_secs: None,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(!r.dry_run);
        assert!(
            r.proposals
                .iter()
                .any(|p| matches!(p, SleepProposal::AgeOutEpisodic { .. })),
            "age-out still listed for review: {r:?}"
        );
        assert!(
            !r.applied
                .iter()
                .any(|a| a.op == "age_out_soft_forget" && a.ok),
            "must not apply age-out without older_than: {r:?}"
        );
        let hits = vp.recall("org/slp-age", "unique-ageout-zzz").await.unwrap();
        assert!(
            hits.iter().any(|h| h.preview.contains("unique-ageout-zzz")),
            "episodic row must remain: {hits:?}"
        );
    }

    #[tokio::test]
    async fn sleep_apply_age_out_forgets_near_dup_episodic_pair() {
        let vp = Vegapunk::connect_memory(8).await.unwrap();
        vp.remember(RememberInput::episode(
            "org/slp-age-dup",
            "deploy ran on the release candidate build this afternoon",
        ))
        .await
        .unwrap();
        vp.remember(RememberInput::episode(
            "org/slp-age-dup",
            "deploy ran on the release candidate build this evening",
        ))
        .await
        .unwrap();
        let r = vp
            .sleep_with(
                "org/slp-age-dup",
                SleepOptions {
                    dry_run: false,
                    apply_age_out: true,
                    older_than_secs: Some(0),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(
            r.proposals
                .iter()
                .any(|p| matches!(p, SleepProposal::NearDuplicate { .. })),
            "near-dup review proposal expected: {r:?}"
        );
        let applied = r
            .applied
            .iter()
            .filter(|a| a.op == "age_out_soft_forget" && a.ok)
            .count();
        assert_eq!(
            applied, 2,
            "explicit age-out must apply to both near-dup episodic rows: {r:?}"
        );
        assert_eq!(vp.count("org/slp-age-dup", None).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn working_state_roundtrip() {
        let vp = Vegapunk::connect_memory(8).await.unwrap();
        let body = serde_json::json!({
            "goal": "ship auth",
            "next": "write tests",
        });
        let put = vp
            .put_working_state("org/demo", body.clone(), None)
            .await
            .unwrap();
        assert_eq!(put.version, 1);
        let got = vp.get_working_state("org/demo").await.unwrap().unwrap();
        assert_eq!(got.body["goal"], "ship auth");
        assert!(Profile::CodingAgent.policy().restore_working_state);
        assert!(!Profile::Minimal.policy().restore_working_state);
    }

    #[tokio::test]
    async fn working_state_second_none_conflicts_then_cas() {
        let vp = Vegapunk::connect_memory(8).await.unwrap();
        let first = vp
            .put_working_state("org/wm", serde_json::json!({"n": 1}), None)
            .await
            .unwrap();
        assert_eq!(first.version, 1);
        let err = vp
            .put_working_state("org/wm", serde_json::json!({"n": 2}), None)
            .await
            .expect_err("a second None write must conflict (create-only)");
        assert!(
            matches!(err, Error::Nomiso(nomiso::Error::Conflict { .. })),
            "expected Conflict, got {err:?}"
        );
        let got = vp.get_working_state("org/wm").await.unwrap().unwrap();
        assert_eq!(got.body["n"], 1, "conflicting write must not overwrite");
        assert_eq!(got.version, 1);
        let cas = vp
            .put_working_state("org/wm", serde_json::json!({"n": 3}), Some(first.version))
            .await
            .unwrap();
        assert_eq!(cas.version, 2);
        assert_eq!(cas.body["n"], 3);
    }

    #[tokio::test]
    async fn hard_recall_score_floor_abstains() {
        let vp = Vegapunk::connect_memory(8).await.unwrap();
        vp.remember(RememberInput::fact(
            "org/floor",
            "unique floor-token-XYZ about widgets",
        ))
        .await
        .unwrap();
        let mut opts = HardRecallOptions::from_policy(Profile::CodingAgent.policy());
        opts.min_score = Some(1.0e9); // impossibly high floor
        let hr = vp
            .hard_recall_with("org/floor", "floor-token-XYZ widgets", opts)
            .await
            .unwrap();
        assert!(
            hr.abstained,
            "high min_score must abstain: {:?}",
            hr.abstain_reason
        );
    }

    #[tokio::test]
    async fn recall_and_hard_recall_honor_known_as_of() {
        let vp = Vegapunk::connect_memory(8).await.unwrap();
        let now = nomiso::Timestamp::now();
        let future = nomiso::Timestamp::from_second(now.as_second() + 86_400).unwrap();
        vp.remember(RememberInput::fact(
            "org/tlen",
            "present TypeScript preference token",
        ))
        .await
        .unwrap();
        vp.client()
            .put(nomiso::PutRequest {
                scope: "org/tlen".into(),
                category: Category::Semantic,
                content: nomiso::Content::text("future only secret-token-ZZZ"),
                known_at: Some(future),
                valid_from: Some(future),
                ..Default::default()
            })
            .await
            .unwrap();
        let mid = nomiso::Timestamp::from_second(now.as_second() + 60).unwrap();
        let mut ro = RecallOptions::from_policy(Profile::CodingAgent.policy());
        ro.as_of = Some(future);
        ro.known_as_of = Some(mid);
        let hits = vp
            .recall_with("org/tlen", "TypeScript secret-token-ZZZ", ro)
            .await
            .unwrap();
        assert!(
            hits.iter().any(|h| h.preview.contains("TypeScript")),
            "present row must remain: {hits:?}"
        );
        assert!(
            !hits.iter().any(|h| h.preview.contains("secret-token-ZZZ")),
            "future-known row must be hidden: {hits:?}"
        );
        let mut ho = HardRecallOptions::from_policy(Profile::CodingAgent.policy());
        ho.as_of = Some(future);
        ho.known_as_of = Some(mid);
        let hr = vp
            .hard_recall_with("org/tlen", "secret-token-ZZZ", ho)
            .await
            .unwrap();
        assert!(
            hr.hits
                .iter()
                .all(|h| !h.preview.contains("secret-token-ZZZ")),
            "hard-recall must apply the same lens: {:?}",
            hr.hits
        );
        let listed = vp
            .list_with(
                "org/tlen",
                EnumerateOptions {
                    as_of: Some(future),
                    known_as_of: Some(mid),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(listed
            .items
            .iter()
            .any(|i| i.record.content.text.contains("TypeScript")));
        assert!(
            !listed
                .items
                .iter()
                .any(|i| i.record.content.text.contains("secret-token-ZZZ")),
            "list_with must apply known_as_of: {:?}",
            listed.items
        );
        let n = vp
            .count_with(
                "org/tlen",
                EnumerateOptions {
                    as_of: Some(future),
                    known_as_of: Some(mid),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(n, listed.items.len() as u64);
    }

    #[tokio::test]
    async fn list_traces_after_hard_recall_pack() {
        let vp = Vegapunk::connect_memory(8).await.unwrap();
        vp.remember(RememberInput::fact(
            "org/tr-list",
            "trace inventory TypeScript fact",
        ))
        .await
        .unwrap();
        let (hr, _) = vp
            .hard_recall_pack("org/tr-list", "TypeScript")
            .await
            .unwrap();
        let tid = hr.trace_id.expect("pack should emit trace_id");
        let rows = vp
            .list_traces(
                "org/tr-list",
                nomiso::ListTracesRequest {
                    scope: "org/tr-list".into(),
                    limit: Some(8),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(
            rows.iter().any(|r| r.trace_id == tid),
            "list_traces must surface the pack trace: {rows:?}"
        );
    }

    #[tokio::test]
    async fn record_inject_and_correlation_on_pack_trace() {
        let vp = Vegapunk::connect_memory(8)
            .await
            .unwrap()
            .with_correlation(Some(" sess-1 ".into()), Some("turn-9".into()));
        vp.remember(RememberInput::fact(
            "org/inj",
            "inject loop TypeScript fact",
        ))
        .await
        .unwrap();
        let (hr, pack) = vp.hard_recall_pack("org/inj", "TypeScript").await.unwrap();
        let tid = hr.trace_id.expect("trace");
        let ids: Vec<_> = pack
            .cards
            .iter()
            .map(|c| nomiso::MemoryId::new(c.id.clone()))
            .collect();
        assert!(!ids.is_empty(), "pack should contain cards to inject");

        let before = vp.get_trace("org/inj", &tid).await.unwrap();
        let before_kinds: Vec<_> = before.events.iter().map(|e| e.kind.as_str()).collect();
        assert!(before_kinds.contains(&"search"), "{before_kinds:?}");
        assert!(before_kinds.contains(&"pack"), "{before_kinds:?}");
        assert!(
            !before_kinds.contains(&"inject"),
            "pack must not auto-emit inject: {before_kinds:?}"
        );

        assert!(
            vp.record_inject("org/inj", &tid, &[], None).await.is_err(),
            "empty inject must fail closed"
        );

        vp.record_inject("org/inj", &tid, &ids, Some("host injected pack.block"))
            .await
            .unwrap();
        vp.record_trace_outcome("org/inj", &tid, TraceOutcome::Helped, None)
            .await
            .unwrap();
        let bundle = vp.get_trace("org/inj", &tid).await.unwrap();
        let kinds: Vec<_> = bundle.events.iter().map(|e| e.kind.as_str()).collect();
        assert!(kinds.contains(&"search"), "{kinds:?}");
        assert!(kinds.contains(&"pack"), "{kinds:?}");
        assert!(kinds.contains(&"inject"), "{kinds:?}");
        assert!(kinds.contains(&"outcome"), "{kinds:?}");
        assert!(
            bundle
                .events
                .iter()
                .all(|e| e.session_id.as_deref() == Some("sess-1")),
            "{bundle:?}"
        );
        assert!(
            bundle
                .events
                .iter()
                .all(|e| e.turn_id.as_deref() == Some("turn-9")),
            "{bundle:?}"
        );
        let inject = bundle
            .events
            .iter()
            .find(|e| e.kind.as_str() == "inject")
            .expect("inject event");
        let payload_ids = inject
            .payload
            .as_ref()
            .and_then(|p| p.get("memory_ids"))
            .and_then(|v| v.as_array())
            .expect("inject payload memory_ids");
        assert_eq!(payload_ids.len(), ids.len(), "{payload_ids:?}");

        let by_mem = vp
            .list_traces_for_memory("org/inj", &ids[0], Default::default())
            .await
            .unwrap();
        assert!(
            by_mem
                .iter()
                .any(|r| r.trace_id == tid && r.outcome == Some(TraceOutcome::Helped)),
            "list_traces_for_memory must join inject+outcome: {by_mem:?}"
        );
        let unknown = vp
            .list_traces_for_memory(
                "org/inj",
                &nomiso::MemoryId::new(uuid::Uuid::now_v7().to_string()),
                Default::default(),
            )
            .await
            .unwrap();
        assert!(
            unknown.is_empty(),
            "unknown memory id must be empty: {unknown:?}"
        );

        let applied = vp
            .apply_writer_ops(
                "org/inj",
                &[WriterOp::Put {
                    input: RememberInput::fact("org/inj", "inject loop follow-up Rust fact"),
                }],
            )
            .await
            .unwrap();
        let write_tid = applied.trace_id.expect("apply_ops should emit write trace");
        let write_bundle = vp.get_trace("org/inj", &write_tid).await.unwrap();
        assert!(
            write_bundle
                .events
                .iter()
                .any(|e| e.kind.as_str() == "write"),
            "{write_bundle:?}"
        );
        assert!(
            write_bundle
                .events
                .iter()
                .all(|e| e.session_id.as_deref() == Some("sess-1")
                    && e.turn_id.as_deref() == Some("turn-9")),
            "{write_bundle:?}"
        );
    }

    #[tokio::test]
    async fn uncertainty_list_and_count_filter() {
        let vp = Vegapunk::connect_memory(8).await.unwrap();
        vp.remember(RememberInput::fact(
            "org/unc",
            "Alice prefers TypeScript for agent tooling.",
        ))
        .await
        .unwrap();
        vp.remember(RememberInput {
            scope: "org/unc".into(),
            text: "Conflict: Alice prefers TypeScript vs Rust; no trusted prior.".into(),
            category: Some(nomiso::Category::Uncertainty),
            confidence: Some(0.5),
            source: None,
            embedding: None,
            episodic: false,
            idempotency_key: None,
        })
        .await
        .unwrap();
        let listed = vp
            .list_with(
                "org/unc",
                EnumerateOptions {
                    categories: Some(vec![nomiso::Category::Uncertainty]),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(listed.items.len(), 1, "{listed:?}");
        assert_eq!(
            listed.items[0].record.category,
            nomiso::Category::Uncertainty
        );
        assert!(listed.items[0]
            .record
            .content
            .text
            .contains("TypeScript vs Rust"));
        let n = vp
            .count_with(
                "org/unc",
                EnumerateOptions {
                    categories: Some(vec![nomiso::Category::Uncertainty]),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(n, 1);
        let all = vp.count("org/unc", None).await.unwrap();
        assert!(all >= 2, "unfiltered count={all}");
        let mut ro = RecallOptions::from_policy(Profile::CodingAgent.policy());
        ro.categories = Some(vec![nomiso::Category::Uncertainty]);
        let hits = vp
            .recall_with("org/unc", "Alice prefers", ro)
            .await
            .unwrap();
        assert!(
            hits.iter()
                .all(|h| h.category == nomiso::Category::Uncertainty),
            "recall --category must not leak other categories: {hits:?}"
        );
        assert!(
            hits.iter()
                .any(|h| h.preview.contains("TypeScript vs Rust")),
            "{hits:?}"
        );
        let mut ho = HardRecallOptions::from_policy(Profile::CodingAgent.policy());
        ho.categories = Some(vec![nomiso::Category::Uncertainty]);
        let hr = vp
            .hard_recall_with("org/unc", "Alice prefers", ho)
            .await
            .unwrap();
        assert!(
            hr.hits
                .iter()
                .all(|h| h.category == nomiso::Category::Uncertainty),
            "hard-recall --category must not leak: {:?}",
            hr.hits
        );
    }

    #[tokio::test]
    async fn scope_isolation_via_vegapunk() {
        let vp = Vegapunk::connect_memory(8).await.unwrap();
        vp.remember(RememberInput::fact(
            "org/a",
            "secret alpha-token-AAA only in scope a",
        ))
        .await
        .unwrap();
        vp.remember(RememberInput::fact(
            "org/b",
            "secret beta-token-BBB only in scope b",
        ))
        .await
        .unwrap();
        let hits = vp.recall("org/a", "secret alpha-token").await.unwrap();
        assert!(hits.iter().all(|h| h.scope == "org/a"));
        assert!(!hits.iter().any(|h| h.preview.contains("beta-token")));
    }

    #[tokio::test]
    async fn find_candidates_returns_open_prior() {
        let vp = Vegapunk::connect_memory(8).await.unwrap();
        let stored = vp
            .remember(RememberInput::fact(
                "org/cand",
                "Alice prefers TypeScript for agent tooling.",
            ))
            .await
            .unwrap();
        let hits = vp
            .find_candidates("org/cand", "TypeScript preference", Some(5))
            .await
            .unwrap();
        assert!(
            hits.iter()
                .any(|h| h.id == stored.id && h.version == stored.version),
            "{hits:?}"
        );
    }

    #[tokio::test]
    async fn find_candidates_skips_superseded_prior() {
        let vp = Vegapunk::connect_memory(8).await.unwrap();
        let first = vp
            .remember(RememberInput::fact(
                "org/cand2",
                "Alice prefers JavaScript for agent tooling.",
            ))
            .await
            .unwrap();
        let second = vp
            .supersede(
                first.id.clone(),
                first.version,
                RememberInput::fact("org/cand2", "Alice prefers TypeScript for agent tooling."),
            )
            .await
            .unwrap();
        let hits = vp
            .find_candidates("org/cand2", "agent tooling preference", Some(5))
            .await
            .unwrap();
        assert!(
            hits.iter().any(|h| h.id == second.id),
            "open successor missing: {hits:?}"
        );
        assert!(
            hits.iter().all(|h| h.id != first.id),
            "superseded prior must not be a candidate: {hits:?}"
        );
    }

    #[tokio::test]
    async fn store_artifact_then_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let vp = Vegapunk::connect_memory(8)
            .await
            .unwrap()
            .with_blob_store(Arc::new(nomiso_blob::FsBlobStore::new(dir.path())));
        let art = vp
            .store_artifact(StoreArtifactInput {
                scope: "org/blob".into(),
                bytes: b"hello-cas".to_vec(),
                media_type: "text/plain".into(),
                source: Some("test".into()),
                trust: None,
            })
            .await
            .unwrap();
        assert_eq!(art.blake3.len(), 64);
        assert!(art.location.starts_with("file://"));
        assert_eq!(art.scope, "org/blob");
    }

    #[tokio::test]
    async fn ingest_compaction_prefix_puts() {
        let vp = Vegapunk::connect_memory(8)
            .await
            .unwrap()
            .with_writer(Arc::new(RuleWriter));
        let report = vp
            .ingest_compaction(CompactionIngest {
                scope: "org/compact".into(),
                transcript: "Alice prefers TypeScript for MCP.\nUse pnpm not npm.".into(),
                summary: Some("compacted auth setup".into()),
            })
            .await
            .unwrap();
        assert!(
            report.apply.outcomes.iter().any(|o| o.is_ok()),
            "{:?}",
            report.apply
        );
        assert!(report.checkpoint.is_some());
        assert!(
            report.checkpoint_error.is_none(),
            "{:?}",
            report.checkpoint_error
        );
        let hits = vp.recall("org/compact", "TypeScript MCP").await.unwrap();
        assert!(!hits.is_empty(), "{hits:?}");
    }

    struct FailSecondWriter;

    #[async_trait::async_trait]
    impl MemoryWriter for FailSecondWriter {
        async fn extract(&self, episode: &WriteEpisode) -> Result<Vec<WriterOp>> {
            Ok(vec![
                WriterOp::Put {
                    input: RememberInput {
                        scope: episode.scope.clone(),
                        text: "Alice prefers TypeScript for MCP.".into(),
                        category: None,
                        confidence: Some(0.9),
                        source: Some("test".into()),
                        embedding: None,
                        episodic: false,
                        idempotency_key: None,
                    },
                },
                WriterOp::Forget {
                    id: nomiso::MemoryId::new("memory:does-not-exist"),
                    scope: episode.scope.clone(),
                    expected_version: Some(1),
                    hard: false,
                },
            ])
        }
    }

    #[tokio::test]
    async fn ingest_compaction_skips_checkpoint_when_apply_fails() {
        let vp = Vegapunk::connect_memory(8)
            .await
            .unwrap()
            .with_writer(Arc::new(FailSecondWriter));
        let report = vp
            .ingest_compaction(CompactionIngest {
                scope: "org/compact-fail".into(),
                transcript: "ignored; writer emits put+bad forget".into(),
                summary: Some("must not checkpoint when apply has failures".into()),
            })
            .await
            .unwrap();
        assert!(
            report.apply.outcomes.iter().any(|o| !o.is_ok()),
            "{:?}",
            report.apply
        );
        assert!(report.checkpoint.is_none(), "{:?}", report.checkpoint);
        assert!(
            report.checkpoint_error.is_none(),
            "{:?}",
            report.checkpoint_error
        );
        let hits = vp
            .recall("org/compact-fail", "TypeScript MCP")
            .await
            .unwrap();
        assert!(
            hits.iter().any(|h| h.preview.contains("TypeScript")),
            "prefix-preserving apply keeps the committed prefix: {hits:?}"
        );
    }
}
