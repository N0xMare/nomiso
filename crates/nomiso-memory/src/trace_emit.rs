//! Emit plane traces from memory-toolkit paths.

use nomiso_core::{
    cap_preview, AppendTraceEvent, SearchHit, TraceEventKind, TraceHitCard, TraceOutcome,
};
use nomiso_service::NomisoClient;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::error::Result;
use crate::reader::{ContextPack, HardRecallResult};
use crate::types::RememberOutcome;
use crate::writer::{ApplyOpOutcome, ApplyResult};

/// Start a new trace id (uuid v7 string).
pub fn new_trace_id() -> String {
    Uuid::now_v7().to_string()
}

fn hit_cards(hits: &[SearchHit]) -> Vec<TraceHitCard> {
    hits.iter()
        .enumerate()
        .map(|(i, h)| TraceHitCard {
            id: h.id.clone(),
            rank: i as u32,
            score: h.score,
            score_kind: h.score_kind,
            preview: cap_preview(&h.preview),
        })
        .collect()
}

/// Record hard-recall search + pack under one trace_id.
pub async fn emit_hard_recall(
    client: &NomisoClient,
    scope: &str,
    trace_id: &str,
    session_id: Option<&str>,
    turn_id: Option<&str>,
    hr: &HardRecallResult,
    pack: Option<&ContextPack>,
) -> Result<()> {
    let cards = hit_cards(&hr.hits);
    client
        .append_trace_event(AppendTraceEvent {
            trace_id: trace_id.into(),
            scope: scope.into(),
            kind: TraceEventKind::Search,
            session_id: session_id.map(str::to_string),
            turn_id: turn_id.map(str::to_string),
            payload: Some(json!({
                "queries": hr.queries,
                "abstained": hr.abstained,
                "hits": cards,
            })),
            memory_ids: vec![],
        })
        .await?;
    if let Some(p) = pack {
        client
            .append_trace_event(AppendTraceEvent {
                trace_id: trace_id.into(),
                scope: scope.into(),
                kind: TraceEventKind::Pack,
                session_id: session_id.map(str::to_string),
                turn_id: turn_id.map(str::to_string),
                payload: Some(json!({
                    "abstained": p.abstained,
                    "estimated_tokens": p.estimated_tokens,
                    "card_count": p.cards.len(),
                    // ids only — not full pack text (T7)
                    "memory_ids": p.cards.iter().map(|c| c.id.to_string()).collect::<Vec<_>>(),
                })),
                memory_ids: vec![],
            })
            .await?;
    }
    Ok(())
}

/// Record apply_ops write outcomes under a trace.
pub async fn emit_apply_ops(
    client: &NomisoClient,
    scope: &str,
    trace_id: &str,
    session_id: Option<&str>,
    turn_id: Option<&str>,
    outcomes: &[ApplyOpOutcome],
) -> Result<()> {
    let writes: Vec<_> = outcomes
        .iter()
        .filter_map(|o| match o {
            ApplyOpOutcome::Ok {
                index,
                result: ApplyResult::Stored(RememberOutcome { id, version, .. }),
            } => Some(json!({"index": index, "id": id.to_string(), "version": version})),
            ApplyOpOutcome::Ok {
                index,
                result: ApplyResult::Forgotten { id, .. },
            } => Some(json!({"index": index, "forgotten": id.to_string()})),
            ApplyOpOutcome::Err {
                index, error, code, ..
            } => Some(json!({"index": index, "error": error, "code": code})),
            _ => None,
        })
        .collect();
    client
        .append_trace_event(AppendTraceEvent {
            trace_id: trace_id.into(),
            scope: scope.into(),
            kind: TraceEventKind::Write,
            session_id: session_id.map(str::to_string),
            turn_id: turn_id.map(str::to_string),
            payload: Some(json!({ "ops": writes })),
            memory_ids: vec![],
        })
        .await?;
    Ok(())
}

/// Who evaluated the outcome (CTX-010): observed execution results, host
/// feedback, or model judgments stay distinguished — a helped/harmed label
/// alone is not causal proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Evaluator {
    /// The host harness observed the execution result.
    #[default]
    Host,
    /// A model judged the outcome (weaker evidence).
    Model,
    /// Directly observed execution signal (test pass, exit code).
    Execution,
}

impl Evaluator {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Model => "model",
            Self::Execution => "execution",
        }
    }
}

/// Host-reported outcome attach.
pub async fn emit_outcome(
    client: &NomisoClient,
    scope: &str,
    trace_id: &str,
    outcome: TraceOutcome,
    note: Option<&str>,
    session_id: Option<&str>,
    turn_id: Option<&str>,
) -> Result<()> {
    emit_outcome_attributed(
        client,
        scope,
        trace_id,
        outcome,
        OutcomeMeta {
            evaluator: Evaluator::Host,
            note,
            session_id,
            turn_id,
        },
    )
    .await
}

/// Attribution + correlation for an outcome event.
#[derive(Debug, Clone, Default)]
pub struct OutcomeMeta<'a> {
    /// Who evaluated the result (default `host`).
    pub evaluator: Evaluator,
    pub note: Option<&'a str>,
    pub session_id: Option<&'a str>,
    pub turn_id: Option<&'a str>,
}

/// Attributed outcome attach — distinguishes who evaluated the result.
pub async fn emit_outcome_attributed(
    client: &NomisoClient,
    scope: &str,
    trace_id: &str,
    outcome: TraceOutcome,
    meta: OutcomeMeta<'_>,
) -> Result<()> {
    let OutcomeMeta {
        evaluator,
        note,
        session_id,
        turn_id,
    } = meta;
    client
        .append_trace_event(AppendTraceEvent {
            trace_id: trace_id.into(),
            scope: scope.into(),
            kind: TraceEventKind::Outcome,
            session_id: session_id.map(str::to_string),
            turn_id: turn_id.map(str::to_string),
            payload: Some(json!({
                "outcome": outcome.as_str(),
                "evaluator": evaluator.as_str(),
                "note": note,
            })),
            memory_ids: vec![],
        })
        .await?;
    Ok(())
}

/// Host reports pack injection (policy stays host-side; this is the journal).
pub async fn emit_inject(
    client: &NomisoClient,
    scope: &str,
    trace_id: &str,
    memory_ids: &[nomiso_core::MemoryId],
    note: Option<&str>,
    session_id: Option<&str>,
    turn_id: Option<&str>,
) -> Result<()> {
    client
        .append_trace_event(AppendTraceEvent {
            trace_id: trace_id.into(),
            scope: scope.into(),
            kind: TraceEventKind::Inject,
            session_id: session_id.map(str::to_string),
            turn_id: turn_id.map(str::to_string),
            payload: Some(json!({
                "memory_ids": memory_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
                "note": note,
            })),
            memory_ids: memory_ids.to_vec(),
        })
        .await?;
    Ok(())
}

/// Convenience after a single remember.
pub async fn emit_remember(
    client: &NomisoClient,
    scope: &str,
    trace_id: &str,
    wr: &RememberOutcome,
    session_id: Option<&str>,
    turn_id: Option<&str>,
) -> Result<()> {
    client
        .append_trace_event(AppendTraceEvent {
            trace_id: trace_id.into(),
            scope: scope.into(),
            kind: TraceEventKind::Write,
            session_id: session_id.map(str::to_string),
            turn_id: turn_id.map(str::to_string),
            payload: Some(json!({
                "id": wr.id.to_string(),
                "version": wr.version,
            })),
            memory_ids: vec![],
        })
        .await?;
    Ok(())
}
