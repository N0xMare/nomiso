//! Skill-track Hindsight retain compare on the coding-agent pack.
//!
//! Skip-honest when `EVAL_HINDSIGHT_URL` is unset, or when retain reports
//! zero extract tokens (plane / `LLM_PROVIDER=none` chunks mode).
//! Ingest is **transcript only** — never gold_memories text as retain content.

use serde::{Deserialize, Serialize};

use crate::coding_agent::CodingScenario;

/// One scenario's retain coverage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HindsightSkillRow {
    pub scenario: String,
    pub pattern: String,
    pub gold_n: usize,
    pub hit_n: usize,
    pub recall: f64,
    pub detail: String,
}

/// Hindsight retain report (never blended into plane hit@k).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HindsightSkillReport {
    pub track: String,
    pub extractor: String,
    pub skipped: Option<String>,
    pub rows: Vec<HindsightSkillRow>,
    pub passed: bool,
    pub recall_macro: f64,
}

impl HindsightSkillReport {
    /// Markdown table.
    pub fn markdown(&self) -> String {
        let mut s = format!(
            "# Coding skill `hindsight_retain`\n\ntrack={} passed={} skipped={:?} recall_macro={:.3}\n\n",
            self.track, self.passed, self.skipped, self.recall_macro
        );
        s.push_str("| scenario | pattern | gold | hit | recall |\n|---|---|---:|---:|---:|\n");
        for r in &self.rows {
            s.push_str(&format!(
                "| {} | {} | {} | {} | {:.2} |\n",
                r.scenario, r.pattern, r.gold_n, r.hit_n, r.recall
            ));
        }
        s
    }
}

/// Run retain+recall against local Hindsight. `Ok` skip report when unavailable.
pub async fn run_hindsight_retain_skill() -> Result<HindsightSkillReport, String> {
    let base = match std::env::var("EVAL_HINDSIGHT_URL") {
        Ok(u) if !u.trim().is_empty() => u.trim_end_matches('/').to_string(),
        _ => {
            return Ok(HindsightSkillReport {
                track: "skill".into(),
                extractor: "hindsight_retain".into(),
                skipped: Some("EVAL_HINDSIGHT_URL unset".into()),
                rows: vec![],
                passed: true,
                recall_macro: 0.0,
            });
        }
    };
    let prefix =
        std::env::var("EVAL_HINDSIGHT_BANK").unwrap_or_else(|_| "nomiso-coding-skill".into());
    let mut scenarios = crate::bundled_coding_scenarios().map_err(|e| e.to_string())?;
    if let Ok(n) = std::env::var("EVAL_HINDSIGHT_RETAIN_LIMIT") {
        if let Ok(n) = n.parse::<usize>() {
            scenarios.truncate(n);
        }
    }
    let http = reqwest::Client::new();

    let mut rows = Vec::new();
    let mut saw_extract = false;
    for s in &scenarios {
        if s.gold_memories.is_empty() && s.transcript.is_empty() {
            continue;
        }
        match retain_one(&http, &base, &prefix, s).await {
            Ok(RetainOutcome::NoTranscript) => {
                rows.push(HindsightSkillRow {
                    scenario: s.id.clone(),
                    pattern: s.pattern.clone(),
                    gold_n: s.gold_memories.len(),
                    hit_n: 0,
                    recall: 0.0,
                    detail: "skip: empty transcript (do not retain gold_memories as content)"
                        .into(),
                });
                continue;
            }
            Ok(RetainOutcome::Chunks) => {
                // Per-scenario: empty extract (Lightning sometimes returns
                // empty content). Do not abort the suite as plane-chunks.
                rows.push(HindsightSkillRow {
                    scenario: s.id.clone(),
                    pattern: s.pattern.clone(),
                    gold_n: s.gold_memories.len(),
                    hit_n: 0,
                    recall: 0.0,
                    detail: "retain output_tokens=0 (no extract this scenario)".into(),
                });
            }
            Ok(RetainOutcome::Extracted) => {
                saw_extract = true;
                rows.push(score_recall(&http, &base, &prefix, s).await?);
            }
            Err(e) => {
                // Per-scenario: 429 / upstream overload must not abort the pack.
                rows.push(HindsightSkillRow {
                    scenario: s.id.clone(),
                    pattern: s.pattern.clone(),
                    gold_n: s.gold_memories.len(),
                    hit_n: 0,
                    recall: 0.0,
                    detail: format!("retain transport: {e}"),
                });
            }
        }
    }
    if !saw_extract {
        return Ok(HindsightSkillReport {
            track: "skill".into(),
            extractor: "hindsight_retain".into(),
            skipped: Some(
                "hindsight retain extracted 0 tokens on every scenario (chunks / LLM_PROVIDER=none or empty LLM content)"
                    .into(),
            ),
            rows,
            passed: true,
            recall_macro: 0.0,
        });
    }
    let n = rows.len() as f64;
    let recall_macro = if n == 0.0 {
        0.0
    } else {
        rows.iter().map(|r| r.recall).sum::<f64>() / n
    };
    Ok(HindsightSkillReport {
        track: "skill".into(),
        extractor: "hindsight_retain".into(),
        skipped: None,
        rows,
        passed: true, // reported, not a plane gate
        recall_macro,
    })
}

enum RetainOutcome {
    Chunks,
    Extracted,
    /// Fair skill foil: never feed gold fact text as retain content.
    NoTranscript,
}

async fn retain_one(
    http: &reqwest::Client,
    base: &str,
    prefix: &str,
    s: &CodingScenario,
) -> Result<RetainOutcome, String> {
    let bank = format!("{prefix}-{}", s.id.replace('_', "-"));
    let put_bank = format!("{base}/v1/default/banks/{bank}");
    let _ = http
        .put(&put_bank)
        .json(&serde_json::json!({}))
        .send()
        .await;
    if s.transcript.is_empty() || s.transcript.iter().all(|t| t.text.trim().is_empty()) {
        return Ok(RetainOutcome::NoTranscript);
    }
    let content = s
        .transcript
        .iter()
        .map(|t| format!("{}: {}", t.role, t.text))
        .collect::<Vec<_>>()
        .join("\n");
    let url = format!("{base}/v1/default/banks/{bank}/memories");
    let resp = http
        .post(&url)
        .json(&serde_json::json!({
            "async": false,
            "items": [{
                "content": content,
                "document_id": format!("skill:{}", s.id),
                "update_mode": "replace",
            }]
        }))
        .send()
        .await
        .map_err(|e| format!("hindsight retain: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("hindsight retain {status}: {body}"));
    }
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("hindsight retain json: {e}"))?;
    let tokens = v
        .pointer("/usage/output_tokens")
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    if tokens == 0 {
        Ok(RetainOutcome::Chunks)
    } else {
        Ok(RetainOutcome::Extracted)
    }
}

async fn score_recall(
    http: &reqwest::Client,
    base: &str,
    prefix: &str,
    s: &CodingScenario,
) -> Result<HindsightSkillRow, String> {
    let bank = format!("{prefix}-{}", s.id.replace('_', "-"));
    let url = format!("{base}/v1/default/banks/{bank}/memories/recall");
    let query = s
        .probes
        .first()
        .map(|p| p.query.clone())
        .filter(|q| !q.trim().is_empty())
        .unwrap_or_else(|| {
            s.gold_memories
                .first()
                .map(|m| m.text.chars().take(80).collect())
                .unwrap_or_else(|| s.id.clone())
        });
    let resp = http
        .post(&url)
        .json(&serde_json::json!({
            "query": query,
            "types": ["world", "experience"],
            "budget": "mid",
            "max_tokens": 2048,
        }))
        .send()
        .await
        .map_err(|e| format!("hindsight recall: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("hindsight recall {}", resp.status()));
    }
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("hindsight recall json: {e}"))?;
    let texts: Vec<String> = v
        .get("results")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|r| r.get("text").and_then(|t| t.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let blob = texts.join("\n").to_ascii_lowercase();
    let gold_n = s.gold_memories.len();
    let hit_n = s
        .gold_memories
        .iter()
        .filter(|m| {
            let t = m.text.to_ascii_lowercase();
            let key: String = t.split_whitespace().take(6).collect::<Vec<_>>().join(" ");
            !key.is_empty() && blob.contains(&key)
        })
        .count();
    let recall = if gold_n == 0 {
        1.0
    } else {
        hit_n as f64 / gold_n as f64
    };
    Ok(HindsightSkillRow {
        scenario: s.id.clone(),
        pattern: s.pattern.clone(),
        gold_n,
        hit_n,
        recall,
        detail: format!("recall hits {hit_n}/{gold_n}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn coding_skill_hindsight_retain_skip_honest() {
        let report = run_hindsight_retain_skill().await.expect("run");
        eprintln!("{}", report.markdown());
        assert!(report.passed);
        if std::env::var("EVAL_HINDSIGHT_URL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .is_none()
        {
            assert!(report
                .skipped
                .as_deref()
                .unwrap_or("")
                .contains("EVAL_HINDSIGHT_URL"));
        }
    }
}
