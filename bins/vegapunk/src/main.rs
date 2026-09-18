//! vegapunk — AXI-style agent-first CLI for the Vegapunk product on Nomiso.
//!
//! Primary users: agents/models. Product is harness-agnostic (no vendor LLM required).
//! Grok/Codex process backends are optional maintainer features only.

mod config;
mod connect;
mod output;
mod scope;

#[cfg(feature = "serve")]
mod http;
#[cfg(feature = "mcp")]
mod mcp;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use tracing::{info, Level};
use tracing_subscriber::EnvFilter;
use vegapunk::{
    CheckpointInput, CheckpointOutcome, Profile, RememberInput, RuleWriter, Vegapunk, WriteEpisode,
    WriterOp,
};

use crate::config::{load_config, write_example, VegapunkFileConfig};
use crate::output::{print_error, print_help_hints, print_out, HelpStream, OutputFormat};
use crate::scope::resolve_scope;

#[derive(Debug, Parser)]
#[command(
    name = "vegapunk",
    about = "Vegapunk — agent-first memory product on Nomiso (AXI CLI)",
    version,
    // Bare invocation runs Status (content-first).
    subcommand_required = false,
    arg_required_else_help = false
)]
struct Cli {
    /// Memory profile.
    #[arg(long, global = true, env = "VEGAPUNK_PROFILE")]
    profile: Option<String>,

    /// Surreal endpoint (`rocksdb://…` default, `ws://…`, or explicit `memory` demo).
    #[arg(long, global = true, env = "VEGAPUNK_ENDPOINT")]
    endpoint: Option<String>,

    /// Embedding dimension (must match store HNSW).
    #[arg(long, global = true, env = "VEGAPUNK_EMBED_DIM")]
    embed_dim: Option<usize>,

    /// Default / active scope (also VEGAPUNK_SCOPE / vegapunk.toml).
    #[arg(long, global = true, env = "VEGAPUNK_SCOPE")]
    scope: Option<String>,

    /// Disable hashing embedder on write (ignored when --embed-url is set).
    #[arg(long, global = true, default_value_t = false)]
    no_hash_embed: bool,

    /// Host session id: stamped on emitted traces; also filters `traces`.
    #[arg(long, global = true, env = "VEGAPUNK_SESSION_ID")]
    session_id: Option<String>,

    /// Host turn id: stamped on emitted traces; also filters `traces`.
    #[arg(long, global = true, env = "VEGAPUNK_TURN_ID")]
    turn_id: Option<String>,

    /// OpenAI-compatible embeddings base URL (replaces hashing).
    #[arg(long, global = true, env = "VEGAPUNK_EMBED_URL")]
    embed_url: Option<String>,

    /// Embed model id when using --embed-url.
    #[arg(long, global = true, env = "VEGAPUNK_EMBED_MODEL")]
    embed_model: Option<String>,

    /// Agent stdout format: toon (default) | json | full.
    #[arg(long, global = true, value_enum, default_value_t = OutputFormat::Toon, env = "VEGAPUNK_FORMAT")]
    format: OutputFormat,

    /// Suppress next-step help[] hints.
    #[arg(long, global = true, default_value_t = false)]
    no_help: bool,

    /// Emit help[] on stdout (default: stderr). Useful when hosts only capture stdout.
    #[arg(long, global = true, default_value_t = false)]
    help_stdout: bool,

    /// Writer backend for write-episode: rule (default) | grok | codex (maintainer).
    #[arg(long, global = true, default_value = "rule", env = "VEGAPUNK_WRITER")]
    writer: String,

    /// Query rewriter: rule (default) | grok | codex (maintainer).
    #[arg(long, global = true, default_value = "rule", env = "VEGAPUNK_REWRITER")]
    rewriter: String,

    /// Optional LLM CLI binary (maintainer feature cli-llm).
    #[arg(long, global = true, env = "VEGAPUNK_LLM_BIN")]
    llm_bin: Option<PathBuf>,

    /// Optional model id for maintainer CLI backends.
    #[arg(long, global = true, env = "VEGAPUNK_LLM_MODEL")]
    llm_model: Option<String>,

    /// LLM process timeout seconds (maintainer backends only).
    #[arg(
        long,
        global = true,
        default_value_t = 180,
        env = "VEGAPUNK_LLM_TIMEOUT"
    )]
    llm_timeout: u64,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Clone, ValueEnum)]
enum CategoryArg {
    Semantic,
    Episodic,
    Identity,
    Procedural,
    Uncertainty,
}

impl From<CategoryArg> for nomiso::Category {
    fn from(c: CategoryArg) -> Self {
        match c {
            CategoryArg::Semantic => Self::Semantic,
            CategoryArg::Episodic => Self::Episodic,
            CategoryArg::Identity => Self::Identity,
            CategoryArg::Procedural => Self::Procedural,
            CategoryArg::Uncertainty => Self::Uncertainty,
        }
    }
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Content-first home: config + health + next steps (also bare `vegapunk`).
    Status,
    /// Health check against the Nomiso plane.
    Health,
    /// Write example vegapunk.toml.
    Init {
        #[arg(long, default_value = "vegapunk.toml")]
        path: PathBuf,
        #[arg(long, default_value_t = false)]
        force: bool,
    },
    /// Remember a fact or episode (structured encode).
    Encode {
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        text: Option<String>,
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long)]
        category: Option<CategoryArg>,
        #[arg(long, default_value_t = false)]
        episodic: bool,
        #[arg(long)]
        idempotency_key: Option<String>,
    },
    /// Explicit hybrid recall (never auto-injects).
    Recall {
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        query: String,
        #[arg(long)]
        limit: Option<u32>,
        /// Valid-time lens (RFC3339).
        #[arg(long)]
        as_of: Option<String>,
        /// Known-time lens (RFC3339).
        #[arg(long)]
        known_as_of: Option<String>,
        /// System-time lens (RFC3339).
        #[arg(long)]
        sys_as_of: Option<String>,
        /// Optional category filter (does not default to uncertainty).
        #[arg(long)]
        category: Option<CategoryArg>,
    },
    /// Checkpoint a durable unit of work.
    Checkpoint {
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        summary: Option<String>,
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        force: bool,
    },
    /// Consolidation pass (deterministic proposals; dry-run default).
    Sleep {
        #[arg(long)]
        scope: Option<String>,
        /// Apply only explicitly enabled age-out soft-forgets. Near-duplicate
        /// proposals are review-only and never auto-deleted.
        #[arg(long, default_value_t = false)]
        apply: bool,
        /// When applying, soft-forget AgeOutEpisodic proposals.
        /// Requires `--older-than-hours` or age-outs are listed only.
        #[arg(long, default_value_t = false)]
        apply_age_out: bool,
        /// Age-out apply/proposal window in hours (e.g. 168). Required to apply age-outs.
        #[arg(long)]
        older_than_hours: Option<u64>,
    },
    /// Supersede a prior memory.
    Supersede {
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        prior_id: String,
        #[arg(long)]
        expected_version: u64,
        #[arg(long)]
        text: Option<String>,
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// Multi-pass hard recall + optional context pack (never auto-injected).
    HardRecall {
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        query: String,
        #[arg(long, default_value_t = false)]
        pack: bool,
        /// Plane-score / BM25-hybrid floor. Default 0.0 does not abstain.
        #[arg(long)]
        min_score: Option<f64>,
        /// Valid-time lens (RFC3339).
        #[arg(long)]
        as_of: Option<String>,
        /// Known-time lens (RFC3339).
        #[arg(long)]
        known_as_of: Option<String>,
        /// System-time lens (RFC3339).
        #[arg(long)]
        sys_as_of: Option<String>,
        /// Optional category filter (does not default to uncertainty).
        #[arg(long)]
        category: Option<CategoryArg>,
        /// Opt-in bounded graph candidate expansion (T5 experiment).
        #[arg(long, default_value_t = false)]
        expand_graph: bool,
    },
    /// Apply host-extracted WriterOp JSON (primary agent write path).
    ApplyOps {
        #[arg(long)]
        file: Option<PathBuf>,
        /// Ops JSON string (or use --file / stdin).
        #[arg(long)]
        json: Option<String>,
    },
    /// List memories under scope (optional text / category / temporal filters). Never auto-injects.
    List {
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        text: Option<String>,
        #[arg(long)]
        limit: Option<u32>,
        /// Resume from a prior `next_cursor` (JSON).
        #[arg(long)]
        cursor_json: Option<String>,
        #[arg(long)]
        as_of: Option<String>,
        #[arg(long)]
        known_as_of: Option<String>,
        #[arg(long)]
        sys_as_of: Option<String>,
        /// Optional category filter (semantic|episodic|identity|procedural|uncertainty).
        #[arg(long)]
        category: Option<CategoryArg>,
    },
    /// Count memories under scope (optional text / category / temporal filters).
    Count {
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        text: Option<String>,
        #[arg(long)]
        as_of: Option<String>,
        #[arg(long)]
        known_as_of: Option<String>,
        #[arg(long)]
        sys_as_of: Option<String>,
        /// Optional category filter (semantic|episodic|identity|procedural|uncertainty).
        #[arg(long)]
        category: Option<CategoryArg>,
    },
    /// List parent traces in a scope (optional --session-id / --turn-id filter).
    Traces {
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        limit: Option<u32>,
        /// Inclusive created_at lower bound (RFC3339).
        #[arg(long)]
        since: Option<String>,
        /// Inclusive created_at upper bound (RFC3339).
        #[arg(long)]
        until: Option<String>,
    },
    /// Traces that referenced a memory id, plus latest host outcome.
    TracesByMemory {
        #[arg(long)]
        scope: Option<String>,
        /// Memory id (`memory:…` or bare uuid).
        #[arg(long)]
        id: String,
        #[arg(long)]
        limit: Option<u32>,
        /// Inclusive created_at lower bound (RFC3339).
        #[arg(long)]
        since: Option<String>,
        /// Inclusive created_at upper bound (RFC3339).
        #[arg(long)]
        until: Option<String>,
    },
    /// Load a trace bundle by trace_id (plane trace plane).
    GetTrace {
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        trace_id: String,
    },
    /// T4 controller: prepare a typed context proposal (manifest + blocks).
    /// The host decides placement; nothing is injected by this command.
    PrepareContext {
        #[arg(long)]
        scope: Option<String>,
        /// Bounded statement of the information need.
        #[arg(long)]
        task: String,
        /// Explicit task entity/resource references (added as query terms).
        #[arg(long = "entity")]
        entities: Vec<String>,
        /// Already-present memory refs: `--inventory <id>` or `<id>:<version>`.
        #[arg(long = "inventory")]
        inventory: Vec<String>,
        /// Rendered-token ceiling (approx chars/4 accounting).
        #[arg(long)]
        budget_tokens: Option<usize>,
        /// Max context blocks.
        #[arg(long)]
        budget_blocks: Option<usize>,
        /// Retrieval candidate cap.
        #[arg(long)]
        candidates: Option<usize>,
        /// Whole-workflow wall-clock deadline, ms.
        #[arg(long)]
        deadline_ms: Option<u64>,
        /// Effort plan: direct | expanded (bounded graph expansion).
        #[arg(long, default_value = "direct")]
        effort: String,
        /// Optional score floor → insufficient_evidence below it.
        #[arg(long)]
        min_score: Option<f64>,
        /// Return a partial proposal instead of failing on channel errors.
        #[arg(long, default_value_t = false)]
        allow_partial: bool,
        /// Skip the durable pack event (proposal cannot be acked later).
        #[arg(long, default_value_t = false)]
        no_trace: bool,
        /// Valid-time lens (RFC3339).
        #[arg(long)]
        as_of: Option<String>,
        /// Known-time lens (RFC3339).
        #[arg(long)]
        known_as_of: Option<String>,
        /// System-time lens (RFC3339).
        #[arg(long)]
        sys_as_of: Option<String>,
        /// Optional category filter.
        #[arg(long)]
        category: Option<CategoryArg>,
        /// Caller correlation id.
        #[arg(long)]
        request_id: Option<String>,
    },
    /// T4 controller: record which proposal blocks the host actually
    /// inserted (subset, truncation). Idempotent; verified against the
    /// proposal's durable pack event — unknown blocks are rejected.
    RecordInsertion {
        #[arg(long)]
        scope: Option<String>,
        /// Trace id from the proposal.
        #[arg(long)]
        trace_id: String,
        /// Proposal id from the proposal.
        #[arg(long)]
        proposal_id: String,
        /// Acknowledging host identity (default: "cli").
        #[arg(long, default_value = "cli")]
        host: String,
        /// Inserted block ids (repeatable).
        #[arg(long = "block")]
        blocks: Vec<String>,
        /// Full inserted list as JSON: [{"block_id":"…","truncated":true,"note":"…"}]
        #[arg(long)]
        inserted_json: Option<String>,
    },
    /// Host reports that a pack was injected (never auto-injected).
    TraceInject {
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        trace_id: String,
        /// Memory ids from the pack that were injected.
        #[arg(long = "id")]
        ids: Vec<String>,
        #[arg(long)]
        note: Option<String>,
    },
    /// Host-reported outcome for a trace (helped|harmed|unknown|skipped).
    TraceOutcome {
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        trace_id: String,
        #[arg(long)]
        outcome: String,
        /// Who evaluated: host | model | execution (default host).
        #[arg(long, default_value = "host")]
        evaluator: String,
        #[arg(long)]
        note: Option<String>,
    },
    /// Open-validity priors for Put vs Supersede.
    Candidates {
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        query: String,
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Store bytes (CAS) then register artifact metadata.
    StoreArtifact {
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        file: PathBuf,
        #[arg(long, default_value = "application/octet-stream")]
        media_type: String,
        #[arg(long)]
        source: Option<String>,
    },
    /// Ingest host-extracted durable lines with prefix-preserving apply. Checkpoint only when all operations succeed; inspect partial outcomes.
    IngestCompaction {
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        transcript: Option<String>,
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long)]
        summary: Option<String>,
    },
    /// Register artifact metadata (blake3 + location; bytes live outside Surreal).
    PutArtifact {
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        blake3: String,
        #[arg(long)]
        location: String,
        #[arg(long, default_value = "application/octet-stream")]
        media_type: String,
        #[arg(long)]
        source: Option<String>,
    },
    /// History chain for a memory id.
    History {
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        id: String,
    },
    /// Get/put coding working-state slot (restore ON for coding profile; not soft-inject).
    WorkingState {
        #[arg(long)]
        scope: Option<String>,
        /// When set, write this JSON as the slot body; else get.
        #[arg(long)]
        put_json: Option<String>,
        /// CAS update: expected current slot version. Omit for create-only first write.
        #[arg(long, requires = "put_json")]
        expected_version: Option<u64>,
    },
    /// Read full scoped records by id, including validity and provenance.
    Read {
        #[arg(long)]
        scope: Option<String>,
        /// Memory id (`memory:…` or bare uuid). Repeatable.
        #[arg(long = "id", required = true)]
        ids: Vec<String>,
    },
    /// List belief_event audit journal for a memory id.
    BeliefEvents {
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        id: String,
    },
    /// Rule/MemoryWriter episode extract → commit (demo; prefer apply-ops for agents).
    WriteEpisode {
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        text: Option<String>,
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// Create a scoped entity record.
    EntityPut {
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        kind: String,
        #[arg(long)]
        name: String,
        #[arg(long)]
        alias: Vec<String>,
        /// Attributes JSON object.
        #[arg(long)]
        attrs: Option<String>,
    },
    /// Get a scoped entity record by id.
    EntityGet {
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        id: String,
    },
    /// CAS-update an entity record.
    EntityUpdate {
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        id: String,
        #[arg(long)]
        expected_version: u64,
        #[arg(long)]
        name: Option<String>,
        /// Replace alias list (repeatable).
        #[arg(long)]
        alias: Vec<String>,
        /// Attributes JSON object.
        #[arg(long)]
        attrs: Option<String>,
    },
    /// Assert a typed relationship between same-scope endpoints.
    RelPut {
        #[arg(long)]
        scope: Option<String>,
        /// Closed-registry predicate (supports, derived_from, contradicts,
        /// mentions, depends_on, applies_to, observed_in, attempted, resolved_by).
        #[arg(long)]
        predicate: String,
        /// Subject endpoint `kind:id` (memory|entity|artifact|span).
        #[arg(long)]
        from: String,
        /// Object endpoint `kind:id`.
        #[arg(long)]
        to: String,
        /// Pin subject revision (versioned kinds only).
        #[arg(long)]
        subject_rev: Option<u64>,
        /// Pin object revision.
        #[arg(long)]
        object_rev: Option<u64>,
        /// Epistemic status (reported|inferred|verified|disputed|unknown).
        #[arg(long)]
        epistemic: Option<String>,
        /// Evidence endpoint `kind:id` (repeatable).
        #[arg(long)]
        evidence: Vec<String>,
    },
    /// CAS-update a relationship (state/epistemic).
    RelUpdate {
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        id: String,
        #[arg(long)]
        expected_version: u64,
        /// New state (active|closed|stale; purged is erasure-driven only).
        #[arg(long)]
        state: Option<String>,
        /// Epistemic status.
        #[arg(long)]
        epistemic: Option<String>,
        /// Reason (required for closed/stale).
        #[arg(long)]
        reason: Option<String>,
    },
    /// List relationships in a scope.
    RelList {
        #[arg(long)]
        scope: Option<String>,
        /// Filter by endpoint `kind:id`.
        #[arg(long = "endpoint-ref")]
        endpoint_ref: Option<String>,
        #[arg(long)]
        predicate: Option<String>,
        #[arg(long)]
        state: Option<String>,
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Bounded traversal from seed endpoints.
    Traverse {
        #[arg(long)]
        scope: Option<String>,
        /// Seed endpoint `kind:id` (repeatable).
        #[arg(long)]
        seed: Vec<String>,
        /// Predicate filter (repeatable; default all non-foundation).
        #[arg(long)]
        predicate: Vec<String>,
        /// Direction (out|in|both).
        #[arg(long)]
        direction: Option<String>,
        /// Edge state filter (repeatable; default active only).
        #[arg(long)]
        state: Vec<String>,
        #[arg(long)]
        max_depth: Option<u32>,
        #[arg(long)]
        max_visited: Option<u32>,
        #[arg(long)]
        max_edges: Option<u32>,
        #[arg(long)]
        deadline_ms: Option<u64>,
    },
    /// Inspect embedding generations and coverage.
    EmbedState,
    /// Attest the active generation's unknown (legacy) identity.
    EmbedAttest {
        #[arg(long)]
        family: String,
        #[arg(long)]
        model: String,
        #[arg(long)]
        dimension: Option<u32>,
        /// l2|none.
        #[arg(long)]
        normalization: Option<String>,
        /// Vector encoding (default f32).
        #[arg(long)]
        encoding: Option<String>,
        /// Declared limitation note (e.g. approximate hashing embedder).
        #[arg(long)]
        limitation: Option<String>,
    },
    /// Declare a new staging embedding generation.
    EmbedDeclare {
        #[arg(long)]
        family: String,
        #[arg(long)]
        model: String,
        #[arg(long)]
        dimension: Option<u32>,
        #[arg(long)]
        normalization: Option<String>,
        #[arg(long)]
        encoding: Option<String>,
        #[arg(long)]
        limitation: Option<String>,
        #[arg(long)]
        note: Option<String>,
    },
    /// Activate a staged generation (coverage revalidated atomically).
    EmbedActivate {
        #[arg(long)]
        generation: u64,
    },
    /// Per-table row counts — the declared consistent frontier for snapshot
    /// manifests and post-restore verification (OPS-004).
    StoreStats,
    /// Embedding provider readiness probe — identity, ready flag, safe
    /// detail, probe latency (OPS-001). Reports `provider: null` when the
    /// composition is hashing-only.
    ProviderStatus,
    /// Enqueue a durable job.
    JobEnqueue {
        #[arg(long)]
        scope: Option<String>,
        /// Registered kind (e.g. reindex).
        #[arg(long)]
        kind: String,
        /// Payload JSON object.
        #[arg(long)]
        payload: Option<String>,
        /// Composition identity JSON object.
        #[arg(long)]
        composition: Option<String>,
        /// Pinned inputs JSON array [{"kind":"memory","id":"…","revision":N}].
        #[arg(long)]
        inputs: Option<String>,
        #[arg(long)]
        max_attempts: Option<u32>,
        #[arg(long)]
        lease_ms: Option<u64>,
        #[arg(long)]
        retry_backoff_ms: Option<u64>,
        #[arg(long)]
        deadline_ms: Option<u64>,
        #[arg(long)]
        dedup_hint: Option<String>,
    },
    /// Get a job record by id.
    JobGet {
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        id: String,
    },
    /// List job summaries (payload bodies omitted).
    JobList {
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        state: Option<String>,
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Claim the next eligible job under a fenced lease (worker primitive).
    JobClaim {
        /// Worker identity.
        #[arg(long)]
        worker: Option<String>,
        /// Scope grant (repeatable); defaults to resolved scope.
        #[arg(long)]
        scope: Vec<String>,
        /// Kind allowlist (repeatable).
        #[arg(long)]
        kind: Vec<String>,
    },
    /// Renew a job lease under its fence (worker primitive).
    JobRenew {
        #[arg(long)]
        id: String,
        #[arg(long)]
        fence: u64,
        #[arg(long)]
        worker: Option<String>,
    },
    /// Persist a job checkpoint under its fence (worker primitive).
    JobCheckpoint {
        #[arg(long)]
        id: String,
        #[arg(long)]
        fence: u64,
        #[arg(long)]
        worker: Option<String>,
        /// Checkpoint JSON value.
        #[arg(long)]
        value: String,
    },
    /// Complete a job under its fence (worker primitive).
    JobComplete {
        #[arg(long)]
        id: String,
        #[arg(long)]
        fence: u64,
        #[arg(long)]
        worker: Option<String>,
        /// Result JSON value.
        #[arg(long)]
        result: Option<String>,
    },
    /// Record a job failure under its fence (worker primitive).
    JobFail {
        #[arg(long)]
        id: String,
        #[arg(long)]
        fence: u64,
        #[arg(long)]
        worker: Option<String>,
        #[arg(long)]
        code: String,
        #[arg(long)]
        message: String,
        #[arg(long, default_value_t = true)]
        retryable: bool,
    },
    /// Cancel a pending or leased job.
    JobCancel {
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        id: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Supersede a pending/leased job by its replacement's id.
    JobSupersede {
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        id: String,
        #[arg(long)]
        replacement_id: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Run a durable-job worker (claim loop; registers built-in executors).
    WorkerRun {
        /// Worker identity recorded on leases.
        #[arg(long)]
        worker: Option<String>,
        /// Scope grant (repeatable); defaults to resolved scope.
        #[arg(long)]
        scope: Vec<String>,
        /// Kind allowlist (repeatable); default registers `reindex` when an embedder is attached.
        #[arg(long)]
        kind: Vec<String>,
        /// Process at most one claimed job then exit.
        #[arg(long, default_value_t = false)]
        once: bool,
        #[arg(long, default_value_t = 1)]
        concurrency: usize,
        #[arg(long, default_value_t = 250)]
        poll_ms: u64,
        /// Bound total claimed jobs.
        #[arg(long)]
        max_jobs: Option<u64>,
    },
    /// Offline skill eval (C0 suite, or `--coding` pack).
    Eval {
        #[arg(long)]
        suite: Option<PathBuf>,
        /// Score the coding-agent pack (gold apply + RuleWriter extract).
        #[arg(long, default_value_t = false)]
        coding: bool,
        /// Live host extract via Grok CLI (`cli-llm`). Report-only; does not fail the gold gate.
        #[arg(long, default_value_t = false)]
        live: bool,
        /// Re-score WriterOp[] dumps (`{scenario_id}.ops.json`). No host call.
        #[arg(long)]
        live_dir: Option<PathBuf>,
        /// System track (host must hard-recall --pack before answering probes).
        #[arg(long, default_value_t = false)]
        system: bool,
        /// Run the Grok CLI system host (report-only). Implies --system.
        #[arg(long, default_value_t = false)]
        system_live: bool,
        /// Re-score system dumps (`{scenario_id}.json`). No host call.
        #[arg(long)]
        system_dir: Option<PathBuf>,
    },
    /// Thin product HTTP API (no model endpoints). Optional Bearer API key.
    Serve {
        /// Bind address.
        #[arg(long, default_value = "127.0.0.1:9191", env = "VEGAPUNK_BIND")]
        bind: String,
        /// Optional API key (Authorization: Bearer …). Empty = no auth (dev).
        #[arg(long, env = "VEGAPUNK_API_KEY")]
        api_key: Option<String>,
    },
    /// Thin product MCP on stdio (Vegapunk ops, not plane catalog).
    Mcp,
    /// Single-process product smoke (encode → hard-recall → apply-ops). CI-safe on memory.
    Smoke,
}

struct Resolved {
    profile: Profile,
    profile_name: String,
    endpoint: String,
    embed_dim: usize,
    default_scope: Option<String>,
    config_path: Option<PathBuf>,
    soft_inject: bool,
    embed_url: Option<String>,
    embed_model: Option<String>,
    embed_api_key_env: Option<String>,
    blob_root: PathBuf,
}

impl Resolved {
    fn embed_plan(&self, hash: bool) -> connect::EmbedPlan {
        connect::EmbedPlan {
            hash,
            url: self.embed_url.clone(),
            model: self.embed_model.clone(),
            api_key_env: self.embed_api_key_env.clone(),
            dim: self.embed_dim,
        }
    }
}

/// Config load/resolve failure: safe outer message, exit code 2.
#[derive(Debug)]
struct ConfigFailure(String);

impl std::fmt::Display for ConfigFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigFailure {}

/// Marker: a structured payload was already printed for this failure.
#[derive(Debug)]
struct ReportedFailure;

impl std::fmt::Display for ReportedFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("operation reported a failure payload")
    }
}

impl std::error::Error for ReportedFailure {}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(Level::WARN.into())
                .from_env_lossy(),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let fmt = cli.format;
    let is_mcp = matches!(cli.command, Some(Commands::Mcp));
    match execute(cli).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            if e.chain().any(|c| c.is::<ReportedFailure>()) {
                return std::process::ExitCode::FAILURE;
            }
            if let Some(cf) = e.chain().find_map(|c| c.downcast_ref::<ConfigFailure>()) {
                if is_mcp {
                    eprintln!("config error: {cf}");
                } else {
                    let _ = print_error(fmt, "config_error", format!("config error: {cf}"), &[]);
                }
                return std::process::ExitCode::from(2);
            }
            let (code, msg) = domain_error_parts(&e);
            if is_mcp {
                eprintln!("error: {msg}");
            } else {
                let _ = print_error(fmt, &code, msg, &[]);
            }
            std::process::ExitCode::FAILURE
        }
    }
}

/// Typed code + safe public message for a runtime error.
fn domain_error_parts(e: &anyhow::Error) -> (String, String) {
    for c in e.chain() {
        if let Some(ve) = c.downcast_ref::<vegapunk::Error>() {
            return (ve.code().to_string(), ve.public_message());
        }
        if let Some(ne) = c.downcast_ref::<nomiso::Error>() {
            return (ne.code().to_string(), ne.public_message());
        }
    }
    ("request_failed".to_string(), e.to_string())
}

async fn execute(mut cli: Cli) -> Result<()> {
    let command = cli.command.take().unwrap_or(Commands::Status);

    match command {
        Commands::Init { path, force } => {
            write_example(&path, force)?;
            print_out(
                &serde_json::json!({
                    "status": "ok",
                    "wrote": path.display().to_string(),
                }),
                cli.format,
            )?;
            hints(
                &cli,
                &[
                    "default endpoint is rocksdb://./.nomiso-data (durable; gitignored)",
                    "demo only: endpoint = \"memory\"  ·  shared daemon: ws://127.0.0.1:8000/rpc",
                    "optional HTTP embed: embed_url + VEGAPUNK_EMBED_API_KEY (match embed_dim)",
                    "run `vegapunk status` then `vegapunk encode --text \"…\"`",
                ],
            );
            return Ok(());
        }
        Commands::Eval {
            suite,
            coding,
            live,
            live_dir,
            system,
            system_live,
            system_dir,
        } => {
            if (live || live_dir.is_some() || system || system_live || system_dir.is_some())
                && !coding
            {
                anyhow::bail!("--live / --live-dir / --system / --system-dir require --coding");
            }
            if coding {
                if let Some(dir) = system_dir {
                    let sys_rep = vegapunk::run_system_from_dir(&dir)
                        .await
                        .context("coding system dir")?;
                    eprintln!("{}", sys_rep.markdown());
                    print_out(&sys_rep, cli.format)?;
                    return Ok(());
                }
                if system || system_live {
                    if system_live {
                        std::env::set_var("VEGAPUNK_SYSTEM_LIVE", "1");
                    }
                    let sys_rep = vegapunk::run_coding_system()
                        .await
                        .context("coding system")?;
                    eprintln!("{}", sys_rep.markdown());
                    print_out(&sys_rep, cli.format)?;
                    return Ok(());
                }
                if let Some(dir) = live_dir {
                    let live_rep = vegapunk::run_live_from_dir(&dir)
                        .await
                        .context("coding live dir")?;
                    eprintln!("{}", live_rep.markdown());
                    print_out(&live_rep, cli.format)?;
                    return Ok(());
                }
                if live {
                    #[cfg(feature = "cli-llm")]
                    {
                        let dump = std::env::var_os("VEGAPUNK_SKILL_LIVE_DIR")
                            .map(std::path::PathBuf::from);
                        let live_rep = vegapunk::run_grok_extract(dump.as_deref())
                            .await
                            .context("coding grok extract")?;
                        eprintln!("{}", live_rep.markdown());
                        print_out(&live_rep, cli.format)?;
                        return Ok(());
                    }
                    #[cfg(not(feature = "cli-llm"))]
                    {
                        anyhow::bail!("--live needs vegapunk-cli --features cli-llm");
                    }
                }
                let gold = vegapunk::run_gold_apply()
                    .await
                    .context("coding gold apply")?;
                print_out(&gold, cli.format)?;
                eprintln!("{}", gold.markdown());
                let rule = vegapunk::run_rule_extract()
                    .await
                    .context("coding rule extract")?;
                eprintln!("{}", rule.markdown());
                if !gold.passed {
                    std::process::exit(1);
                }
                return Ok(());
            }
            let suite = if let Some(p) = suite {
                vegapunk::load_suite_json(&p).context("load suite")?
            } else {
                vegapunk::skill_suite_c0()
            };
            let res = vegapunk::run_skill_suite(&suite)
                .await
                .context("run skill suite")?;
            print_out(&res, cli.format)?;
            if !res.passed {
                std::process::exit(1);
            }
            return Ok(());
        }
        _ => {}
    }

    let (file_cfg, config_path) = load_config().map_err(|e| ConfigFailure(e.to_string()))?;
    let resolved =
        resolve_cli(&cli, file_cfg, config_path).map_err(|e| ConfigFailure(e.to_string()))?;

    match command {
        Commands::Serve { bind, api_key } => run_serve(&cli, &resolved, &bind, api_key).await?,
        Commands::Mcp => run_mcp(&cli, &resolved).await?,
        Commands::Smoke => run_smoke(&cli, &resolved).await?,
        Commands::Status | Commands::Health => run_status(&cli, &resolved).await?,
        other => run_connected(&cli, &resolved, other).await?,
    }

    Ok(())
}

fn help_stream(cli: &Cli) -> HelpStream {
    if cli.no_help {
        HelpStream::Off
    } else if cli.help_stdout {
        HelpStream::Stdout
    } else {
        HelpStream::Stderr
    }
}

fn hints(cli: &Cli, lines: &[&str]) {
    print_help_hints(lines, help_stream(cli));
}

fn nonempty(s: Option<String>) -> Option<String> {
    s.and_then(|v| {
        let t = v.trim().to_string();
        if t.is_empty() {
            None
        } else {
            Some(t)
        }
    })
}

fn resolve_cli(
    cli: &Cli,
    mut file: VegapunkFileConfig,
    config_path: Option<PathBuf>,
) -> Result<Resolved> {
    let cwd = std::env::current_dir().context("current directory")?;
    // A CLI/env endpoint override is resolved against cwd and makes the file's
    // endpoint unused, so it must not trip the legacy ambiguity check.
    if let Some(ep) = nonempty(cli.endpoint.clone()) {
        file.endpoint = crate::config::resolve_local_endpoint(&ep, &cwd)?;
        file.endpoint_set = false;
    }
    if let Some(env_root) = std::env::var_os("VEGAPUNK_BLOB_ROOT") {
        if env_root.is_empty() {
            bail!("VEGAPUNK_BLOB_ROOT must be non-empty");
        }
        let p = PathBuf::from(env_root);
        file.blob_root = Some(if p.is_relative() { cwd.join(p) } else { p });
        file.blob_root_set = false;
    }
    crate::config::resolve_storage_paths(&mut file, config_path.as_deref(), &cwd)?;
    let blob_root = file
        .blob_root
        .clone()
        .unwrap_or_else(|| cwd.join(crate::config::DEFAULT_BLOB_ROOT));
    let profile_name = nonempty(cli.profile.clone()).unwrap_or(file.profile);
    let profile = Profile::parse(&profile_name)
        .with_context(|| format!("unknown profile '{profile_name}'"))?;
    let endpoint = file.endpoint;
    let embed_dim = cli.embed_dim.unwrap_or(file.embed_dim);
    // flag > env (clap binds VEGAPUNK_SCOPE into cli.scope) > config; empty = unset
    let default_scope = nonempty(cli.scope.clone()).or_else(|| nonempty(file.default_scope));
    let embed_url = nonempty(cli.embed_url.clone()).or_else(|| nonempty(file.embed_url));
    let embed_model = nonempty(cli.embed_model.clone()).or_else(|| nonempty(file.embed_model));
    Ok(Resolved {
        profile,
        profile_name,
        endpoint,
        embed_dim,
        default_scope,
        config_path,
        soft_inject: file.soft_inject,
        embed_url,
        embed_model,
        embed_api_key_env: nonempty(file.embed_api_key_env),
        blob_root,
    })
}

fn is_ephemeral(endpoint: &str) -> bool {
    connect::is_ephemeral(endpoint)
}

fn public_endpoint(endpoint: &str) -> String {
    match endpoint.split_once("://") {
        Some(("rocksdb" | "surrealkv" | "memory" | "mem", _)) | None => endpoint.to_owned(),
        Some((scheme @ ("http" | "https" | "ws" | "wss"), _)) => format!("{scheme}://[configured]"),
        Some(_) => "[configured endpoint]".into(),
    }
}

async fn run_status(cli: &Cli, resolved: &Resolved) -> Result<()> {
    let ephemeral = is_ephemeral(&resolved.endpoint);
    // Always print structured status first (connect may fail).
    let mut body = serde_json::json!({
        "status": "connecting",
        "profile": resolved.profile_name,
        "endpoint": public_endpoint(&resolved.endpoint),
        "ephemeral": ephemeral,
        "embed_dim": resolved.embed_dim,
        "default_scope": resolved.default_scope,
        "config": resolved.config_path.as_ref().map(|p| p.display().to_string()),
        "soft_inject_reserved": resolved.soft_inject,
        "soft_inject_active": false,
        "embed_mode": resolved.embed_plan(!cli.no_hash_embed).mode_label(),
        "blob_root": resolved.blob_root.display().to_string(),
        "writer": cli.writer,
        "format": format!("{:?}", cli.format).to_ascii_lowercase(),
        "description": "Vegapunk agent memory on Nomiso (explicit pack inject only)",
    });
    if ephemeral {
        body.as_object_mut().unwrap().insert(
            "warning".into(),
            serde_json::json!(
                "EPHEMERAL: endpoint is in-process memory. This process will not see the last one. \
                 For agents use rocksdb://./.nomiso-data (default) or ws://… — set endpoint = \"memory\" only for demos."
            ),
        );
    }

    connect::check_endpoint(&resolved.endpoint)?;
    let store = connect::store_config(&resolved.endpoint, resolved.embed_dim);

    match Vegapunk::connect(store).await {
        Ok(vp) => match vp.health().await {
            Ok(()) => {
                body["status"] = serde_json::json!("ok");
                body["restore_working_state"] =
                    serde_json::json!(resolved.profile.policy().restore_working_state);
                // Coding profile: surface WM slot for host bootstrap (never auto-inject packs).
                if let Some(scope) = resolved.default_scope.as_deref() {
                    if let Ok(wm) = vp.restore_session_state(scope).await {
                        body["working_state"] = match wm {
                            Some(rec) => serde_json::json!({
                                "slot": rec.slot,
                                "version": rec.version,
                                "body": rec.body,
                            }),
                            None => serde_json::json!(null),
                        };
                    }
                }
                print_out(&body, cli.format)?;
            }
            Err(e) => {
                body["status"] = serde_json::json!("unhealthy");
                body["error"] = serde_json::json!(e.public_message());
                body["code"] = serde_json::json!(e.code());
                print_out(&body, cli.format)?;
                return Err(ReportedFailure.into());
            }
        },
        Err(e) => {
            body["status"] = serde_json::json!("connect_error");
            body["error"] = serde_json::json!(e.public_message());
            body["code"] = serde_json::json!(e.code());
            print_out(&body, cli.format)?;
            return Err(ReportedFailure.into());
        }
    }

    hints(
        cli,
        &[
            "default is durable rocksdb://./.nomiso-data; memory is demo-only",
            "vegapunk encode --scope <scope> --text \"Alice prefers TypeScript\"",
            "vegapunk hard-recall --scope <scope> --query \"TypeScript\" --pack",
            "vegapunk working-state --put-json '{\"goal\":\"…\"}'",
            "vegapunk sleep --apply",
            "vegapunk apply-ops --file ops.json",
            "vegapunk serve --bind 127.0.0.1:9191",
            "vegapunk mcp   # product MCP stdio",
        ],
    );
    Ok(())
}

async fn run_serve(
    cli: &Cli,
    resolved: &Resolved,
    bind: &str,
    api_key: Option<String>,
) -> Result<()> {
    #[cfg(not(feature = "serve"))]
    {
        let _ = (cli, resolved, bind, api_key);
        bail!("rebuild with --features serve (default on product builds)");
    }
    #[cfg(feature = "serve")]
    {
        use secrecy::SecretString;

        let vp = connect::connect_from_parts(
            &resolved.endpoint,
            resolved.embed_dim,
            &resolved.profile_name,
            resolved.embed_plan(!cli.no_hash_embed),
        )
        .await?
        .with_blob_store(Arc::new(
            vegapunk::BlobConfig {
                root: resolved.blob_root.clone(),
            }
            .fs_store(),
        ))
        .with_correlation(cli.session_id.clone(), cli.turn_id.clone());
        vp.health().await.context("health before serve")?;
        let addr: std::net::SocketAddr = bind.parse().context("parse --bind")?;
        let auth_mode = if api_key.as_ref().is_some_and(|s| !s.trim().is_empty()) {
            "bearer"
        } else {
            "none"
        };
        let key = api_key
            .filter(|s| !s.trim().is_empty())
            .map(SecretString::from);
        let app = http::router(http::ServeState { vp, api_key: key });
        info!(%addr, ephemeral = is_ephemeral(&resolved.endpoint), "vegapunk serve");
        if is_ephemeral(&resolved.endpoint) {
            tracing::warn!(
                "serving ephemeral memory endpoint — agents need rocksdb:// or ws:// (memory is demo-only)"
            );
        }
        let listener = tokio::net::TcpListener::bind(addr).await?;
        print_out(
            &serde_json::json!({
                "status": "listening",
                "bind": bind,
                "endpoint": public_endpoint(&resolved.endpoint),
                "ephemeral": is_ephemeral(&resolved.endpoint),
                "auth": auth_mode,
                "routes": http::route_names(),
            }),
            cli.format,
        )?;
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
                info!("shutdown signal");
            })
            .await?;
        Ok(())
    }
}

async fn run_smoke(cli: &Cli, resolved: &Resolved) -> Result<()> {
    let scope = resolve_scope(cli.scope.clone(), resolved.default_scope.as_deref())
        .unwrap_or_else(|_| "org/smoke/user/agent".into());
    let vp = connect::connect_from_parts(
        &resolved.endpoint,
        resolved.embed_dim,
        &resolved.profile_name,
        resolved.embed_plan(!cli.no_hash_embed),
    )
    .await?
    .with_blob_store(Arc::new(
        vegapunk::BlobConfig {
            root: resolved.blob_root.clone(),
        }
        .fs_store(),
    ))
    .with_correlation(cli.session_id.clone(), cli.turn_id.clone());
    vp.health().await?;
    let enc = vp
        .remember(RememberInput::fact(
            &scope,
            "Alice prefers TypeScript for agent tooling and MCP servers.",
        ))
        .await?;
    let (hr, pack) = vp
        .hard_recall_pack(&scope, "TypeScript agent tooling")
        .await?;
    if hr.abstained || pack.abstained {
        bail!(
            "smoke hard-recall abstained after encode (scope={scope}): {:?}",
            hr.abstain_reason
        );
    }
    let ops: Vec<WriterOp> = vec![WriterOp::Put {
        input: RememberInput::fact(&scope, "Alice prefers Rust for systems work."),
    }];
    let applied = vp.apply_writer_ops(&scope, &ops).await?;
    let (hr2, _) = vp.hard_recall_pack(&scope, "prefers Rust").await?;
    print_out(
        &serde_json::json!({
            "status": "ok",
            "scope": scope,
            "endpoint": public_endpoint(&resolved.endpoint),
            "ephemeral": is_ephemeral(&resolved.endpoint),
            "encoded_id": enc.id.to_string(),
            "recall1_hits": hr.hits.len(),
            "pack_tokens": pack.estimated_tokens,
            "apply_ops": applied.outcomes.len(),
            "apply_trace_id": applied.trace_id,
            "recall2_hits": hr2.hits.len(),
            "recall2_abstained": hr2.abstained,
        }),
        cli.format,
    )?;
    if hr2.abstained {
        bail!("smoke second hard-recall abstained after apply-ops");
    }
    Ok(())
}

async fn run_mcp(cli: &Cli, resolved: &Resolved) -> Result<()> {
    #[cfg(not(feature = "mcp"))]
    {
        let _ = (cli, resolved);
        bail!("rebuild with --features mcp (default on product builds)");
    }
    #[cfg(feature = "mcp")]
    {
        let vp = connect::connect_from_parts(
            &resolved.endpoint,
            resolved.embed_dim,
            &resolved.profile_name,
            resolved.embed_plan(!cli.no_hash_embed),
        )
        .await?
        .with_blob_store(Arc::new(
            vegapunk::BlobConfig {
                root: resolved.blob_root.clone(),
            }
            .fs_store(),
        ))
        .with_correlation(cli.session_id.clone(), cli.turn_id.clone());
        vp.health().await.context("health before mcp")?;
        info!(
            tools = ?mcp::tool_names(),
            "starting vegapunk product MCP stdio"
        );
        mcp::serve_stdio(vp).await?;
        Ok(())
    }
}

async fn run_connected(cli: &Cli, resolved: &Resolved, command: Commands) -> Result<()> {
    connect::check_endpoint(&resolved.endpoint)?;
    let store = connect::store_config(&resolved.endpoint, resolved.embed_dim);

    let mut vp = Vegapunk::connect(store)
        .await
        .context("connect vegapunk/nomiso")?
        .with_profile(resolved.profile)
        .with_blob_store(Arc::new(
            vegapunk::BlobConfig {
                root: resolved.blob_root.clone(),
            }
            .fs_store(),
        ));

    vp = attach_writer(
        vp,
        &cli.writer,
        cli.llm_bin.as_ref(),
        cli.llm_model.as_deref(),
        cli.llm_timeout,
    )?;
    vp = attach_rewriter(
        vp,
        &cli.rewriter,
        cli.llm_bin.as_ref(),
        cli.llm_model.as_deref(),
        cli.llm_timeout,
    )?;
    vp = connect::apply_embedder(vp, &resolved.embed_plan(!cli.no_hash_embed))?;
    vp = vp.with_correlation(cli.session_id.clone(), cli.turn_id.clone());

    let fmt = cli.format;
    let cfg_default = resolved.default_scope.as_deref();

    match command {
        Commands::Status
        | Commands::Health
        | Commands::Init { .. }
        | Commands::Eval { .. }
        | Commands::Serve { .. }
        | Commands::Mcp
        | Commands::Smoke => unreachable!("handled in main"),
        Commands::Encode {
            scope,
            text,
            file,
            category,
            episodic,
            idempotency_key,
        } => {
            let scope = resolve_scope(scope.or_else(|| cli.scope.clone()), cfg_default)?;
            let body = load_text(text, file)?;
            let cat = category.map(Into::into);
            let out = vp
                .remember(RememberInput {
                    scope: scope.clone(),
                    text: body,
                    category: cat,
                    confidence: None,
                    source: None,
                    embedding: None,
                    episodic,
                    idempotency_key,
                })
                .await?;
            print_out(&out, fmt)?;
            let h1 = format!("vegapunk recall --scope {scope} --query \"<keywords>\"");
            let h2 = format!("vegapunk hard-recall --scope {scope} --query \"…\" --pack");
            hints(cli, &[&h1, &h2]);
        }
        Commands::Recall {
            scope,
            query,
            limit,
            as_of,
            known_as_of,
            sys_as_of,
            category,
        } => {
            let scope = resolve_scope(scope.or_else(|| cli.scope.clone()), cfg_default)?;
            let mut opts = vegapunk::RecallOptions::from_policy(resolved.profile.policy());
            if let Some(l) = limit {
                opts.limit = l;
            }
            opts.as_of = as_of
                .as_deref()
                .map(vegapunk::parse_timestamp)
                .transpose()
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            opts.known_as_of = known_as_of
                .as_deref()
                .map(vegapunk::parse_timestamp)
                .transpose()
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            opts.sys_as_of = sys_as_of
                .as_deref()
                .map(vegapunk::parse_timestamp)
                .transpose()
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            opts.categories = category.map(|c| vec![c.into()]);
            let hits = vp.recall_with(&scope, &query, opts).await?;
            // Minimal card projection for agents
            let cards: Vec<serde_json::Value> = hits
                .iter()
                .map(|h| {
                    serde_json::json!({
                        "id": h.id.to_string(),
                        "score": h.score,
                        "category": h.category.as_str(),
                        "preview": truncate(&h.preview, 160),
                        "version": h.version,
                    })
                })
                .collect();
            print_out(
                &serde_json::json!({
                    "count": cards.len(),
                    "scope": scope,
                    "hits": cards,
                }),
                fmt,
            )?;
            let h1 = format!("vegapunk hard-recall --scope {scope} --query \"{query}\" --pack");
            hints(
                cli,
                &[
                    &h1,
                    "vegapunk supersede --prior-id <id> --expected-version <n> --text \"…\"",
                ],
            );
        }
        Commands::Checkpoint {
            scope,
            summary,
            file,
            force,
        } => {
            let scope = resolve_scope(scope.or_else(|| cli.scope.clone()), cfg_default)?;
            let summary = load_text(summary, file)?;
            let out = vp
                .checkpoint(CheckpointInput {
                    scope,
                    summary,
                    force,
                })
                .await?;
            print_out(&out, fmt)?;
            if matches!(out, CheckpointOutcome::Skipped { .. }) {
                hints(cli, &["vegapunk checkpoint --force --summary \"…\""]);
            }
        }
        Commands::Sleep {
            scope,
            apply,
            apply_age_out,
            older_than_hours,
        } => {
            let scope = resolve_scope(scope.or_else(|| cli.scope.clone()), cfg_default)?;
            let older_than_secs = older_than_hours
                .map(|h| {
                    i64::try_from(h)
                        .ok()
                        .and_then(|h| h.checked_mul(3600))
                        .ok_or_else(|| anyhow::anyhow!("--older-than-hours {h} overflows seconds"))
                })
                .transpose()?;
            let opts = vegapunk::SleepOptions {
                dry_run: !apply,
                apply_age_out,
                older_than_secs,
                ..Default::default()
            };
            let r = vp.sleep_with(&scope, opts).await?;
            print_out(&r, fmt)?;
            if r.dry_run {
                hints(
                    cli,
                    &[
                        "vegapunk sleep --apply --apply-age-out --older-than-hours 168  # explicit age-out only",
                        "near-duplicate proposals are review-only; apply never deletes them",
                    ],
                );
            }
            if !r.is_ok() {
                return Err(ReportedFailure.into());
            }
        }
        Commands::Supersede {
            scope,
            prior_id,
            expected_version,
            text,
            file,
        } => {
            let scope = resolve_scope(scope.or_else(|| cli.scope.clone()), cfg_default)?;
            let body = load_text(text, file)?;
            let out = vp
                .supersede(
                    nomiso::MemoryId::new(prior_id),
                    expected_version,
                    RememberInput::fact(scope, body),
                )
                .await?;
            print_out(&out, fmt)?;
        }
        Commands::HardRecall {
            scope,
            query,
            pack,
            min_score,
            as_of,
            known_as_of,
            sys_as_of,
            category,
            expand_graph,
        } => {
            let scope = resolve_scope(scope.or_else(|| cli.scope.clone()), cfg_default)?;
            let mut opts = vegapunk::HardRecallOptions::from_policy(resolved.profile.policy());
            if let Some(floor) = min_score {
                opts.min_score = Some(floor);
            }
            opts.as_of = as_of
                .as_deref()
                .map(vegapunk::parse_timestamp)
                .transpose()
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            opts.known_as_of = known_as_of
                .as_deref()
                .map(vegapunk::parse_timestamp)
                .transpose()
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            opts.sys_as_of = sys_as_of
                .as_deref()
                .map(vegapunk::parse_timestamp)
                .transpose()
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            opts.categories = category.map(|c| vec![c.into()]);
            if expand_graph {
                opts.graph_expand = Some(nomiso::GraphExpand::default());
            }
            if pack {
                let (hr, ctx) = vp.hard_recall_pack_with(&scope, &query, opts).await?;
                // Nested + possibly long pack block → JSON under TOON policy
                print_out(
                    &serde_json::json!({
                        "abstained": hr.abstained || ctx.abstained,
                        "trace_id": hr.trace_id,
                        "queries": hr.queries,
                        "hit_count": hr.hits.len(),
                        "pack": {
                            "abstained": ctx.abstained,
                            "reason": ctx.reason,
                            "estimated_tokens": ctx.estimated_tokens,
                            "cards": ctx.cards.iter().map(|c| serde_json::json!({
                                "id": c.id,
                                "score": c.score,
                                "category": c.category,
                                "scope": c.scope,
                                "preview": c.preview,
                                "version": c.version,
                                "reference_only": c.reference_only,
                                "original_bytes": c.original_bytes,
                            })).collect::<Vec<_>>(),
                            "block": ctx.block,
                        },
                        "inject_note": "Host injects pack.block only if non-empty; never auto-injected by Vegapunk",
                    }),
                    fmt,
                )?;
            } else {
                let hr = vp.hard_recall_with(&scope, &query, opts).await?;
                let cards: Vec<_> = hr
                    .hits
                    .iter()
                    .map(|h| {
                        serde_json::json!({
                            "id": h.id.to_string(),
                            "score": h.score,
                            "preview": truncate(&h.preview, 160),
                            "version": h.version,
                        })
                    })
                    .collect();
                print_out(
                    &serde_json::json!({
                        "abstained": hr.abstained,
                        "reason": hr.abstain_reason,
                        "queries": hr.queries,
                        "count": cards.len(),
                        "hits": cards,
                    }),
                    fmt,
                )?;
            }
            hints(
                cli,
                &[
                    "if pack non-empty: inject pack.block into agent context and cite ids",
                    "vegapunk trace-inject --trace-id … --id …  # host-reported; never auto-injected",
                    "vegapunk trace-outcome --trace-id … --outcome helped",
                    "if abstained: rewrite query (error codes, entity names) and retry once",
                ],
            );
        }
        Commands::ApplyOps { file, json } => {
            let active = match resolve_scope(cli.scope.clone(), cfg_default) {
                Ok(s) => s,
                Err(e) => {
                    print_error(
                        fmt,
                        "scope_required",
                        e,
                        &["pass --scope, set VEGAPUNK_SCOPE, or default_scope in vegapunk.toml"],
                    )?;
                    return Err(ReportedFailure.into());
                }
            };
            let raw = load_ops_json(json, file)?;
            let ops: Vec<WriterOp> = match vegapunk::parse_writer_ops_from_model(&raw) {
                Ok(o) => o,
                Err(e) => {
                    print_error(
                        fmt,
                        "ops_parse_failed",
                        e,
                        &["provide WriterOp JSON array via --json, --file, or stdin"],
                    )?;
                    return Err(ReportedFailure.into());
                }
            };
            // Scope pin + policy enforced inside vegapunk::apply_ops (lib-level).
            let out = match vp.apply_writer_ops(&active, &ops).await {
                Ok(o) => o,
                Err(e) => {
                    print_error(
                        fmt,
                        e.code(),
                        e.public_message(),
                        &["ensure each op.scope == active scope; hard-forget needs policy"],
                    )?;
                    return Err(ReportedFailure.into());
                }
            };
            print_out(&out, fmt)?;
            hints(
                cli,
                &[
                    "vegapunk hard-recall --query \"…\" --pack",
                    "prefer noop over low-confidence invent in host extract",
                ],
            );
            if !out.is_ok() {
                return Err(ReportedFailure.into());
            }
        }
        Commands::List {
            scope,
            text,
            limit,
            cursor_json,
            as_of,
            known_as_of,
            sys_as_of,
            category,
        } => {
            let scope = resolve_scope(scope.or_else(|| cli.scope.clone()), cfg_default)?;
            let cursor = cursor_json
                .as_deref()
                .map(|raw| {
                    serde_json::from_str::<nomiso::ListCursor>(raw)
                        .context("--cursor-json must be a returned next_cursor JSON object")
                })
                .transpose()?;
            let page = vp
                .list_with(
                    &scope,
                    vegapunk::EnumerateOptions {
                        text,
                        limit,
                        cursor,
                        as_of: as_of
                            .as_deref()
                            .map(vegapunk::parse_timestamp)
                            .transpose()
                            .map_err(|e| anyhow::anyhow!("{e}"))?,
                        known_as_of: known_as_of
                            .as_deref()
                            .map(vegapunk::parse_timestamp)
                            .transpose()
                            .map_err(|e| anyhow::anyhow!("{e}"))?,
                        sys_as_of: sys_as_of
                            .as_deref()
                            .map(vegapunk::parse_timestamp)
                            .transpose()
                            .map_err(|e| anyhow::anyhow!("{e}"))?,
                        categories: category.map(|c| vec![c.into()]),
                    },
                )
                .await?;
            let cards: Vec<serde_json::Value> = page
                .items
                .iter()
                .map(|i| {
                    serde_json::json!({
                        "id": i.record.id.to_string(),
                        "version": i.record.version,
                        "category": i.record.category.as_str(),
                        "preview": truncate(&i.record.content.text, 160),
                        "score": i.score,
                        "score_kind": i.score_kind,
                        "stale": i.record.stale,
                    })
                })
                .collect();
            print_out(
                &serde_json::json!({
                    "scope": scope,
                    "count": cards.len(),
                    "items": cards,
                    "next_cursor": page.next_cursor,
                }),
                fmt,
            )?;
            hints(
                cli,
                &["vegapunk history --id …", "vegapunk hard-recall --pack"],
            );
        }
        Commands::Count {
            scope,
            text,
            as_of,
            known_as_of,
            sys_as_of,
            category,
        } => {
            let scope = resolve_scope(scope.or_else(|| cli.scope.clone()), cfg_default)?;
            let n = vp
                .count_with(
                    &scope,
                    vegapunk::EnumerateOptions {
                        text,
                        as_of: as_of
                            .as_deref()
                            .map(vegapunk::parse_timestamp)
                            .transpose()
                            .map_err(|e| anyhow::anyhow!("{e}"))?,
                        known_as_of: known_as_of
                            .as_deref()
                            .map(vegapunk::parse_timestamp)
                            .transpose()
                            .map_err(|e| anyhow::anyhow!("{e}"))?,
                        sys_as_of: sys_as_of
                            .as_deref()
                            .map(vegapunk::parse_timestamp)
                            .transpose()
                            .map_err(|e| anyhow::anyhow!("{e}"))?,
                        categories: category.map(|c| vec![c.into()]),
                        ..Default::default()
                    },
                )
                .await?;
            print_out(&serde_json::json!({ "scope": scope, "count": n }), fmt)?;
        }
        Commands::Traces {
            scope,
            limit,
            since,
            until,
        } => {
            let scope = resolve_scope(scope.or_else(|| cli.scope.clone()), cfg_default)?;
            let rows = vp
                .list_traces(
                    &scope,
                    nomiso::ListTracesRequest {
                        scope: scope.clone(),
                        since: since
                            .as_deref()
                            .map(vegapunk::parse_timestamp)
                            .transpose()
                            .map_err(|e| anyhow::anyhow!("{e}"))?,
                        until: until
                            .as_deref()
                            .map(vegapunk::parse_timestamp)
                            .transpose()
                            .map_err(|e| anyhow::anyhow!("{e}"))?,
                        limit,
                        session_id: cli.session_id.clone(),
                        turn_id: cli.turn_id.clone(),
                    },
                )
                .await?;
            print_out(
                &serde_json::json!({
                    "scope": scope,
                    "count": rows.len(),
                    "traces": rows,
                }),
                fmt,
            )?;
            hints(
                cli,
                &[
                    "vegapunk get-trace --trace-id …",
                    "vegapunk traces-by-memory --id …",
                    "vegapunk trace-inject --trace-id … --id …",
                    "vegapunk trace-outcome --trace-id … --outcome helped",
                ],
            );
        }
        Commands::TracesByMemory {
            scope,
            id,
            limit,
            since,
            until,
        } => {
            let scope = resolve_scope(scope.or_else(|| cli.scope.clone()), cfg_default)?;
            let memory_id = nomiso::MemoryId::new(id.clone());
            let rows = vp
                .list_traces_for_memory(
                    &scope,
                    &memory_id,
                    nomiso::TracesByMemoryRequest {
                        scope: scope.clone(),
                        memory_id: memory_id.clone(),
                        since: since
                            .as_deref()
                            .map(vegapunk::parse_timestamp)
                            .transpose()
                            .map_err(|e| anyhow::anyhow!("{e}"))?,
                        until: until
                            .as_deref()
                            .map(vegapunk::parse_timestamp)
                            .transpose()
                            .map_err(|e| anyhow::anyhow!("{e}"))?,
                        limit,
                        session_id: cli.session_id.clone(),
                        turn_id: cli.turn_id.clone(),
                    },
                )
                .await?;
            print_out(
                &serde_json::json!({
                    "scope": scope,
                    "memory_id": id,
                    "count": rows.len(),
                    "traces": rows,
                }),
                fmt,
            )?;
            hints(
                cli,
                &[
                    "vegapunk get-trace --trace-id …",
                    "vegapunk trace-outcome --trace-id … --outcome helped",
                ],
            );
        }
        Commands::GetTrace { scope, trace_id } => {
            let scope = resolve_scope(scope.or_else(|| cli.scope.clone()), cfg_default)?;
            let bundle = vp.get_trace(&scope, &trace_id).await?;
            print_out(&bundle, fmt)?;
            hints(
                cli,
                &[
                    "vegapunk trace-inject --trace-id … --id …",
                    "vegapunk trace-outcome --trace-id … --outcome helped",
                ],
            );
        }
        Commands::PrepareContext {
            scope,
            task,
            entities,
            inventory,
            budget_tokens,
            budget_blocks,
            candidates,
            deadline_ms,
            effort,
            min_score,
            allow_partial,
            no_trace,
            as_of,
            known_as_of,
            sys_as_of,
            category,
            request_id,
        } => {
            let scope = resolve_scope(scope.or_else(|| cli.scope.clone()), cfg_default)?;
            let mut req = vegapunk::PrepareContextRequest::for_task(&scope, &task)
                .with_policy(resolved.profile.policy());
            req.request_id = request_id;
            req.entities = entities;
            let mut inv = Vec::with_capacity(inventory.len());
            for s in &inventory {
                let (id, version) = match s.split_once(':') {
                    None => (s.as_str(), None),
                    Some((i, "")) => (i, None),
                    Some((i, v)) => (
                        i,
                        Some(
                            v.parse::<u64>()
                                .map_err(|_| anyhow::anyhow!("bad --inventory version in '{s}'"))?,
                        ),
                    ),
                };
                inv.push(vegapunk::InventoryItem {
                    memory_id: nomiso::MemoryId::new(id),
                    version,
                });
            }
            req.inventory = inv;
            if let Some(t) = budget_tokens {
                req.budget.max_tokens = t;
            }
            if let Some(b) = budget_blocks {
                req.budget.max_blocks = b;
            }
            if let Some(c) = candidates {
                req.budget.max_candidates = c;
            }
            req.budget.deadline_ms = deadline_ms;
            req.effort = match effort.as_str() {
                "direct" => None,
                "expanded" => Some(vegapunk::Effort::Expanded(nomiso::GraphExpand::default())),
                other => bail!("--effort must be direct|expanded, got {other}"),
            };
            req.min_score = min_score;
            req.degradation = if allow_partial {
                vegapunk::DegradationPolicy::AllowPartial
            } else {
                vegapunk::DegradationPolicy::Strict
            };
            req.emit_trace = !no_trace;
            req.as_of = as_of
                .as_deref()
                .map(vegapunk::parse_timestamp)
                .transpose()
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            req.known_as_of = known_as_of
                .as_deref()
                .map(vegapunk::parse_timestamp)
                .transpose()
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            req.sys_as_of = sys_as_of
                .as_deref()
                .map(vegapunk::parse_timestamp)
                .transpose()
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            req.categories = category.map(|c| vec![c.into()]);
            let proposal = vegapunk::prepare_context(vp.client(), req).await?;
            print_out(&serde_json::to_value(&proposal)?, fmt)?;
            hints(
                cli,
                &[
                    "host inserts blocks itself; then acknowledge with record-insertion",
                    "vegapunk record-insertion --trace-id <trace_id> --proposal-id <proposal_id> --block <block_id> [--block …]",
                    "nothing is auto-injected; 'rendered' is derived from blocks only",
                ],
            );
        }
        Commands::RecordInsertion {
            scope,
            trace_id,
            proposal_id,
            host,
            blocks,
            inserted_json,
        } => {
            let scope = resolve_scope(scope.or_else(|| cli.scope.clone()), cfg_default)?;
            let mut inserted: Vec<vegapunk::InsertedBlock> = blocks
                .into_iter()
                .map(|b| vegapunk::InsertedBlock {
                    block_id: b,
                    truncated: false,
                    note: None,
                })
                .collect();
            if let Some(j) = inserted_json {
                let extra: Vec<vegapunk::InsertedBlock> = serde_json::from_str(&j)
                    .map_err(|e| anyhow::anyhow!("--inserted-json: {e}"))?;
                inserted.extend(extra);
            }
            let rec = vegapunk::record_insertion(
                vp.client(),
                vegapunk::InsertionAck {
                    trace_id,
                    proposal_id,
                    scope,
                    host,
                    inserted,
                    session_id: None,
                    turn_id: None,
                },
            )
            .await?;
            print_out(&serde_json::to_value(&rec)?, fmt)?;
            hints(
                cli,
                &["vegapunk trace-outcome --trace-id … --outcome helped|harmed|unknown|skipped"],
            );
        }
        Commands::TraceInject {
            scope,
            trace_id,
            ids,
            note,
        } => {
            let scope = resolve_scope(scope.or_else(|| cli.scope.clone()), cfg_default)?;
            let mems: Vec<nomiso::MemoryId> = ids.into_iter().map(nomiso::MemoryId::new).collect();
            vp.record_inject(&scope, &trace_id, &mems, note.as_deref())
                .await?;
            print_out(
                &serde_json::json!({
                    "status": "ok",
                    "trace_id": trace_id,
                    "injected": mems.len(),
                }),
                fmt,
            )?;
            hints(
                cli,
                &["vegapunk trace-outcome --trace-id … --outcome helped"],
            );
        }
        Commands::TraceOutcome {
            scope,
            trace_id,
            outcome,
            evaluator,
            note,
        } => {
            let scope = resolve_scope(scope.or_else(|| cli.scope.clone()), cfg_default)?;
            let o = match outcome.to_ascii_lowercase().as_str() {
                "helped" => vegapunk::TraceOutcome::Helped,
                "harmed" => vegapunk::TraceOutcome::Harmed,
                "unknown" => vegapunk::TraceOutcome::Unknown,
                "skipped" => vegapunk::TraceOutcome::Skipped,
                other => bail!("outcome must be helped|harmed|unknown|skipped, got {other}"),
            };
            let ev = match evaluator.to_ascii_lowercase().as_str() {
                "host" => vegapunk::Evaluator::Host,
                "model" => vegapunk::Evaluator::Model,
                "execution" => vegapunk::Evaluator::Execution,
                other => bail!("evaluator must be host|model|execution, got {other}"),
            };
            vp.record_trace_outcome_attributed(&scope, &trace_id, o, ev, note.as_deref())
                .await?;
            print_out(
                &serde_json::json!({ "status": "ok", "trace_id": trace_id, "outcome": outcome }),
                fmt,
            )?;
        }
        Commands::Candidates {
            scope,
            query,
            limit,
        } => {
            let scope = resolve_scope(scope.or_else(|| cli.scope.clone()), cfg_default)?;
            let hits = vp.find_candidates(&scope, &query, limit).await?;
            print_out(&hits, fmt)?;
            hints(
                cli,
                &["vegapunk supersede --prior-id memory:… --expected-version N --text \"…\""],
            );
        }
        Commands::StoreArtifact {
            scope,
            file,
            media_type,
            source,
        } => {
            let scope = resolve_scope(scope.or_else(|| cli.scope.clone()), cfg_default)?;
            let bytes = std::fs::read(&file).with_context(|| format!("read {}", file.display()))?;
            let art = vp
                .store_artifact(vegapunk::StoreArtifactInput {
                    scope,
                    bytes,
                    media_type,
                    source,
                    trust: None,
                })
                .await?;
            print_out(&art, fmt)?;
        }
        Commands::IngestCompaction {
            scope,
            transcript,
            file,
            summary,
        } => {
            let scope = resolve_scope(scope.or_else(|| cli.scope.clone()), cfg_default)?;
            let transcript = load_text(transcript, file)?;
            let out = vp
                .ingest_compaction(vegapunk::CompactionIngest {
                    scope,
                    transcript,
                    summary,
                })
                .await?;
            print_out(&out, fmt)?;
            if !out.is_ok() {
                return Err(ReportedFailure.into());
            }
        }
        Commands::PutArtifact {
            scope,
            blake3,
            location,
            media_type,
            source,
        } => {
            let scope = resolve_scope(scope.or_else(|| cli.scope.clone()), cfg_default)?;
            let art = vp
                .put_artifact(nomiso::PutArtifactRequest {
                    scope: scope.clone(),
                    blake3,
                    location,
                    media_type,
                    source,
                    trust: None,
                })
                .await?;
            print_out(&art, fmt)?;
        }
        Commands::History { scope, id } => {
            let scope = resolve_scope(scope.or_else(|| cli.scope.clone()), cfg_default)?;
            let chain = vp.history(&scope, nomiso::MemoryId::new(id)).await?;
            let cards: Vec<serde_json::Value> = chain
                .iter()
                .map(|e| {
                    serde_json::json!({
                        "id": e.record.id.to_string(),
                        "version": e.record.version,
                        "preview": truncate(&e.record.content.text, 120),
                        "valid_from": e.record.valid_from.to_string(),
                        "valid_until": e.record.valid_until.map(|t| t.to_string()),
                        "stale": e.record.stale,
                    })
                })
                .collect();
            print_out(
                &serde_json::json!({ "scope": scope, "history": cards }),
                fmt,
            )?;
        }
        Commands::WriteEpisode { scope, text, file } => {
            let scope = resolve_scope(scope.or_else(|| cli.scope.clone()), cfg_default)?;
            let body = load_text(text, file)?;
            let out = vp
                .write_episode(WriteEpisode {
                    scope,
                    text: body,
                    source: Some("vegapunk-cli".into()),
                })
                .await?;
            print_out(&out, fmt)?;
            hints(
                cli,
                &[
                    "agents: prefer host extract → vegapunk apply-ops --file ops.json",
                    "write-episode is rule/MemoryWriter demo path",
                ],
            );
            if !out.iter().all(|o| o.is_ok()) {
                return Err(ReportedFailure.into());
            }
        }
        Commands::WorkingState {
            scope,
            put_json,
            expected_version,
        } => {
            let scope = resolve_scope(scope.or_else(|| cli.scope.clone()), cfg_default)?;
            if let Some(raw) = put_json {
                let body: serde_json::Value = serde_json::from_str(&raw)
                    .map_err(|e| anyhow::anyhow!("put_json must be JSON object: {e}"))?;
                let rec = vp.put_working_state(&scope, body, expected_version).await?;
                print_out(
                    &serde_json::json!({
                        "status": "ok",
                        "scope": scope,
                        "slot": "coding-wm",
                        "version": rec.version,
                        "body": rec.body,
                        "sys_updated": rec.sys_updated.to_string(),
                    }),
                    fmt,
                )?;
            } else {
                let got = vp.get_working_state(&scope).await?;
                match got {
                    Some(rec) => print_out(
                        &serde_json::json!({
                            "status": "ok",
                            "scope": scope,
                            "slot": rec.slot,
                            "version": rec.version,
                            "body": rec.body,
                            "sys_created": rec.sys_created.to_string(),
                            "sys_updated": rec.sys_updated.to_string(),
                            "restore_working_state": resolved.profile.policy().restore_working_state,
                        }),
                        fmt,
                    )?,
                    None => print_out(
                        &serde_json::json!({
                            "status": "empty",
                            "scope": scope,
                            "slot": "coding-wm",
                            "restore_working_state": resolved.profile.policy().restore_working_state,
                            "hint": "vegapunk working-state --put-json '{\"goal\":\"…\"}'",
                        }),
                        fmt,
                    )?,
                }
            }
        }
        Commands::Read { scope, ids } => {
            let scope = resolve_scope(scope.or_else(|| cli.scope.clone()), cfg_default)?;
            let records = vp
                .client()
                .read(nomiso::ReadRequest {
                    ids: ids.into_iter().map(nomiso::MemoryId::new).collect(),
                    scope,
                    scope_match: nomiso::ScopeMatch::Exact,
                    as_of: None,
                    known_as_of: None,
                    sys_as_of: None,
                })
                .await?;
            print_out(&records, fmt)?;
        }
        Commands::BeliefEvents { scope, id } => {
            let scope = resolve_scope(scope.or_else(|| cli.scope.clone()), cfg_default)?;
            let events = vp
                .client()
                .list_belief_events(&nomiso::MemoryId::new(id.clone()), &scope)
                .await?;
            print_out(
                &serde_json::json!({
                    "scope": scope,
                    "memory_id": id,
                    "count": events.len(),
                    "events": events,
                }),
                fmt,
            )?;
            hints(
                cli,
                &[
                    "vegapunk history --id …",
                    "distinct from get-trace (use flywheel)",
                ],
            );
        }
        Commands::EntityPut {
            scope,
            kind,
            name,
            alias,
            attrs,
        } => {
            let scope = resolve_scope(scope.or_else(|| cli.scope.clone()), cfg_default)?;
            let rec = vp
                .client()
                .put_entity(nomiso::PutEntityRequest {
                    scope,
                    kind,
                    name,
                    aliases: alias,
                    attrs: match attrs {
                        Some(a) => Some(json_arg(&a, "attrs")?),
                        None => None,
                    },
                })
                .await?;
            print_out(&rec, fmt)?;
        }
        Commands::EntityGet { scope, id } => {
            let scope = resolve_scope(scope.or_else(|| cli.scope.clone()), cfg_default)?;
            let rec = vp.client().get_entity(&id, &scope).await?;
            print_out(&rec, fmt)?;
        }
        Commands::EntityUpdate {
            scope,
            id,
            expected_version,
            name,
            alias,
            attrs,
        } => {
            let scope = resolve_scope(scope.or_else(|| cli.scope.clone()), cfg_default)?;
            let rec = vp
                .client()
                .update_entity(nomiso::UpdateEntityRequest {
                    id,
                    scope,
                    expected_version,
                    name,
                    aliases: if alias.is_empty() { None } else { Some(alias) },
                    attrs: match attrs {
                        Some(a) => Some(json_arg(&a, "attrs")?),
                        None => None,
                    },
                })
                .await?;
            print_out(&rec, fmt)?;
        }
        Commands::RelPut {
            scope,
            predicate,
            from,
            to,
            subject_rev,
            object_rev,
            epistemic,
            evidence,
        } => {
            let scope = resolve_scope(scope.or_else(|| cli.scope.clone()), cfg_default)?;
            let w = vp
                .client()
                .put_relationship(nomiso::PutRelationshipRequest {
                    scope,
                    predicate: snake_enum(&predicate, "predicate")?,
                    subject: parse_endpoint(&from)?,
                    object: parse_endpoint(&to)?,
                    subject_rev,
                    object_rev,
                    epistemic: match epistemic {
                        Some(e) => snake_enum(&e, "epistemic")?,
                        None => Default::default(),
                    },
                    evidence: evidence
                        .iter()
                        .map(|e| {
                            let r = parse_endpoint(e)?;
                            Ok::<_, anyhow::Error>(nomiso::EvidenceRef {
                                kind: r.kind,
                                id: r.id,
                                revision: None,
                            })
                        })
                        .collect::<Result<Vec<_>>>()?,
                    valid_from: None,
                    valid_until: None,
                    producer: None,
                    dedupe: true,
                })
                .await?;
            print_out(&w, fmt)?;
        }
        Commands::RelUpdate {
            scope,
            id,
            expected_version,
            state,
            epistemic,
            reason,
        } => {
            let scope = resolve_scope(scope.or_else(|| cli.scope.clone()), cfg_default)?;
            let rec = vp
                .client()
                .update_relationship(nomiso::UpdateRelationshipRequest {
                    id,
                    scope,
                    expected_version,
                    epistemic: match epistemic {
                        Some(e) => Some(snake_enum(&e, "epistemic")?),
                        None => None,
                    },
                    state: match state {
                        Some(s) => Some(snake_enum(&s, "state")?),
                        None => None,
                    },
                    state_reason: reason,
                    evidence: None,
                    valid_until: None,
                })
                .await?;
            print_out(&rec, fmt)?;
        }
        Commands::RelList {
            scope,
            endpoint_ref,
            predicate,
            state,
            limit,
        } => {
            let scope = resolve_scope(scope.or_else(|| cli.scope.clone()), cfg_default)?;
            let rows = vp
                .client()
                .list_relationships(nomiso::ListRelationshipsRequest {
                    scope,
                    endpoint: match endpoint_ref {
                        Some(e) => Some(parse_endpoint(&e)?),
                        None => None,
                    },
                    predicate: match predicate {
                        Some(p) => Some(snake_enum(&p, "predicate")?),
                        None => None,
                    },
                    state: match state {
                        Some(s) => Some(snake_enum(&s, "state")?),
                        None => None,
                    },
                    limit,
                })
                .await?;
            print_out(&rows, fmt)?;
        }
        Commands::Traverse {
            scope,
            seed,
            predicate,
            direction,
            state,
            max_depth,
            max_visited,
            max_edges,
            deadline_ms,
        } => {
            let scope = resolve_scope(scope.or_else(|| cli.scope.clone()), cfg_default)?;
            if seed.is_empty() {
                bail!("traverse requires at least one --seed endpoint");
            }
            let result = vp
                .client()
                .traverse(nomiso::TraverseRequest {
                    scope,
                    seeds: seed
                        .iter()
                        .map(|s| parse_endpoint(s))
                        .collect::<Result<Vec<_>>>()?,
                    predicates: if predicate.is_empty() {
                        None
                    } else {
                        Some(
                            predicate
                                .iter()
                                .map(|p| snake_enum(p, "predicate"))
                                .collect::<Result<Vec<_>>>()?,
                        )
                    },
                    direction: match direction {
                        Some(d) => snake_enum(&d, "direction")?,
                        None => Default::default(),
                    },
                    states: if state.is_empty() {
                        None
                    } else {
                        Some(
                            state
                                .iter()
                                .map(|s| snake_enum(s, "state"))
                                .collect::<Result<Vec<_>>>()?,
                        )
                    },
                    budget: nomiso::TraverseBudget {
                        max_depth,
                        max_visited,
                        max_edges,
                        deadline_ms,
                    },
                })
                .await?;
            print_out(&result, fmt)?;
        }
        Commands::EmbedState => {
            let st = vp.client().embedding_state().await?;
            print_out(&st, fmt)?;
        }
        Commands::EmbedAttest {
            family,
            model,
            dimension,
            normalization,
            encoding,
            limitation,
        } => {
            let gen = vp
                .client()
                .attest_embedding_identity(embed_identity(
                    family,
                    model,
                    dimension.or(Some(resolved.embed_dim as u32)),
                    normalization,
                    encoding,
                    limitation,
                )?)
                .await?;
            print_out(&gen, fmt)?;
        }
        Commands::EmbedDeclare {
            family,
            model,
            dimension,
            normalization,
            encoding,
            limitation,
            note,
        } => {
            let gen = vp
                .client()
                .declare_embedding_generation(nomiso::DeclareGenerationRequest {
                    identity: embed_identity(
                        family,
                        model,
                        dimension.or(Some(resolved.embed_dim as u32)),
                        normalization,
                        encoding,
                        limitation,
                    )?,
                    note,
                })
                .await?;
            print_out(&gen, fmt)?;
            hints(
                cli,
                &[
                    "vegapunk job-enqueue --kind reindex --payload '<json>'",
                    "vegapunk worker-run --once",
                ],
            );
        }
        Commands::EmbedActivate { generation } => {
            let gen = vp
                .client()
                .activate_embedding_generation(generation)
                .await?;
            print_out(&gen, fmt)?;
        }
        Commands::StoreStats => {
            let tables = vp.client().table_counts().await?;
            print_out(
                &serde_json::json!({ "tables": tables, "schema_version": nomiso::SCHEMA_VERSION }),
                fmt,
            )?;
        }
        Commands::ProviderStatus => {
            let identity = vp.client().embedder().and_then(|e| e.identity());
            let status = vp.client().provider_status().await?;
            print_out(
                &serde_json::json!({ "provider": identity, "status": status }),
                fmt,
            )?;
        }
        Commands::JobEnqueue {
            scope,
            kind,
            payload,
            composition,
            inputs,
            max_attempts,
            lease_ms,
            retry_backoff_ms,
            deadline_ms,
            dedup_hint,
        } => {
            let scope = resolve_scope(scope.or_else(|| cli.scope.clone()), cfg_default)?;
            let r = vp
                .client()
                .enqueue_job(nomiso::EnqueueJobRequest {
                    scope,
                    kind,
                    inputs: match inputs {
                        Some(i) => serde_json::from_str(&i)
                            .with_context(|| "invalid --inputs JSON array")?,
                        None => vec![],
                    },
                    composition: match composition {
                        Some(c) => Some(json_arg(&c, "composition")?),
                        None => None,
                    },
                    payload: match payload {
                        Some(p) => Some(json_arg(&p, "payload")?),
                        None => None,
                    },
                    budget: nomiso::JobBudget {
                        max_attempts: max_attempts.unwrap_or(3),
                        lease_ms: lease_ms.unwrap_or(30_000),
                        retry_backoff_ms,
                        deadline_ms,
                    },
                    dedup_hint,
                })
                .await?;
            print_out(&r, fmt)?;
            hints(
                cli,
                &["vegapunk worker-run --once", "vegapunk job-get --id …"],
            );
        }
        Commands::JobGet { scope, id } => {
            let scope = resolve_scope(scope.or_else(|| cli.scope.clone()), cfg_default)?;
            let j = vp.client().get_job(&scope, &id).await?;
            print_out(&j, fmt)?;
        }
        Commands::JobList {
            scope,
            kind,
            state,
            limit,
        } => {
            let scope = resolve_scope(scope.or_else(|| cli.scope.clone()), cfg_default)?;
            let jobs = vp
                .client()
                .list_jobs(nomiso::ListJobsRequest {
                    scope,
                    kind,
                    state: match state {
                        Some(s) => Some(snake_enum(&s, "job state")?),
                        None => None,
                    },
                    limit,
                })
                .await?;
            print_out(&jobs, fmt)?;
        }
        Commands::JobClaim {
            worker,
            scope,
            kind,
        } => {
            let worker = worker.unwrap_or_else(|| "cli".into());
            let scopes = if scope.is_empty() {
                vec![resolve_scope(cli.scope.clone(), cfg_default)?]
            } else {
                scope
            };
            let lease = vp
                .client()
                .claim_job(nomiso::ClaimJobRequest {
                    worker,
                    scopes,
                    kinds: kind,
                })
                .await?;
            match lease {
                Some(l) => print_out(&l, fmt)?,
                None => print_out(&serde_json::json!({ "status": "no_eligible_jobs" }), fmt)?,
            }
        }
        Commands::JobRenew { id, fence, worker } => {
            let worker = worker.unwrap_or_else(|| "cli".into());
            let lease = vp.client().renew_job_lease(&id, fence, &worker).await?;
            print_out(&lease, fmt)?;
        }
        Commands::JobCheckpoint {
            id,
            fence,
            worker,
            value,
        } => {
            let worker = worker.unwrap_or_else(|| "cli".into());
            let j = vp
                .client()
                .checkpoint_job(&id, fence, &worker, json_arg(&value, "value")?)
                .await?;
            print_out(&j, fmt)?;
        }
        Commands::JobComplete {
            id,
            fence,
            worker,
            result,
        } => {
            let worker = worker.unwrap_or_else(|| "cli".into());
            let result = match result {
                Some(r) => json_arg(&r, "result")?,
                None => serde_json::json!({}),
            };
            let j = vp
                .client()
                .complete_job(&id, fence, &worker, result)
                .await?;
            print_out(&j, fmt)?;
        }
        Commands::JobFail {
            id,
            fence,
            worker,
            code,
            message,
            retryable,
        } => {
            let worker = worker.unwrap_or_else(|| "cli".into());
            let j = vp
                .client()
                .fail_job(
                    &id,
                    fence,
                    &worker,
                    nomiso::JobError {
                        code,
                        message,
                        retryable,
                    },
                )
                .await?;
            print_out(&j, fmt)?;
        }
        Commands::JobCancel { scope, id, reason } => {
            let scope = resolve_scope(scope.or_else(|| cli.scope.clone()), cfg_default)?;
            let j = vp
                .client()
                .cancel_job(
                    &scope,
                    &id,
                    reason.as_deref().unwrap_or("cancelled via cli"),
                )
                .await?;
            print_out(&j, fmt)?;
        }
        Commands::JobSupersede {
            scope,
            id,
            replacement_id,
            reason,
        } => {
            let scope = resolve_scope(scope.or_else(|| cli.scope.clone()), cfg_default)?;
            let j = vp
                .client()
                .supersede_job(
                    &scope,
                    &id,
                    &replacement_id,
                    reason.as_deref().unwrap_or("superseded via cli"),
                )
                .await?;
            print_out(&j, fmt)?;
        }
        Commands::WorkerRun {
            worker,
            scope,
            kind,
            once,
            concurrency,
            poll_ms,
            max_jobs,
        } => {
            let scopes = if scope.is_empty() {
                vec![resolve_scope(cli.scope.clone(), cfg_default)?]
            } else {
                scope
            };
            let kinds = if kind.is_empty() {
                vec!["reindex".to_string()]
            } else {
                kind
            };
            let mut w = nomiso::Worker::new(
                vp.client().clone(),
                nomiso::WorkerConfig {
                    worker: worker.unwrap_or_else(|| format!("vegapunk-{}", std::process::id())),
                    scopes,
                    poll_interval: std::time::Duration::from_millis(poll_ms),
                    concurrency,
                    max_jobs: if once { Some(1) } else { max_jobs },
                },
            );
            for k in &kinds {
                match k.as_str() {
                    "reindex" => {
                        let emb = vp.client().embedder().cloned().ok_or_else(|| {
                            anyhow::anyhow!(
                                "reindex executor requires an attached embedder (configure embed_http or hash embed)"
                            )
                        })?;
                        w.register(std::sync::Arc::new(nomiso::ReindexExecutor::new(emb)));
                    }
                    other => bail!("unknown built-in executor kind: {other}"),
                }
            }
            let stats = w.run().await?;
            print_out(&stats, fmt)?;
        }
    }

    Ok(())
}

/// Parse `kind:id` (bare ids default to `memory`) into an EndpointRef.
fn parse_endpoint(s: &str) -> Result<nomiso::EndpointRef> {
    let (kind_raw, id) = match s.split_once(':') {
        Some((k, i)) => (k, i.to_string()),
        None => ("memory", s.to_string()),
    };
    let kind: nomiso::EndpointKind =
        serde_json::from_value(serde_json::json!(kind_raw)).map_err(|_| {
            anyhow::anyhow!("endpoint kind must be memory|entity|artifact|span, got '{kind_raw}'")
        })?;
    Ok(nomiso::EndpointRef::new(kind, id))
}

/// Decode a snake_case enum flag into its typed variant.
fn snake_enum<T: serde::de::DeserializeOwned>(s: &str, what: &str) -> Result<T> {
    serde_json::from_value(serde_json::json!(s))
        .map_err(|_| anyhow::anyhow!("invalid {what}: '{s}'"))
}

/// Decode a JSON flag value.
fn json_arg(s: &str, what: &str) -> Result<serde_json::Value> {
    serde_json::from_str(s).with_context(|| format!("invalid --{what} JSON"))
}

/// Build an EmbeddingIdentity from CLI flags.
fn embed_identity(
    family: String,
    model: String,
    dimension: Option<u32>,
    normalization: Option<String>,
    encoding: Option<String>,
    limitation: Option<String>,
) -> Result<nomiso::EmbeddingIdentity> {
    Ok(nomiso::EmbeddingIdentity {
        family,
        model,
        dimension: dimension.unwrap_or(0),
        normalization: match normalization {
            Some(n) => snake_enum(&n, "normalization")?,
            None => nomiso::EmbeddingNormalization::None,
        },
        encoding: encoding.unwrap_or_else(|| "f32".into()),
        limitation,
    })
}

fn attach_writer(
    vp: Vegapunk,
    writer: &str,
    llm_bin: Option<&PathBuf>,
    llm_model: Option<&str>,
    llm_timeout: u64,
) -> Result<Vegapunk> {
    let w = writer.trim().to_ascii_lowercase();
    match w.as_str() {
        "rule" | "rules" | "none" | "" => Ok(vp.with_writer(Arc::new(RuleWriter))),
        "grok" | "codex" => {
            #[cfg(feature = "cli-llm")]
            {
                use vegapunk::CliChatWriter;
                let llm = make_cli_llm(&w, llm_bin, llm_model, llm_timeout)?;
                let mut writer = CliChatWriter::new(llm);
                if let Some(m) = llm_model {
                    writer = writer.with_model(m);
                }
                Ok(vp.with_writer(Arc::new(writer)))
            }
            #[cfg(not(feature = "cli-llm"))]
            {
                let _ = (llm_bin, llm_model, llm_timeout);
                bail!(
                    "writer '{writer}' is maintainer-only; rebuild with --features cli-llm \
                     (not part of shipped product — use apply-ops with host model extract)"
                );
            }
        }
        other => bail!("unknown --writer '{other}' (expected: rule | grok | codex)"),
    }
}

fn attach_rewriter(
    vp: Vegapunk,
    rewriter: &str,
    llm_bin: Option<&PathBuf>,
    llm_model: Option<&str>,
    llm_timeout: u64,
) -> Result<Vegapunk> {
    let r = rewriter.trim().to_ascii_lowercase();
    match r.as_str() {
        "rule" | "rules" | "none" | "" => Ok(vp),
        "grok" | "codex" => {
            #[cfg(feature = "cli-llm")]
            {
                use vegapunk::CliQueryRewriter;
                let llm = make_cli_llm(&r, llm_bin, llm_model, llm_timeout)?;
                let mut rw = CliQueryRewriter::new(llm);
                if let Some(m) = llm_model {
                    rw = rw.with_model(m);
                }
                Ok(vp.with_rewriter(Arc::new(rw)))
            }
            #[cfg(not(feature = "cli-llm"))]
            {
                let _ = (llm_bin, llm_model, llm_timeout);
                bail!("rewriter '{rewriter}' is maintainer-only; rebuild with --features cli-llm");
            }
        }
        other => bail!("unknown --rewriter '{other}' (expected: rule | grok | codex)"),
    }
}

#[cfg(feature = "cli-llm")]
fn make_cli_llm(
    kind: &str,
    llm_bin: Option<&PathBuf>,
    llm_model: Option<&str>,
    llm_timeout: u64,
) -> Result<Arc<vegapunk::CliLlm>> {
    use vegapunk::{CliBackendKind, CliLlm, CliLlmConfig};

    let kind = CliBackendKind::parse(kind).context("llm backend kind")?;
    let mut cfg = match kind {
        CliBackendKind::Grok => CliLlmConfig::grok(),
        CliBackendKind::Codex => CliLlmConfig::codex(),
    };
    if let Some(bin) = llm_bin {
        cfg = cfg.with_binary(bin.clone());
    }
    if let Some(m) = llm_model {
        cfg = cfg.with_model(m);
    }
    cfg = cfg.with_timeout(std::time::Duration::from_secs(llm_timeout));
    Ok(Arc::new(CliLlm::new(cfg)))
}

fn load_text(text: Option<String>, file: Option<PathBuf>) -> Result<String> {
    match (text, file) {
        (Some(t), None) => Ok(t),
        (None, Some(p)) => {
            std::fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))
        }
        (Some(_), Some(_)) => bail!("provide either --text or --file, not both"),
        (None, None) => bail!("provide --text or --file"),
    }
}

fn load_ops_json(json: Option<String>, file: Option<PathBuf>) -> Result<String> {
    match (json, file) {
        (Some(t), None) => Ok(t),
        (None, Some(p)) => {
            std::fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))
        }
        (Some(_), Some(_)) => bail!("provide either --json or --file, not both"),
        (None, None) => {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .context("read ops JSON from stdin")?;
            if buf.trim().is_empty() {
                bail!("provide --json, --file, or stdin WriterOp JSON array");
            }
            Ok(buf)
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{t}… (truncated, {n} chars total — use read --id <id>)")
    }
}
