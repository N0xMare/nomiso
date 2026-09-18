//! Thin product MCP for Vegapunk (stdio).
//!
//! Product ops only — not the full Nomiso plane catalog (`nomisod` owns that).

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ServerHandler};
use serde::{Deserialize, Serialize};
use vegapunk::{parse_writer_ops_from_model, CheckpointInput, RememberInput, Vegapunk};

/// Product MCP server.
#[derive(Clone)]
pub struct VegapunkMcp {
    vp: Vegapunk,
    tool_router: ToolRouter<Self>,
}

impl VegapunkMcp {
    pub fn new(vp: Vegapunk) -> Self {
        Self {
            vp,
            tool_router: Self::tool_router(),
        }
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

fn err_text(e: impl Into<vegapunk::Error>) -> CallToolResult {
    let e: vegapunk::Error = e.into();
    CallToolResult::structured_error(serde_json::json!({
        "error": e.public_message(),
        "code": e.code(),
    }))
}

fn report_result(r: &vegapunk::ApplyOpsReport) -> CallToolResult {
    if r.is_ok() {
        json_text(r)
    } else {
        CallToolResult::structured_error(
            serde_json::to_value(r)
                .unwrap_or_else(|_| serde_json::json!({"error": "report serialization failed"})),
        )
    }
}

fn parse_category(raw: Option<&str>) -> Result<Option<nomiso::Category>, vegapunk::Error> {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) => match nomiso::Category::parse(s) {
            Some(nomiso::Category::Trace) | None => Err(vegapunk::Error::Invalid(format!(
                "category must be semantic|episodic|identity|procedural|uncertainty, got {s}"
            ))),
            Some(c) => Ok(Some(c)),
        },
    }
}

fn apply_lenses(
    as_of: &mut Option<nomiso::Timestamp>,
    known_as_of: &mut Option<nomiso::Timestamp>,
    sys_as_of: &mut Option<nomiso::Timestamp>,
    as_s: Option<&str>,
    known_s: Option<&str>,
    sys_s: Option<&str>,
) -> vegapunk::Result<()> {
    if let Some(s) = as_s {
        *as_of = Some(vegapunk::parse_timestamp(s)?);
    }
    if let Some(s) = known_s {
        *known_as_of = Some(vegapunk::parse_timestamp(s)?);
    }
    if let Some(s) = sys_s {
        *sys_as_of = Some(vegapunk::parse_timestamp(s)?);
    }
    Ok(())
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct RememberArgs {
    scope: String,
    text: String,
    #[serde(default)]
    idempotency_key: Option<String>,
    #[serde(default)]
    episodic: bool,
    /// Optional category (semantic|episodic|identity|procedural|uncertainty).
    #[serde(default)]
    category: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct HardRecallArgs {
    scope: String,
    query: String,
    /// When true (default), return context pack for host inject.
    #[serde(default = "default_true")]
    pack: bool,
    /// Abstain when best plane score is below this floor.
    #[serde(default)]
    min_score: Option<f64>,
    #[serde(default)]
    as_of: Option<String>,
    #[serde(default)]
    known_as_of: Option<String>,
    #[serde(default)]
    sys_as_of: Option<String>,
    /// Optional category filter (does not default to uncertainty).
    #[serde(default)]
    category: Option<String>,
    /// Overlay host session id for this call (else process --session-id).
    #[serde(default)]
    session_id: Option<String>,
    /// Overlay host turn id for this call (else process --turn-id).
    #[serde(default)]
    turn_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SleepArgs {
    scope: String,
    #[serde(default)]
    apply: bool,
    #[serde(default)]
    apply_age_out: bool,
    /// Age-out apply/proposal window in hours. Required to apply age-outs.
    #[serde(default)]
    older_than_hours: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct WorkingStateArgs {
    scope: String,
    /// When set, put this JSON body; else get.
    #[serde(default)]
    put_json: Option<serde_json::Value>,
    /// CAS update: expected current slot version. Omit for create-only write.
    #[serde(default)]
    expected_version: Option<u64>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ApplyOpsArgs {
    /// Active scope pin (required). All ops must target this scope.
    scope: String,
    /// WriterOp JSON array string from host extract (precision-first).
    ops_json: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    turn_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CheckpointArgs {
    scope: String,
    summary: String,
    #[serde(default)]
    force: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SupersedeArgs {
    scope: String,
    prior_id: String,
    expected_version: u64,
    text: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RecallArgs {
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
    /// Optional category filter (does not default to uncertainty).
    #[serde(default)]
    category: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListArgs {
    scope: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
    /// Resume enumeration from a previously returned next_cursor.
    #[serde(default)]
    cursor: Option<nomiso::ListCursor>,
    #[serde(default)]
    as_of: Option<String>,
    #[serde(default)]
    known_as_of: Option<String>,
    #[serde(default)]
    sys_as_of: Option<String>,
    /// Optional category filter (semantic|episodic|identity|procedural|uncertainty).
    #[serde(default)]
    category: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CountArgs {
    scope: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    as_of: Option<String>,
    #[serde(default)]
    known_as_of: Option<String>,
    #[serde(default)]
    sys_as_of: Option<String>,
    /// Optional category filter (semantic|episodic|identity|procedural|uncertainty).
    #[serde(default)]
    category: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListTracesArgs {
    scope: String,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    since: Option<String>,
    #[serde(default)]
    until: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    turn_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TracesByMemoryArgs {
    scope: String,
    memory_id: String,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    since: Option<String>,
    #[serde(default)]
    until: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    turn_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CandidatesArgs {
    scope: String,
    query: String,
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct StoreArtifactMcpArgs {
    scope: String,
    bytes_b64: String,
    #[serde(default)]
    media_type: Option<String>,
    #[serde(default)]
    source: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct IngestCompactionArgs {
    scope: String,
    transcript: String,
    #[serde(default)]
    summary: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GetTraceArgs {
    scope: String,
    trace_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TraceOutcomeArgs {
    scope: String,
    trace_id: String,
    /// helped | harmed | unknown | skipped
    outcome: String,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    turn_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TraceInjectArgs {
    scope: String,
    trace_id: String,
    /// Memory ids from the pack that were injected.
    #[serde(default)]
    memory_ids: Vec<String>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    turn_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct PutArtifactArgs {
    scope: String,
    blake3: String,
    location: String,
    #[serde(default = "default_media")]
    media_type: String,
    #[serde(default)]
    source: Option<String>,
}

fn default_media() -> String {
    "application/octet-stream".into()
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct HistoryArgs {
    scope: String,
    id: String,
}

#[tool_router(router = tool_router)]
impl VegapunkMcp {
    #[tool(
        name = "vegapunk_remember",
        description = "Store a durable fact/episode via Vegapunk (structured remember)."
    )]
    async fn vegapunk_remember(
        &self,
        Parameters(args): Parameters<RememberArgs>,
    ) -> CallToolResult {
        let category = match parse_category(args.category.as_deref()) {
            Ok(c) => c,
            Err(e) => return err_text(e),
        };
        match self
            .vp
            .remember(RememberInput {
                scope: args.scope,
                text: args.text,
                category,
                confidence: None,
                source: None,
                embedding: None,
                episodic: args.episodic,
                idempotency_key: args.idempotency_key,
            })
            .await
        {
            Ok(o) => json_text(&o),
            Err(e) => err_text(e),
        }
    }

    #[tool(
        name = "vegapunk_hard_recall_pack",
        description = "Multi-pass hard recall + context pack. Host injects pack.block only if non-empty. Never auto-injected."
    )]
    async fn vegapunk_hard_recall_pack(
        &self,
        Parameters(args): Parameters<HardRecallArgs>,
    ) -> CallToolResult {
        let mut opts = vegapunk::HardRecallOptions::from_policy(self.vp.policy());
        if let Some(floor) = args.min_score {
            opts.min_score.replace(floor);
        }
        if let Err(e) = apply_lenses(
            &mut opts.as_of,
            &mut opts.known_as_of,
            &mut opts.sys_as_of,
            args.as_of.as_deref(),
            args.known_as_of.as_deref(),
            args.sys_as_of.as_deref(),
        ) {
            return err_text(e);
        }
        match parse_category(args.category.as_deref()) {
            Ok(c) => opts.categories = c.map(|c| vec![c]),
            Err(e) => return err_text(e),
        }
        if args.pack {
            match self
                .vp
                .overlay_correlation(args.session_id.clone(), args.turn_id.clone())
                .hard_recall_pack_with(&args.scope, &args.query, opts)
                .await
            {
                Ok((hr, ctx)) => json_text(&serde_json::json!({
                    "abstained": hr.abstained || ctx.abstained,
                    "trace_id": hr.trace_id,
                    "queries": hr.queries,
                    "hit_count": hr.hits.len(),
                    "pack": ctx,
                    "inject_note": "Host injects pack.block only if non-empty; then vegapunk_trace_inject + vegapunk_trace_outcome",
                })),
                Err(e) => err_text(e),
            }
        } else {
            match self
                .vp
                .hard_recall_with(&args.scope, &args.query, opts)
                .await
            {
                Ok(hr) => json_text(&hr),
                Err(e) => err_text(e),
            }
        }
    }

    #[tool(
        name = "vegapunk_sleep",
        description = "Review-only duplicate proposals; apply=true only performs explicitly enabled age-out soft-forgets. No automatic near-duplicate deletion."
    )]
    async fn vegapunk_sleep(&self, Parameters(args): Parameters<SleepArgs>) -> CallToolResult {
        let older_than_secs = match args
            .older_than_hours
            .map(|h| {
                i64::try_from(h)
                    .ok()
                    .and_then(|h| h.checked_mul(3600))
                    .ok_or_else(|| {
                        vegapunk::Error::Invalid(format!("older_than_hours {h} overflows seconds"))
                    })
            })
            .transpose()
        {
            Ok(v) => v,
            Err(e) => return err_text(e),
        };
        let opts = vegapunk::SleepOptions {
            dry_run: !args.apply,
            apply_age_out: args.apply_age_out,
            older_than_secs,
            ..Default::default()
        };
        match self.vp.sleep_with(&args.scope, opts).await {
            Ok(r) => {
                if r.is_ok() {
                    json_text(&r)
                } else {
                    CallToolResult::structured_error(serde_json::to_value(&r).unwrap_or_else(
                        |_| serde_json::json!({"error": "report serialization failed"}),
                    ))
                }
            }
            Err(e) => err_text(e),
        }
    }

    #[tool(
        name = "vegapunk_working_state",
        description = "Get or put coding working-state slot (restore ON for coding profile; not soft-inject)."
    )]
    async fn vegapunk_working_state(
        &self,
        Parameters(args): Parameters<WorkingStateArgs>,
    ) -> CallToolResult {
        if let Some(body) = args.put_json {
            match self
                .vp
                .put_working_state(&args.scope, body, args.expected_version)
                .await
            {
                Ok(rec) => json_text(&rec),
                Err(e) => err_text(e),
            }
        } else if args.expected_version.is_some() {
            err_text(vegapunk::Error::Invalid(
                "expected_version requires put_json".into(),
            ))
        } else {
            match self.vp.restore_session_state(&args.scope).await {
                Ok(rec) => json_text(&rec),
                Err(e) => err_text(e),
            }
        }
    }

    #[tool(
        name = "vegapunk_apply_ops",
        description = "Commit host-extracted WriterOp JSON array (primary agent write path). Prefer precision; never invent prior_id."
    )]
    async fn vegapunk_apply_ops(
        &self,
        Parameters(args): Parameters<ApplyOpsArgs>,
    ) -> CallToolResult {
        let ops = match parse_writer_ops_from_model(&args.ops_json) {
            Ok(o) => o,
            Err(e) => return err_text(e),
        };
        match self
            .vp
            .overlay_correlation(args.session_id, args.turn_id)
            .apply_writer_ops(&args.scope, &ops)
            .await
        {
            Ok(o) => report_result(&o),
            Err(e) => err_text(e),
        }
    }

    #[tool(
        name = "vegapunk_checkpoint",
        description = "Checkpoint a durable unit of work (policy-gated)."
    )]
    async fn vegapunk_checkpoint(
        &self,
        Parameters(args): Parameters<CheckpointArgs>,
    ) -> CallToolResult {
        match self
            .vp
            .checkpoint(CheckpointInput {
                scope: args.scope,
                summary: args.summary,
                force: args.force,
            })
            .await
        {
            Ok(o) => json_text(&o),
            Err(e) => err_text(e),
        }
    }

    #[tool(
        name = "vegapunk_candidates",
        description = "Find open-validity priors (id+version) before Put vs Supersede."
    )]
    async fn vegapunk_candidates(
        &self,
        Parameters(args): Parameters<CandidatesArgs>,
    ) -> CallToolResult {
        match self
            .vp
            .find_candidates(&args.scope, &args.query, args.limit)
            .await
        {
            Ok(h) => json_text(&h),
            Err(e) => err_text(e),
        }
    }

    #[tool(
        name = "vegapunk_store_artifact",
        description = "CAS-put bytes then register artifact metadata. bytes_b64 is standard base64."
    )]
    async fn vegapunk_store_artifact(
        &self,
        Parameters(args): Parameters<StoreArtifactMcpArgs>,
    ) -> CallToolResult {
        let json = vegapunk::StoreArtifactJson {
            scope: args.scope,
            bytes_b64: args.bytes_b64,
            media_type: args
                .media_type
                .unwrap_or_else(|| "application/octet-stream".into()),
            source: args.source,
        };
        match json.into_input() {
            Ok(input) => match self.vp.store_artifact(input).await {
                Ok(a) => json_text(&a),
                Err(e) => err_text(e),
            },
            Err(e) => err_text(e),
        }
    }

    #[tool(
        name = "vegapunk_ingest_compaction",
        description = "Ingest host-extracted durable lines with prefix-preserving apply. Checkpoint only when all operations succeed; inspect partial outcomes."
    )]
    async fn vegapunk_ingest_compaction(
        &self,
        Parameters(args): Parameters<IngestCompactionArgs>,
    ) -> CallToolResult {
        match self
            .vp
            .ingest_compaction(vegapunk::CompactionIngest {
                scope: args.scope,
                transcript: args.transcript,
                summary: args.summary,
            })
            .await
        {
            Ok(o) => {
                if o.is_ok() {
                    json_text(&o)
                } else {
                    CallToolResult::structured_error(serde_json::to_value(&o).unwrap_or_else(
                        |_| serde_json::json!({"error": "report serialization failed"}),
                    ))
                }
            }
            Err(e) => err_text(e),
        }
    }

    #[tool(
        name = "vegapunk_supersede",
        description = "Supersede a prior memory by id + expected_version with new fact text."
    )]
    async fn vegapunk_supersede(
        &self,
        Parameters(args): Parameters<SupersedeArgs>,
    ) -> CallToolResult {
        match self
            .vp
            .supersede(
                nomiso::MemoryId::new(args.prior_id),
                args.expected_version,
                RememberInput::fact(args.scope, args.text),
            )
            .await
        {
            Ok(o) => json_text(&o),
            Err(e) => err_text(e),
        }
    }

    #[tool(
        name = "vegapunk_recall",
        description = "Single-pass hybrid recall (cards). Prefer hard_recall_pack for multipass."
    )]
    async fn vegapunk_recall(&self, Parameters(args): Parameters<RecallArgs>) -> CallToolResult {
        let mut opts = vegapunk::RecallOptions::from_policy(self.vp.policy());
        if let Some(l) = args.limit {
            opts.limit = l;
        }
        if let Err(e) = apply_lenses(
            &mut opts.as_of,
            &mut opts.known_as_of,
            &mut opts.sys_as_of,
            args.as_of.as_deref(),
            args.known_as_of.as_deref(),
            args.sys_as_of.as_deref(),
        ) {
            return err_text(e);
        }
        match parse_category(args.category.as_deref()) {
            Ok(c) => opts.categories = c.map(|c| vec![c]),
            Err(e) => return err_text(e),
        }
        match self.vp.recall_with(&args.scope, &args.query, opts).await {
            Ok(hits) => {
                let cards: Vec<_> = hits
                    .iter()
                    .map(|h| {
                        serde_json::json!({
                            "id": h.id.to_string(),
                            "score": h.score,
                            "preview": h.preview,
                            "version": h.version,
                        })
                    })
                    .collect();
                json_text(&serde_json::json!({
                    "count": cards.len(),
                    "hits": cards,
                }))
            }
            Err(e) => err_text(e),
        }
    }

    #[tool(
        name = "vegapunk_list",
        description = "List memories under scope (enumerate; optional text / category / temporal filters). Never auto-injects."
    )]
    async fn vegapunk_list(&self, Parameters(args): Parameters<ListArgs>) -> CallToolResult {
        let cats = match parse_category(args.category.as_deref()) {
            Ok(c) => c.map(|c| vec![c]),
            Err(e) => return err_text(e),
        };
        let mut opts = vegapunk::EnumerateOptions {
            text: args.text,
            limit: args.limit,
            categories: cats,
            cursor: args.cursor,
            ..Default::default()
        };
        if let Err(e) = apply_lenses(
            &mut opts.as_of,
            &mut opts.known_as_of,
            &mut opts.sys_as_of,
            args.as_of.as_deref(),
            args.known_as_of.as_deref(),
            args.sys_as_of.as_deref(),
        ) {
            return err_text(e);
        }
        match self.vp.list_with(&args.scope, opts).await {
            Ok(page) => json_text(&page),
            Err(e) => err_text(e),
        }
    }

    #[tool(
        name = "vegapunk_count",
        description = "Count memories under scope (optional text / category / temporal filters)."
    )]
    async fn vegapunk_count(&self, Parameters(args): Parameters<CountArgs>) -> CallToolResult {
        let cats = match parse_category(args.category.as_deref()) {
            Ok(c) => c.map(|c| vec![c]),
            Err(e) => return err_text(e),
        };
        let mut opts = vegapunk::EnumerateOptions {
            text: args.text,
            categories: cats,
            ..Default::default()
        };
        if let Err(e) = apply_lenses(
            &mut opts.as_of,
            &mut opts.known_as_of,
            &mut opts.sys_as_of,
            args.as_of.as_deref(),
            args.known_as_of.as_deref(),
            args.sys_as_of.as_deref(),
        ) {
            return err_text(e);
        }
        match self.vp.count_with(&args.scope, opts).await {
            Ok(n) => json_text(&serde_json::json!({ "scope": args.scope, "count": n })),
            Err(e) => err_text(e),
        }
    }

    #[tool(
        name = "vegapunk_list_traces",
        description = "List parent traces in a scope (optional session_id/turn_id filter). Then vegapunk_get_trace / vegapunk_trace_inject / vegapunk_trace_outcome."
    )]
    async fn vegapunk_list_traces(
        &self,
        Parameters(args): Parameters<ListTracesArgs>,
    ) -> CallToolResult {
        let since = match args
            .since
            .as_deref()
            .map(vegapunk::parse_timestamp)
            .transpose()
        {
            Ok(t) => t,
            Err(e) => return err_text(e),
        };
        let until = match args
            .until
            .as_deref()
            .map(vegapunk::parse_timestamp)
            .transpose()
        {
            Ok(t) => t,
            Err(e) => return err_text(e),
        };
        match self
            .vp
            .list_traces(
                &args.scope,
                nomiso::ListTracesRequest {
                    scope: args.scope.clone(),
                    since,
                    until,
                    limit: args.limit,
                    session_id: args.session_id,
                    turn_id: args.turn_id,
                },
            )
            .await
        {
            Ok(rows) => json_text(&serde_json::json!({
                "scope": args.scope,
                "count": rows.len(),
                "traces": rows,
            })),
            Err(e) => err_text(e),
        }
    }

    #[tool(
        name = "vegapunk_traces_by_memory",
        description = "Traces that referenced a memory id, plus latest host outcome (helped/harmed/unknown/skipped)."
    )]
    async fn vegapunk_traces_by_memory(
        &self,
        Parameters(args): Parameters<TracesByMemoryArgs>,
    ) -> CallToolResult {
        let since = match args
            .since
            .as_deref()
            .map(vegapunk::parse_timestamp)
            .transpose()
        {
            Ok(t) => t,
            Err(e) => return err_text(e),
        };
        let until = match args
            .until
            .as_deref()
            .map(vegapunk::parse_timestamp)
            .transpose()
        {
            Ok(t) => t,
            Err(e) => return err_text(e),
        };
        let memory_id = nomiso::MemoryId::new(args.memory_id.clone());
        match self
            .vp
            .list_traces_for_memory(
                &args.scope,
                &memory_id,
                nomiso::TracesByMemoryRequest {
                    scope: args.scope.clone(),
                    memory_id: memory_id.clone(),
                    since,
                    until,
                    limit: args.limit,
                    session_id: args.session_id,
                    turn_id: args.turn_id,
                },
            )
            .await
        {
            Ok(rows) => json_text(&serde_json::json!({
                "scope": args.scope,
                "memory_id": args.memory_id,
                "count": rows.len(),
                "traces": rows,
            })),
            Err(e) => err_text(e),
        }
    }

    #[tool(
        name = "vegapunk_get_trace",
        description = "Load plane trace events for a trace_id under scope."
    )]
    async fn vegapunk_get_trace(
        &self,
        Parameters(args): Parameters<GetTraceArgs>,
    ) -> CallToolResult {
        match self.vp.get_trace(&args.scope, &args.trace_id).await {
            Ok(b) => json_text(&b),
            Err(e) => err_text(e),
        }
    }

    #[tool(
        name = "vegapunk_trace_inject",
        description = "Host reports pack injection (never auto-injected). Then vegapunk_trace_outcome."
    )]
    async fn vegapunk_trace_inject(
        &self,
        Parameters(args): Parameters<TraceInjectArgs>,
    ) -> CallToolResult {
        let mems: Vec<nomiso::MemoryId> = args
            .memory_ids
            .into_iter()
            .map(nomiso::MemoryId::new)
            .collect();
        match self
            .vp
            .overlay_correlation(args.session_id, args.turn_id)
            .record_inject(&args.scope, &args.trace_id, &mems, args.note.as_deref())
            .await
        {
            Ok(()) => json_text(&serde_json::json!({
                "status": "ok",
                "trace_id": args.trace_id,
                "injected": mems.len(),
            })),
            Err(e) => err_text(e),
        }
    }

    #[tool(
        name = "vegapunk_trace_outcome",
        description = "Host-reported outcome for a trace (helped|harmed|unknown|skipped). Plane does not infer."
    )]
    async fn vegapunk_trace_outcome(
        &self,
        Parameters(args): Parameters<TraceOutcomeArgs>,
    ) -> CallToolResult {
        let o = match args.outcome.to_ascii_lowercase().as_str() {
            "helped" => vegapunk::TraceOutcome::Helped,
            "harmed" => vegapunk::TraceOutcome::Harmed,
            "unknown" => vegapunk::TraceOutcome::Unknown,
            "skipped" => vegapunk::TraceOutcome::Skipped,
            other => {
                return err_text(vegapunk::Error::Invalid(format!("bad outcome {other}")));
            }
        };
        match self
            .vp
            .overlay_correlation(args.session_id, args.turn_id)
            .record_trace_outcome(&args.scope, &args.trace_id, o, args.note.as_deref())
            .await
        {
            Ok(()) => json_text(&serde_json::json!({
                "status": "ok",
                "trace_id": args.trace_id,
                "outcome": args.outcome,
            })),
            Err(e) => err_text(e),
        }
    }

    #[tool(
        name = "vegapunk_put_artifact",
        description = "Register artifact metadata (blake3 + location). Store bytes outside Surreal (Fs/S3/RustFS)."
    )]
    async fn vegapunk_put_artifact(
        &self,
        Parameters(args): Parameters<PutArtifactArgs>,
    ) -> CallToolResult {
        match self
            .vp
            .put_artifact(nomiso::PutArtifactRequest {
                scope: args.scope,
                blake3: args.blake3,
                location: args.location,
                media_type: args.media_type,
                source: args.source,
                trust: None,
            })
            .await
        {
            Ok(a) => json_text(&a),
            Err(e) => err_text(e),
        }
    }

    #[tool(
        name = "vegapunk_history",
        description = "Supersession history chain for a memory id."
    )]
    async fn vegapunk_history(&self, Parameters(args): Parameters<HistoryArgs>) -> CallToolResult {
        match self
            .vp
            .history(&args.scope, nomiso::MemoryId::new(args.id))
            .await
        {
            Ok(h) => json_text(&h),
            Err(e) => err_text(e),
        }
    }

    // --- Entities / relationships (REL-*) ---

    #[tool(
        name = "vegapunk_entity_put",
        description = "Create a scoped entity record (kind, name, aliases, attrs)."
    )]
    async fn vegapunk_entity_put(
        &self,
        Parameters(args): Parameters<nomiso::PutEntityRequest>,
    ) -> CallToolResult {
        match self.vp.client().put_entity(args).await {
            Ok(r) => json_text(&r),
            Err(e) => err_text(e),
        }
    }

    #[tool(
        name = "vegapunk_entity_get",
        description = "Get a scoped entity record by id."
    )]
    async fn vegapunk_entity_get(
        &self,
        Parameters(args): Parameters<ScopedIdArgs>,
    ) -> CallToolResult {
        match self.vp.client().get_entity(&args.id, &args.scope).await {
            Ok(r) => json_text(&r),
            Err(e) => err_text(e),
        }
    }

    #[tool(
        name = "vegapunk_entity_update",
        description = "CAS-update an entity record (expected_version guards lost updates)."
    )]
    async fn vegapunk_entity_update(
        &self,
        Parameters(args): Parameters<nomiso::UpdateEntityRequest>,
    ) -> CallToolResult {
        match self.vp.client().update_entity(args).await {
            Ok(r) => json_text(&r),
            Err(e) => err_text(e),
        }
    }

    #[tool(
        name = "vegapunk_relationship_put",
        description = "Assert a typed relationship between same-scope endpoints (closed predicate registry, optional revision pins + evidence). Idempotent on identical intent."
    )]
    async fn vegapunk_relationship_put(
        &self,
        Parameters(args): Parameters<nomiso::PutRelationshipRequest>,
    ) -> CallToolResult {
        match self.vp.client().put_relationship(args).await {
            Ok(w) => json_text(&w),
            Err(e) => err_text(e),
        }
    }

    #[tool(
        name = "vegapunk_relationship_update",
        description = "CAS-update a relationship: epistemic status, state transitions (closed/stale need reason), evidence replacement."
    )]
    async fn vegapunk_relationship_update(
        &self,
        Parameters(args): Parameters<nomiso::UpdateRelationshipRequest>,
    ) -> CallToolResult {
        match self.vp.client().update_relationship(args).await {
            Ok(r) => json_text(&r),
            Err(e) => err_text(e),
        }
    }

    #[tool(
        name = "vegapunk_relationship_list",
        description = "List relationships in a scope with optional endpoint, predicate, and state filters."
    )]
    async fn vegapunk_relationship_list(
        &self,
        Parameters(args): Parameters<nomiso::ListRelationshipsRequest>,
    ) -> CallToolResult {
        match self.vp.client().list_relationships(args).await {
            Ok(r) => json_text(&r),
            Err(e) => err_text(e),
        }
    }

    #[tool(
        name = "vegapunk_traverse",
        description = "Bounded graph traversal from seed endpoints: depth/visited/edge/deadline budgets, direction + predicate + state filters, path provenance + truncation reasons."
    )]
    async fn vegapunk_traverse(
        &self,
        Parameters(args): Parameters<nomiso::TraverseRequest>,
    ) -> CallToolResult {
        match self.vp.client().traverse(args).await {
            Ok(r) => json_text(&r),
            Err(e) => err_text(e),
        }
    }

    // --- Embedding administration (MIG-004/005) ---

    #[tool(
        name = "vegapunk_embedding_state",
        description = "Inspect embedding generations: identity, status, frontier, expected/embedded counts."
    )]
    async fn vegapunk_embedding_state(
        &self,
        Parameters(_args): Parameters<EmptyArgs>,
    ) -> CallToolResult {
        match self.vp.client().embedding_state().await {
            Ok(s) => json_text(&s),
            Err(e) => err_text(e),
        }
    }

    #[tool(
        name = "vegapunk_embedding_attest",
        description = "Attest the legacy (unknown-identity) active generation's model identity."
    )]
    async fn vegapunk_embedding_attest(
        &self,
        Parameters(args): Parameters<nomiso::EmbeddingIdentity>,
    ) -> CallToolResult {
        match self.vp.client().attest_embedding_identity(args).await {
            Ok(g) => json_text(&g),
            Err(e) => err_text(e),
        }
    }

    #[tool(
        name = "vegapunk_embedding_declare",
        description = "Declare a new staging embedding generation (captures source frontier + expected coverage)."
    )]
    async fn vegapunk_embedding_declare(
        &self,
        Parameters(args): Parameters<nomiso::DeclareGenerationRequest>,
    ) -> CallToolResult {
        match self.vp.client().declare_embedding_generation(args).await {
            Ok(g) => json_text(&g),
            Err(e) => err_text(e),
        }
    }

    #[tool(
        name = "vegapunk_embedding_stage",
        description = "Stage vectors into a declared generation; idempotent per (generation, memory), dimension-checked."
    )]
    async fn vegapunk_embedding_stage(
        &self,
        Parameters(args): Parameters<StageArgs>,
    ) -> CallToolResult {
        match self
            .vp
            .client()
            .stage_embeddings(args.generation, args.items)
            .await
        {
            Ok(n) => json_text(&serde_json::json!({ "staged": n })),
            Err(e) => err_text(e),
        }
    }

    #[tool(
        name = "vegapunk_embedding_activate",
        description = "Atomically activate a staged generation once coverage is complete (revalidates inside the transaction)."
    )]
    async fn vegapunk_embedding_activate(
        &self,
        Parameters(args): Parameters<GenerationArgs>,
    ) -> CallToolResult {
        match self
            .vp
            .client()
            .activate_embedding_generation(args.generation)
            .await
        {
            Ok(g) => json_text(&g),
            Err(e) => err_text(e),
        }
    }

    // --- Durable job journal (JOB-*) ---

    #[tool(
        name = "vegapunk_job_enqueue",
        description = "Enqueue a durable job: typed pinned inputs, composition identity, budget. Identical live intent deduplicates."
    )]
    async fn vegapunk_job_enqueue(
        &self,
        Parameters(args): Parameters<nomiso::EnqueueJobRequest>,
    ) -> CallToolResult {
        match self.vp.client().enqueue_job(args).await {
            Ok(r) => json_text(&r),
            Err(e) => err_text(e),
        }
    }

    #[tool(
        name = "vegapunk_job_get",
        description = "Get a full job record (checkpoint/result included) by scope + id."
    )]
    async fn vegapunk_job_get(&self, Parameters(args): Parameters<ScopedIdArgs>) -> CallToolResult {
        match self.vp.client().get_job(&args.scope, &args.id).await {
            Ok(j) => json_text(&j),
            Err(e) => err_text(e),
        }
    }

    #[tool(
        name = "vegapunk_job_list",
        description = "List job summaries for lifecycle inspection; raw payload bodies never included."
    )]
    async fn vegapunk_job_list(
        &self,
        Parameters(args): Parameters<nomiso::ListJobsRequest>,
    ) -> CallToolResult {
        match self.vp.client().list_jobs(args).await {
            Ok(v) => json_text(&v),
            Err(e) => err_text(e),
        }
    }

    #[tool(
        name = "vegapunk_job_claim",
        description = "Claim the next eligible job under a fenced lease, restricted to scope + kind grants (worker primitive)."
    )]
    async fn vegapunk_job_claim(
        &self,
        Parameters(args): Parameters<nomiso::ClaimJobRequest>,
    ) -> CallToolResult {
        match self.vp.client().claim_job(args).await {
            Ok(l) => json_text(&l),
            Err(e) => err_text(e),
        }
    }

    #[tool(
        name = "vegapunk_job_renew",
        description = "Renew a job's lease under its fencing token (worker primitive)."
    )]
    async fn vegapunk_job_renew(&self, Parameters(args): Parameters<FencedArgs>) -> CallToolResult {
        match self
            .vp
            .client()
            .renew_job_lease(&args.id, args.fence, &args.worker)
            .await
        {
            Ok(l) => json_text(&l),
            Err(e) => err_text(e),
        }
    }

    #[tool(
        name = "vegapunk_job_checkpoint",
        description = "Persist a bounded durable checkpoint under the active lease fence (worker primitive)."
    )]
    async fn vegapunk_job_checkpoint(
        &self,
        Parameters(args): Parameters<JobValueArgs>,
    ) -> CallToolResult {
        match self
            .vp
            .client()
            .checkpoint_job(&args.id, args.fence, &args.worker, args.value)
            .await
        {
            Ok(j) => json_text(&j),
            Err(e) => err_text(e),
        }
    }

    #[tool(
        name = "vegapunk_job_complete",
        description = "Complete a job under its fence; pinned input revisions revalidate atomically (worker primitive)."
    )]
    async fn vegapunk_job_complete(
        &self,
        Parameters(args): Parameters<JobValueArgs>,
    ) -> CallToolResult {
        match self
            .vp
            .client()
            .complete_job(&args.id, args.fence, &args.worker, args.value)
            .await
        {
            Ok(j) => json_text(&j),
            Err(e) => err_text(e),
        }
    }

    #[tool(
        name = "vegapunk_job_fail",
        description = "Record a job failure under its fence; retryable failures reschedule with backoff (worker primitive)."
    )]
    async fn vegapunk_job_fail(&self, Parameters(args): Parameters<JobFailArgs>) -> CallToolResult {
        match self
            .vp
            .client()
            .fail_job(&args.id, args.fence, &args.worker, args.error)
            .await
        {
            Ok(j) => json_text(&j),
            Err(e) => err_text(e),
        }
    }

    #[tool(
        name = "vegapunk_job_cancel",
        description = "Cancel a pending or leased job; committed checkpoints remain visible."
    )]
    async fn vegapunk_job_cancel(
        &self,
        Parameters(args): Parameters<JobReasonArgs>,
    ) -> CallToolResult {
        match self
            .vp
            .client()
            .cancel_job(&args.scope, &args.id, &args.reason)
            .await
        {
            Ok(j) => json_text(&j),
            Err(e) => err_text(e),
        }
    }

    #[tool(
        name = "vegapunk_job_supersede",
        description = "Supersede a pending/leased job by its replacement's id."
    )]
    async fn vegapunk_job_supersede(
        &self,
        Parameters(args): Parameters<JobSupersedeArgs>,
    ) -> CallToolResult {
        match self
            .vp
            .client()
            .supersede_job(&args.scope, &args.id, &args.replacement_id, &args.reason)
            .await
        {
            Ok(j) => json_text(&j),
            Err(e) => err_text(e),
        }
    }
}

/// Shared arg shapes for positional-parameter ops.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ScopedIdArgs {
    scope: String,
    id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct EmptyArgs {}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GenerationArgs {
    generation: u64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct StageArgs {
    generation: u64,
    items: Vec<nomiso::StagedEmbedding>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct FencedArgs {
    id: String,
    fence: u64,
    worker: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct JobValueArgs {
    id: String,
    fence: u64,
    worker: String,
    value: serde_json::Value,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct JobFailArgs {
    id: String,
    fence: u64,
    worker: String,
    error: nomiso::JobError,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct JobReasonArgs {
    scope: String,
    id: String,
    reason: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct JobSupersedeArgs {
    scope: String,
    id: String,
    replacement_id: String,
    reason: String,
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for VegapunkMcp {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.server_info = Implementation::new("vegapunk", env!("CARGO_PKG_VERSION"));
        info.instructions = Some(
            "Vegapunk product memory tools on Nomiso. Explicit pack inject only. \
             After pack: inject pack.block, vegapunk_trace_inject, then vegapunk_trace_outcome. \
             Optional session_id/turn_id overlay on flywheel tools. \
             Host models extract WriterOps; use vegapunk_apply_ops. Plane raw ops: nomisod."
                .into(),
        );
        info
    }
}

/// Exported tool names.
pub fn tool_names() -> &'static [&'static str] {
    &[
        "vegapunk_remember",
        "vegapunk_hard_recall_pack",
        "vegapunk_apply_ops",
        "vegapunk_checkpoint",
        "vegapunk_supersede",
        "vegapunk_recall",
        "vegapunk_list",
        "vegapunk_count",
        "vegapunk_get_trace",
        "vegapunk_list_traces",
        "vegapunk_traces_by_memory",
        "vegapunk_trace_inject",
        "vegapunk_trace_outcome",
        "vegapunk_put_artifact",
        "vegapunk_store_artifact",
        "vegapunk_candidates",
        "vegapunk_ingest_compaction",
        "vegapunk_history",
        "vegapunk_sleep",
        "vegapunk_working_state",
        "vegapunk_entity_put",
        "vegapunk_entity_get",
        "vegapunk_entity_update",
        "vegapunk_relationship_put",
        "vegapunk_relationship_update",
        "vegapunk_relationship_list",
        "vegapunk_traverse",
        "vegapunk_embedding_state",
        "vegapunk_embedding_attest",
        "vegapunk_embedding_declare",
        "vegapunk_embedding_stage",
        "vegapunk_embedding_activate",
        "vegapunk_job_enqueue",
        "vegapunk_job_get",
        "vegapunk_job_list",
        "vegapunk_job_claim",
        "vegapunk_job_renew",
        "vegapunk_job_checkpoint",
        "vegapunk_job_complete",
        "vegapunk_job_fail",
        "vegapunk_job_cancel",
        "vegapunk_job_supersede",
    ]
}

/// Serve product MCP over stdio.
pub async fn serve_stdio(vp: Vegapunk) -> anyhow::Result<()> {
    use rmcp::ServiceExt;
    let server = VegapunkMcp::new(vp);
    let transport = rmcp::transport::stdio();
    let service = server.serve(transport).await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "serve")]
    #[tokio::test]
    async fn keyed_put_replays_across_http_and_mcp() {
        let vp = Vegapunk::connect_memory(8).await.unwrap();
        let app = crate::http::router(crate::http::ServeState {
            vp: vp.clone(),
            api_key: None,
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        let response = client
            .post(format!("{base}/v1/remember"))
            .json(&serde_json::json!({
                "scope": "org/cross", "text": "same durable fact", "idempotency_key": "cross-1"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        let first: serde_json::Value = response.json().await.unwrap();
        let mcp = VegapunkMcp::new(vp);
        let replay = mcp
            .vegapunk_remember(Parameters(RememberArgs {
                scope: "org/cross".into(),
                text: "same durable fact".into(),
                episodic: false,
                category: None,
                idempotency_key: Some("cross-1".into()),
            }))
            .await;
        assert_eq!(replay.is_error, Some(false));
        let replay = replay.structured_content.unwrap();
        assert_eq!(replay["id"], first["id"]);
        assert_eq!(replay["replayed"], true);
        let changed = client.post(format!("{base}/v1/remember")).json(&serde_json::json!({
            "scope": "org/cross", "text": "same durable fact", "source": "explicit different source", "idempotency_key": "cross-1"
        })).send().await.unwrap();
        assert_eq!(changed.status().as_u16(), 409);
        assert_eq!(
            changed.json::<serde_json::Value>().await.unwrap()["code"],
            "idempotency_conflict"
        );
        server.abort();
    }

    #[test]
    fn tool_list() {
        let n = tool_names();
        assert!(n.contains(&"vegapunk_apply_ops"));
        assert!(n.contains(&"vegapunk_hard_recall_pack"));
        assert!(n.contains(&"vegapunk_list"));
        assert!(n.contains(&"vegapunk_get_trace"));
        assert!(n.contains(&"vegapunk_list_traces"));
        assert!(n.contains(&"vegapunk_traces_by_memory"));
        assert!(n.contains(&"vegapunk_trace_inject"));
        assert!(n.contains(&"vegapunk_trace_outcome"));
        assert!(n.contains(&"vegapunk_put_artifact"));
        assert!(n.contains(&"vegapunk_store_artifact"));
        assert!(n.contains(&"vegapunk_candidates"));
        assert!(n.contains(&"vegapunk_ingest_compaction"));
        assert!(n.contains(&"vegapunk_sleep"));
        assert!(n.contains(&"vegapunk_working_state"));
        assert!(n.contains(&"vegapunk_entity_put"));
        assert!(n.contains(&"vegapunk_entity_get"));
        assert!(n.contains(&"vegapunk_entity_update"));
        assert!(n.contains(&"vegapunk_relationship_put"));
        assert!(n.contains(&"vegapunk_relationship_update"));
        assert!(n.contains(&"vegapunk_relationship_list"));
        assert!(n.contains(&"vegapunk_traverse"));
        assert!(n.contains(&"vegapunk_embedding_state"));
        assert!(n.contains(&"vegapunk_embedding_attest"));
        assert!(n.contains(&"vegapunk_embedding_declare"));
        assert!(n.contains(&"vegapunk_embedding_stage"));
        assert!(n.contains(&"vegapunk_embedding_activate"));
        assert!(n.contains(&"vegapunk_job_enqueue"));
        assert!(n.contains(&"vegapunk_job_get"));
        assert!(n.contains(&"vegapunk_job_list"));
        assert!(n.contains(&"vegapunk_job_claim"));
        assert!(n.contains(&"vegapunk_job_renew"));
        assert!(n.contains(&"vegapunk_job_checkpoint"));
        assert!(n.contains(&"vegapunk_job_complete"));
        assert!(n.contains(&"vegapunk_job_fail"));
        assert!(n.contains(&"vegapunk_job_cancel"));
        assert!(n.contains(&"vegapunk_job_supersede"));
        assert_eq!(n.len(), 42);
    }
}
