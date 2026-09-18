//! Skill track for the coding-agent pack (`evals/coding_agent/`).
//!
//! Plane ingest stays in `nomiso-eval`. This module scores **remember**:
//! gold `WriterOp[]` apply, RuleWriter extract vs gold, optional live writer.
//! Hindsight retain compare lives in `nomiso-eval` (HTTP adapter).

use std::collections::HashMap;
use std::sync::Arc;

use nomiso::MemoryId;
use nomiso_embed::HashingEmbedder;
use nomiso_eval::CodingScenario;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::llm::parse_writer_ops_from_model;
use crate::writer::{apply_ops, ApplyResult, MemoryWriter, RuleWriter, WriteEpisode, WriterOp};
use crate::{Profile, Vegapunk};

/// How ops were produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillExtractor {
    /// Authoritative gold_ops applied (CI gate).
    GoldApply,
    /// Line-splitting RuleWriter (offline baseline; not a live host).
    RuleWriter,
    /// Host CLI writer when `cli-llm` + env are set.
    LiveHost,
}

/// Per-scenario extract/apply row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillRow {
    pub scenario: String,
    pub pattern: String,
    pub extractor: SkillExtractor,
    pub passed: bool,
    pub gold_n: usize,
    pub pred_n: usize,
    pub matched: usize,
    pub precision: f64,
    pub recall: f64,
    pub invented_prior: bool,
    pub uncertainty_ok: Option<bool>,
    pub detail: String,
}

/// One extractor's report (never blended with plane hit@k).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodingSkillReport {
    pub track: String,
    pub extractor: SkillExtractor,
    pub skipped: Option<String>,
    pub rows: Vec<SkillRow>,
    pub passed: bool,
    pub precision_macro: f64,
    pub recall_macro: f64,
    pub invented_prior_n: u32,
}

impl CodingSkillReport {
    /// Markdown table for last-miss / stdout.
    pub fn markdown(&self) -> String {
        let mut s = format!(
            "# Coding skill `{:?}`\n\ntrack={} passed={} skipped={:?}\n",
            self.extractor, self.track, self.passed, self.skipped
        );
        let passed_note = match self.extractor {
            SkillExtractor::GoldApply => "passed=all gold ops applied",
            _ => "passed=no invented prior; P/R is extract quality",
        };
        s.push_str(&format!(
            "precision_macro={:.3} recall_macro={:.3} invented_prior_n={} ({passed_note})\n\n",
            self.precision_macro, self.recall_macro, self.invented_prior_n
        ));
        s.push_str("| scenario | pattern | pass | P | R | gold | pred | hit | invented_prior |\n|---|---|---|---:|---:|---:|---:|---:|---|\n");
        for r in &self.rows {
            s.push_str(&format!(
                "| {} | {} | {} | {:.2} | {:.2} | {} | {} | {} | {} |\n",
                r.scenario,
                r.pattern,
                r.passed,
                r.precision,
                r.recall,
                r.gold_n,
                r.pred_n,
                r.matched,
                r.invented_prior
            ));
        }
        s
    }
}

/// Flatten transcript for extractors.
pub fn episode_text(s: &CodingScenario) -> String {
    if s.transcript.is_empty() {
        return s
            .gold_memories
            .iter()
            .map(|m| m.text.clone())
            .collect::<Vec<_>>()
            .join("\n");
    }
    s.transcript
        .iter()
        .map(|t| format!("{}: {}", t.role, t.text))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Gold apply (CI): remap `supersede.prior_id` gold keys → stored ids.
pub async fn run_gold_apply() -> Result<CodingSkillReport> {
    let scenarios = load_pack()?;
    let mut rows = Vec::new();
    for s in &scenarios {
        rows.push(apply_gold_scenario(s).await?);
    }
    Ok(summarize(SkillExtractor::GoldApply, rows, None))
}

/// RuleWriter extract vs gold_ops (reported; does not fail the gold gate).
pub async fn run_rule_extract() -> Result<CodingSkillReport> {
    let scenarios = load_pack()?;
    let writer = RuleWriter;
    let mut rows = Vec::new();
    for s in &scenarios {
        let episode = WriteEpisode {
            scope: s.scope.clone(),
            text: episode_text(s),
            source: Some("coding-skill-rule".into()),
        };
        let pred = writer.extract(&episode).await?;
        rows.push(score_extract(s, SkillExtractor::RuleWriter, &pred));
    }
    Ok(summarize(SkillExtractor::RuleWriter, rows, None))
}

/// Live host extract when `VEGAPUNK_SKILL_LIVE=1` and a writer is attached.
///
/// Without the env flag this returns a skipped report (suite stays green).
pub async fn run_live_extract(writer: Option<Arc<dyn MemoryWriter>>) -> Result<CodingSkillReport> {
    if !env_flag("VEGAPUNK_SKILL_LIVE") {
        return Ok(skipped(
            SkillExtractor::LiveHost,
            "VEGAPUNK_SKILL_LIVE unset (skip-honest)",
        ));
    }
    let dump = std::env::var_os("VEGAPUNK_SKILL_LIVE_DIR").map(std::path::PathBuf::from);
    live_extract(writer, dump.as_deref()).await
}

async fn live_extract(
    writer: Option<Arc<dyn MemoryWriter>>,
    dump_dir: Option<&std::path::Path>,
) -> Result<CodingSkillReport> {
    let Some(writer) = writer else {
        return Ok(skipped(
            SkillExtractor::LiveHost,
            "no MemoryWriter attached",
        ));
    };
    let scenarios = load_pack()?;
    let mut rows = Vec::new();
    for s in &scenarios {
        let episode = WriteEpisode {
            scope: s.scope.clone(),
            text: episode_text(s),
            source: Some("coding-skill-live".into()),
        };
        match writer.extract(&episode).await {
            Ok(pred) => {
                if let Some(dir) = dump_dir {
                    let path = dir.join(format!("{}.ops.json", s.id));
                    if let Err(e) = dump_pred(&path, &pred) {
                        tracing::warn!(path = %path.display(), error = %e, "failed to dump live pred");
                    }
                }
                rows.push(score_extract(s, SkillExtractor::LiveHost, &pred));
            }
            Err(e) => rows.push(SkillRow {
                scenario: s.id.clone(),
                pattern: s.pattern.clone(),
                extractor: SkillExtractor::LiveHost,
                passed: false,
                gold_n: mutating_gold_n(s),
                pred_n: 0,
                matched: 0,
                precision: 0.0,
                recall: 0.0,
                invented_prior: false,
                uncertainty_ok: None,
                detail: format!("extract error: {e}"),
            }),
        }
    }
    Ok(summarize(SkillExtractor::LiveHost, rows, None))
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|v| matches!(v.as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

fn dump_pred(path: &std::path::Path, pred: &[WriterOp]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::io(e.to_string()))?;
    }
    let js = serde_json::to_string_pretty(pred).map_err(|e| Error::invalid(e.to_string()))?;
    std::fs::write(path, js).map_err(|e| Error::io(e.to_string()))?;
    Ok(())
}

/// Score a directory of `{scenario_id}.ops.json` (host-written WriterOp[]).
pub async fn run_live_from_dir(dir: &std::path::Path) -> Result<CodingSkillReport> {
    if !dir.is_dir() {
        return Ok(skipped(
            SkillExtractor::LiveHost,
            &format!("live pred dir missing: {}", dir.display()),
        ));
    }
    let scenarios = load_pack()?;
    let mut rows = Vec::new();
    for s in &scenarios {
        let path = dir.join(format!("{}.ops.json", s.id));
        if !path.is_file() {
            rows.push(SkillRow {
                scenario: s.id.clone(),
                pattern: s.pattern.clone(),
                extractor: SkillExtractor::LiveHost,
                passed: false,
                gold_n: mutating_gold_n(s),
                pred_n: 0,
                matched: 0,
                precision: 0.0,
                recall: 0.0,
                invented_prior: false,
                uncertainty_ok: None,
                detail: format!("missing {}", path.display()),
            });
            continue;
        }
        let raw = std::fs::read_to_string(&path).map_err(|e| Error::io(e.to_string()))?;
        match parse_writer_ops_from_model(&raw) {
            Ok(pred) => rows.push(score_extract(s, SkillExtractor::LiveHost, &pred)),
            Err(e) => rows.push(SkillRow {
                scenario: s.id.clone(),
                pattern: s.pattern.clone(),
                extractor: SkillExtractor::LiveHost,
                passed: false,
                gold_n: mutating_gold_n(s),
                pred_n: 0,
                matched: 0,
                precision: 0.0,
                recall: 0.0,
                invented_prior: false,
                uncertainty_ok: None,
                detail: format!("unparseable {}: {e}", path.display()),
            }),
        }
    }
    Ok(summarize(SkillExtractor::LiveHost, rows, None))
}

/// Grok CLI extract over the pack (`cli-llm`). Writes preds under `out_dir` if set.
#[cfg(feature = "cli-llm")]
pub async fn run_grok_extract(out_dir: Option<&std::path::Path>) -> Result<CodingSkillReport> {
    use crate::cli_llm::{CliLlm, CliLlmConfig};
    use crate::CliChatWriter;

    let bin = std::env::var("VEGAPUNK_LLM_BIN").unwrap_or_else(|_| "grok".into());
    let model = std::env::var("VEGAPUNK_LLM_MODEL").ok();
    let mut cfg = CliLlmConfig::grok().with_binary(bin);
    if let Some(m) = model {
        cfg = cfg.with_model(m);
    }
    let writer: Arc<dyn MemoryWriter> = Arc::new(CliChatWriter::new(Arc::new(CliLlm::new(cfg))));
    if let Some(dir) = out_dir {
        std::fs::create_dir_all(dir).map_err(|e| Error::io(e.to_string()))?;
    }
    live_extract(Some(writer), out_dir).await
}

fn load_pack() -> Result<Vec<CodingScenario>> {
    let scenarios =
        nomiso_eval::bundled_coding_scenarios().map_err(|e| Error::invalid(e.to_string()))?;
    nomiso_eval::validate_coding_pack(&scenarios).map_err(|e| Error::invalid(e.to_string()))?;
    Ok(scenarios)
}

fn skipped(extractor: SkillExtractor, why: &str) -> CodingSkillReport {
    CodingSkillReport {
        track: "skill".into(),
        extractor,
        skipped: Some(why.into()),
        rows: vec![],
        passed: true,
        precision_macro: 0.0,
        recall_macro: 0.0,
        invented_prior_n: 0,
    }
}

fn summarize(
    extractor: SkillExtractor,
    rows: Vec<SkillRow>,
    skipped: Option<String>,
) -> CodingSkillReport {
    let n = rows.len() as f64;
    let precision_macro = if n == 0.0 {
        0.0
    } else {
        rows.iter().map(|r| r.precision).sum::<f64>() / n
    };
    let recall_macro = if n == 0.0 {
        0.0
    } else {
        rows.iter().map(|r| r.recall).sum::<f64>() / n
    };
    let invented_prior_n = rows.iter().filter(|r| r.invented_prior).count() as u32;
    let passed = rows.iter().all(|r| r.passed);
    CodingSkillReport {
        track: "skill".into(),
        extractor,
        skipped,
        rows,
        passed,
        precision_macro,
        recall_macro,
        invented_prior_n,
    }
}

async fn apply_gold_scenario(s: &CodingScenario) -> Result<SkillRow> {
    if s.gold_ops.is_empty() {
        return Ok(SkillRow {
            scenario: s.id.clone(),
            pattern: s.pattern.clone(),
            extractor: SkillExtractor::GoldApply,
            passed: true,
            gold_n: 0,
            pred_n: 0,
            matched: 0,
            precision: 1.0,
            recall: 1.0,
            invented_prior: false,
            uncertainty_ok: None,
            detail: "no gold_ops (vacuous)".into(),
        });
    }
    let raw = serde_json::to_string(&s.gold_ops).map_err(|e| Error::invalid(e.to_string()))?;
    let ops = parse_writer_ops_from_model(&raw)?;
    if ops.iter().any(empty_prior) {
        return Ok(fail_row(
            s,
            SkillExtractor::GoldApply,
            ops.len(),
            "empty prior_id",
        ));
    }

    let emb = HashingEmbedder::new(32).map_err(|e| Error::invalid(e.to_string()))?;
    let vp = Vegapunk::connect_memory(32)
        .await?
        .with_profile(Profile::CodingAgent)
        .with_embedder(Arc::new(emb));

    let mut key_ids: HashMap<String, (MemoryId, u64)> = HashMap::new();
    // One op per apply_ops so p07-style foreign scopes are legal.
    // This is not a batch active-scope pin proof.
    let mut applied = 0usize;
    let mut details = Vec::new();

    for (i, op) in ops.into_iter().enumerate() {
        let op = remap_supersede(op, &key_ids)?;
        let scope = crate::writer::op_scope(&op)
            .unwrap_or(s.scope.as_str())
            .to_string();
        let outcomes =
            apply_ops(vp.client(), vp.policy(), &scope, std::slice::from_ref(&op)).await?;
        let Some(out) = outcomes.into_iter().next() else {
            details.push(format!("op[{i}]: no outcome"));
            continue;
        };
        match out.ok_result() {
            Some(ApplyResult::Stored(o)) => {
                applied += 1;
                if let WriterOp::Put { input } | WriterOp::Supersede { new: input, .. } = &op {
                    if let Some(m) = s.gold_memories.iter().find(|m| m.text == input.text) {
                        key_ids.insert(m.key.clone(), (o.id.clone(), o.version));
                    }
                    if let WriterOp::Supersede { prior_id, .. } = &op {
                        let key = prior_id.as_str();
                        key_ids.insert(key.to_string(), (o.id.clone(), o.version));
                    }
                }
            }
            Some(ApplyResult::Noop { .. }) => {
                applied += 1;
            }
            Some(ApplyResult::Forgotten { .. }) => {
                applied += 1;
            }
            None => {
                details.push(format!("op[{i}]: {out:?}"));
            }
        }
    }

    let gold_n = s.gold_ops.len();
    let passed = details.is_empty() && applied == gold_n;
    Ok(SkillRow {
        scenario: s.id.clone(),
        pattern: s.pattern.clone(),
        extractor: SkillExtractor::GoldApply,
        passed,
        gold_n,
        pred_n: applied,
        matched: applied,
        precision: if gold_n == 0 {
            1.0
        } else {
            applied as f64 / gold_n as f64
        },
        recall: if gold_n == 0 {
            1.0
        } else {
            applied as f64 / gold_n as f64
        },
        invented_prior: false,
        uncertainty_ok: None,
        detail: if passed {
            "applied".into()
        } else {
            details.join("; ")
        },
    })
}

fn remap_supersede(op: WriterOp, key_ids: &HashMap<String, (MemoryId, u64)>) -> Result<WriterOp> {
    match op {
        WriterOp::Supersede {
            prior_id,
            expected_version,
            new,
        } => {
            let key = prior_id.as_str();
            let (id, ver) = key_ids.get(key).cloned().ok_or_else(|| {
                Error::invalid(format!(
                    "supersede prior_id '{key}' has no prior put in this scenario"
                ))
            })?;
            let ver = if expected_version == 0 {
                ver
            } else {
                expected_version
            };
            Ok(WriterOp::Supersede {
                prior_id: id,
                expected_version: ver,
                new,
            })
        }
        other => Ok(other),
    }
}

fn empty_prior(op: &WriterOp) -> bool {
    matches!(op, WriterOp::Supersede { prior_id, .. } if prior_id.as_str().trim().is_empty())
}

fn fail_row(
    s: &CodingScenario,
    extractor: SkillExtractor,
    pred_n: usize,
    detail: &str,
) -> SkillRow {
    SkillRow {
        scenario: s.id.clone(),
        pattern: s.pattern.clone(),
        extractor,
        passed: false,
        gold_n: mutating_gold_n(s),
        pred_n,
        matched: 0,
        precision: 0.0,
        recall: 0.0,
        invented_prior: true,
        uncertainty_ok: None,
        detail: detail.into(),
    }
}

fn mutating_gold_n(s: &CodingScenario) -> usize {
    extract_gold_ops(s)
        .iter()
        .filter(|v| {
            matches!(
                v.get("op").and_then(|o| o.as_str()),
                Some("put" | "supersede" | "forget")
            )
        })
        .count()
}

/// Ops a host may emit from the **episode** (no invented prior_id).
///
/// Plane `gold_ops` may include a closed prior + supersede. The transcript
/// usually only states the new fact, so extract gold is a `put` of that fact.
pub fn extract_gold_ops(s: &CodingScenario) -> Vec<serde_json::Value> {
    let episode = normalize(&episode_text(s));
    let has_supersede = s
        .gold_ops
        .iter()
        .any(|op| op.get("op").and_then(|o| o.as_str()) == Some("supersede"));
    let mut out = Vec::new();
    for op in &s.gold_ops {
        let kind = op.get("op").and_then(|o| o.as_str()).unwrap_or("");
        match kind {
            "supersede" => {
                let mut put = op.clone();
                if let Some(obj) = put.as_object_mut() {
                    obj.insert("op".into(), serde_json::json!("put"));
                    obj.remove("prior_id");
                    obj.remove("expected_version");
                }
                out.push(put);
            }
            "put" if !has_supersede => {
                if text_supported_by_episode(op, &episode) {
                    out.push(op.clone());
                }
            }
            "noop" => out.push(op.clone()),
            _ => {}
        }
    }
    if out.is_empty() {
        out.push(serde_json::json!({
            "op": "noop",
            "reason": "episode has no durable extract target"
        }));
    }
    out
}

fn text_supported_by_episode(op: &serde_json::Value, episode_norm: &str) -> bool {
    let t = op.get("text").and_then(|x| x.as_str()).unwrap_or("");
    let nt = normalize(t);
    // Pack scopes are `org/eval/coding/…`. Those crumbs must not keep a
    // plane-only sentence that the transcript never stated.
    let toks: Vec<&str> = nt
        .split_whitespace()
        .filter(|w| w.len() > 3 && !SUPPORT_STOP.contains(w))
        .collect();
    if toks.is_empty() {
        return false;
    }
    let hits = toks.iter().filter(|w| episode_norm.contains(**w)).count();
    if toks.len() < 3 {
        hits > 0
    } else {
        hits >= 3 || hits * 2 >= toks.len()
    }
}

const SUPPORT_STOP: &[&str] = &["org", "eval", "coding"];

fn score_extract(s: &CodingScenario, extractor: SkillExtractor, pred: &[WriterOp]) -> SkillRow {
    let gold_owned = extract_gold_ops(s);
    let gold: Vec<&serde_json::Value> = gold_owned.iter().collect();
    let gold_mut: Vec<&serde_json::Value> = gold
        .iter()
        .copied()
        .filter(|v| {
            matches!(
                v.get("op").and_then(|o| o.as_str()),
                Some("put" | "supersede" | "forget")
            )
        })
        .collect();

    let mut used = vec![false; pred.len()];
    let mut matched = 0usize;
    for g in &gold_mut {
        let gkind = g.get("op").and_then(|o| o.as_str()).unwrap_or("");
        let gtext = g.get("text").and_then(|t| t.as_str()).unwrap_or("");
        let gcat = g.get("category").and_then(|t| t.as_str());
        if let Some(i) = pred.iter().enumerate().position(|(i, p)| {
            !used[i]
                && kinds_match(gkind, p)
                && texts_match(gtext, pred_text(p))
                && cat_allows(gcat, p)
        }) {
            used[i] = true;
            matched += 1;
        }
    }

    let pred_mut: Vec<&WriterOp> = pred
        .iter()
        .filter(|p| !matches!(p, WriterOp::Noop { .. }))
        .collect();
    let gold_n = gold_mut.len();
    let pred_n = pred_mut.len();
    let precision = if pred_n == 0 {
        if gold_n == 0 {
            1.0
        } else {
            0.0
        }
    } else {
        matched as f64 / pred_n as f64
    };
    // Empty gold_mut + leftover puts = invented facts (abstain miss), not R=1.0.
    let recall = if gold_n == 0 {
        if pred_n == 0 {
            1.0
        } else {
            0.0
        }
    } else {
        matched as f64 / gold_n as f64
    };

    let episode = episode_text(s);
    let invented_prior = pred.iter().any(|p| match p {
        WriterOp::Supersede { prior_id, .. } => {
            let k = prior_id.as_str();
            k.trim().is_empty() || !episode.contains(k)
        }
        _ => false,
    });

    let gold_unc = gold
        .iter()
        .any(|v| v.get("category").and_then(|c| c.as_str()) == Some("uncertainty"));
    let uncertainty_ok = if gold_unc {
        Some(pred.iter().any(|p| {
            matches!(
                p,
                WriterOp::Put { input } if input.category == Some(nomiso::Category::Uncertainty)
            )
        }))
    } else {
        None
    };

    // Extract `passed` is discipline-only (no invented prior). Precision/recall are the score.
    let passed = !invented_prior;

    SkillRow {
        scenario: s.id.clone(),
        pattern: s.pattern.clone(),
        extractor,
        passed,
        gold_n,
        pred_n,
        matched,
        precision,
        recall,
        invented_prior,
        uncertainty_ok,
        detail: format!("matched {matched}/{gold_n} gold mutating ops"),
    }
}

fn kinds_match(gold: &str, pred: &WriterOp) -> bool {
    matches!(
        (gold, pred),
        ("put", WriterOp::Put { .. })
            | ("supersede", WriterOp::Supersede { .. })
            | ("forget", WriterOp::Forget { .. })
            | ("noop", WriterOp::Noop { .. })
    )
}

fn pred_text(op: &WriterOp) -> &str {
    match op {
        WriterOp::Put { input } | WriterOp::Supersede { new: input, .. } => input.text.as_str(),
        WriterOp::Noop { reason } => reason.as_deref().unwrap_or(""),
        WriterOp::Forget { .. } => "",
    }
}

fn cat_allows(gold: Option<&str>, pred: &WriterOp) -> bool {
    // Plane gold often labels error-lessons `episodic` and locks `procedural`.
    // Host extract may pick semantic/procedural/episodic for the same fact.
    // Only `uncertainty` is a writer-rule lock (do not pick a winner).
    let Some("uncertainty") = gold else {
        return true;
    };
    match pred {
        WriterOp::Put { input } | WriterOp::Supersede { new: input, .. } => {
            input.category == Some(nomiso::Category::Uncertainty)
        }
        _ => false,
    }
}

fn texts_match(gold: &str, pred: &str) -> bool {
    if gold.is_empty() || pred.is_empty() {
        return false;
    }
    let a = normalize(gold);
    let b = normalize(pred);
    if a.contains(&b) || b.contains(&a) {
        return true;
    }
    let ta: std::collections::HashSet<&str> =
        a.split_whitespace().filter(|w| w.len() > 3).collect();
    let tb: std::collections::HashSet<&str> =
        b.split_whitespace().filter(|w| w.len() > 3).collect();
    if ta.is_empty() || tb.is_empty() {
        return false;
    }
    let inter = ta.intersection(&tb).count();
    let shorter = ta.len().min(tb.len());
    inter * 2 >= shorter
}

fn normalize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || c.is_whitespace() {
                c
            } else {
                ' '
            }
        })
        .flat_map(|c| c.to_lowercase())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn coding_skill_gold_apply() {
        let report = run_gold_apply().await.expect("gold apply");
        eprintln!("{}", report.markdown());
        for r in &report.rows {
            assert!(r.passed, "{}: {}", r.scenario, r.detail);
        }
        assert!(report.passed);
    }

    #[tokio::test]
    async fn coding_skill_rule_writer_reports() {
        let report = run_rule_extract().await.expect("rule");
        eprintln!("{}", report.markdown());
        assert_eq!(report.extractor, SkillExtractor::RuleWriter);
        assert!(!report.rows.is_empty());
        // RuleWriter is a lexical baseline, not the host. Do not gate CI on P/R.
        assert_eq!(report.invented_prior_n, 0);
    }

    #[tokio::test]
    async fn coding_skill_live_skips_without_flag() {
        let report = live_extract(None, None).await.expect("skip");
        assert!(report
            .skipped
            .as_deref()
            .unwrap_or("")
            .contains("no MemoryWriter"));
        assert!(report.passed);
    }

    #[tokio::test]
    async fn coding_skill_live_env_skips_without_flag() {
        let report = run_live_extract(None).await.expect("skip");
        if report.skipped.as_deref().unwrap_or("").contains("unset") {
            assert!(report.passed);
        } else {
            // Parallel test may have set the env; still skip-honest without a writer.
            assert!(report
                .skipped
                .as_deref()
                .unwrap_or("")
                .contains("no MemoryWriter"));
        }
    }

    #[test]
    fn extract_gold_converts_supersede_and_drops_unsupported_put() {
        let s = nomiso_eval::bundled_coding_scenarios()
            .unwrap()
            .into_iter()
            .find(|s| s.id == "p02_supersede_formatter")
            .expect("p02");
        let g = extract_gold_ops(&s);
        assert_eq!(g.len(), 1, "{g:?}");
        assert_eq!(g[0]["op"], "put");
        assert!(g[0].get("prior_id").is_none());
        assert!(g[0]["text"].as_str().unwrap_or("").contains("clippy"));
    }

    #[test]
    fn extract_gold_drops_put_not_in_episode() {
        let s = nomiso_eval::CodingScenario {
            id: "unit_drop".into(),
            pattern: "p07_scope_isolation".into(),
            scope: "org/eval/coding/nomiso".into(),
            transcript: vec![
                nomiso_eval::TranscriptTurn {
                    role: "user".into(),
                    text: "Remember the sidecar staging token is rot13-not-real.".into(),
                },
                nomiso_eval::TranscriptTurn {
                    role: "assistant".into(),
                    text: "Stored only under org/eval/coding/sidecar.".into(),
                },
            ],
            gold_ops: vec![
                serde_json::json!({
                    "op": "put",
                    "scope": "org/eval/coding/nomiso",
                    "text": "Nomiso eval default_scope is org/eval/coding/nomiso. Do not read sidecar secrets.",
                    "category": "semantic"
                }),
                serde_json::json!({
                    "op": "put",
                    "scope": "org/eval/coding/sidecar",
                    "text": "Sidecar staging API token is rot13-not-real. Never copy into the nomiso coding scope.",
                    "category": "semantic"
                }),
            ],
            gold_memories: vec![],
            probes: vec![],
            notes: None,
            origin: None,
        };
        let g = extract_gold_ops(&s);
        assert_eq!(g.len(), 1, "{g:?}");
        assert!(g[0]["text"]
            .as_str()
            .unwrap_or("")
            .contains("rot13-not-real"));
    }

    #[test]
    fn extract_gold_p07_secret_is_noop() {
        let s = nomiso_eval::bundled_coding_scenarios()
            .unwrap()
            .into_iter()
            .find(|s| s.id == "p07_scope_isolation")
            .expect("p07");
        let g = extract_gold_ops(&s);
        assert_eq!(g.len(), 1, "{g:?}");
        assert_eq!(g[0]["op"], "noop");
    }

    #[test]
    fn extract_match_requires_text_not_just_category() {
        let gold = serde_json::json!([{
            "op": "put",
            "scope": "org/eval/coding/nomiso",
            "text": "vegapunk-cli is implemented in Rust.",
            "category": "semantic"
        }]);
        let pred = vec![WriterOp::Put {
            input: crate::RememberInput {
                scope: "org/eval/coding/nomiso".into(),
                text: "Format gate is rustfmt only.".into(),
                category: Some(nomiso::Category::Semantic),
                confidence: None,
                source: None,
                embedding: None,
                episodic: false,
                idempotency_key: None,
            },
        }];
        let s = nomiso_eval::CodingScenario {
            id: "unit_cat".into(),
            pattern: "p01_toolchain".into(),
            scope: "org/eval/coding/nomiso".into(),
            transcript: vec![nomiso_eval::TranscriptTurn {
                role: "assistant".into(),
                text: "vegapunk-cli is implemented in Rust.".into(),
            }],
            gold_ops: gold.as_array().cloned().unwrap_or_default(),
            gold_memories: vec![],
            probes: vec![],
            notes: None,
            origin: None,
        };
        let row = score_extract(&s, SkillExtractor::LiveHost, &pred);
        assert_eq!(
            row.matched, 0,
            "same category + different text must not match"
        );
        assert_eq!(row.precision, 0.0);
        assert_eq!(row.recall, 0.0);
    }

    #[test]
    fn extract_match_allows_plane_vs_host_category() {
        let gold = serde_json::json!([{
            "op": "put",
            "scope": "org/eval/coding/nomiso",
            "text": "Build failed with NOMI_SCOPE_DENIED_42 because apply-ops ran empty.",
            "category": "episodic"
        }]);
        let pred = vec![WriterOp::Put {
            input: crate::RememberInput {
                scope: "org/eval/coding/nomiso".into(),
                text: "Applying ops without a scope fails with NOMI_SCOPE_DENIED_42.".into(),
                category: Some(nomiso::Category::Semantic),
                confidence: None,
                source: None,
                embedding: None,
                episodic: false,
                idempotency_key: None,
            },
        }];
        let s = nomiso_eval::CodingScenario {
            id: "unit_cat_flex".into(),
            pattern: "p04_error_lesson".into(),
            scope: "org/eval/coding/nomiso".into(),
            transcript: vec![nomiso_eval::TranscriptTurn {
                role: "tool".into(),
                text: "Build failed with NOMI_SCOPE_DENIED_42 because apply-ops ran empty.".into(),
            }],
            gold_ops: gold.as_array().cloned().unwrap_or_default(),
            gold_memories: vec![],
            probes: vec![],
            notes: None,
            origin: None,
        };
        let row = score_extract(&s, SkillExtractor::LiveHost, &pred);
        assert_eq!(row.matched, 1, "episodic gold vs semantic host, same error");
    }

    #[test]
    fn extract_match_enforces_uncertainty_category() {
        let gold = serde_json::json!([{
            "op": "put",
            "scope": "org/eval/coding/nomiso",
            "text": "Unresolved conflict: rustfs vs FsBlobStore. No trusted prior_id.",
            "category": "uncertainty"
        }]);
        let pred = vec![WriterOp::Put {
            input: crate::RememberInput {
                scope: "org/eval/coding/nomiso".into(),
                text: "Unresolved conflict: rustfs vs FsBlobStore. No trusted prior_id.".into(),
                category: Some(nomiso::Category::Semantic),
                confidence: None,
                source: None,
                embedding: None,
                episodic: false,
                idempotency_key: None,
            },
        }];
        let s = nomiso_eval::CodingScenario {
            id: "unit_unc".into(),
            pattern: "p10_uncertainty".into(),
            scope: "org/eval/coding/nomiso".into(),
            transcript: vec![nomiso_eval::TranscriptTurn {
                role: "assistant".into(),
                text: "Unresolved conflict: rustfs vs FsBlobStore. No trusted prior_id.".into(),
            }],
            gold_ops: gold.as_array().cloned().unwrap_or_default(),
            gold_memories: vec![],
            probes: vec![],
            notes: None,
            origin: None,
        };
        let row = score_extract(&s, SkillExtractor::LiveHost, &pred);
        assert_eq!(
            row.matched, 0,
            "uncertainty gold must not match a winner put"
        );
        assert_eq!(row.uncertainty_ok, Some(false));
    }

    #[test]
    fn texts_match_hyphen_and_overlap() {
        assert!(texts_match(
            "process B vegapunk hard-recall on the same rocksdb path",
            "Process B hard-recalls from the same rocks path"
        ));
        assert!(texts_match(
            "Never auto-dump Nomiso memory into the prompt.",
            "Never auto-dump memory into the prompt."
        ));
        assert!(!texts_match(
            "vegapunk-cli is implemented in Rust.",
            "Format gate is rustfmt only."
        ));
    }

    #[test]
    fn extract_gold_p07b_is_transcript_supported() {
        let s = nomiso_eval::bundled_coding_scenarios()
            .unwrap()
            .into_iter()
            .find(|s| s.id == "p07b_scope_tact")
            .expect("p07b");
        let g = extract_gold_ops(&s);
        assert_eq!(g.len(), 1, "{g:?}");
        assert_eq!(g[0]["op"], "put");
        let text = g[0]["text"].as_str().unwrap_or("");
        assert!(text.contains("tact"), "{text}");
        assert!(
            !text.contains("TACT_ROOT_ONLY_SLOT"),
            "extract gold must not invent plane retrieval keys: {text}"
        );
    }

    #[tokio::test]
    async fn live_from_dir_scores_writer_ops_json() {
        let tmp = tempfile::tempdir().expect("tmp");
        let dir = tmp.path();
        let scenarios = load_pack().expect("pack");
        let s = scenarios
            .iter()
            .find(|x| x.id == "p11_abstain_unknown")
            .expect("p11");
        let path = dir.join(format!("{}.ops.json", s.id));
        std::fs::write(
            &path,
            r#"[{"op":"noop","reason":"probe token is not a convention"}]"#,
        )
        .expect("write");
        // Missing other scenarios → those rows fail-closed; p11 should score.
        let report = run_live_from_dir(dir).await.expect("score");
        let row = report
            .rows
            .iter()
            .find(|r| r.scenario == "p11_abstain_unknown")
            .expect("row");
        assert!(!row.invented_prior);
        assert_eq!(row.gold_n, 0);
        assert_eq!(row.pred_n, 0);
        assert_eq!(row.precision, 1.0);
        assert_eq!(row.recall, 1.0);
    }
}
