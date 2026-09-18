//! Phase C0 offline skill / dual-plane eval harness (no live model calls).
//!
//! Scores **writer parse + apply**, **rule writer**, and **hard_recall multipass**
//! separately from the Nomiso plane harness in `nomiso-eval`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use nomiso_embed::HashingEmbedder;
use serde::{Deserialize, Serialize};

use crate::cli_writer::CliChatWriter;
use crate::error::{Error, Result};
use crate::llm::{parse_writer_ops_from_model, MockLlm};
use crate::reader::{hard_recall, HardRecallOptions, RuleQueryRewriter};
use crate::recall::EnumerateOptions;
use crate::remember::RememberInput;
use crate::writer::{apply_ops, ApplyResult, MemoryWriter, RuleWriter, WriteEpisode, WriterOp};
use crate::{Profile, TraceOutcome, Vegapunk};

/// One write-skill case (JSONL line or suite entry).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteCase {
    pub name: String,
    /// Scope for the episode.
    pub scope: String,
    /// Episode text fed to the writer.
    pub episode: String,
    /// Optional pre-parsed ops JSON (when set, skip extract; test apply path).
    #[serde(default)]
    pub ops_json: Option<String>,
    /// When true, run RuleWriter extract instead of ops_json.
    #[serde(default)]
    pub use_rule_writer: bool,
    /// Expected: at least one Stored result.
    #[serde(default)]
    pub expect_stored: bool,
    /// Expected: all ops are Noop (or extract yields noop).
    #[serde(default)]
    pub expect_noop_only: bool,
    /// Expected substring in some stored preview.
    #[serde(default)]
    pub expect_preview_contains: Option<String>,
    /// After apply, recall query that must hit.
    #[serde(default)]
    pub recall_query: Option<String>,
    /// Substring that must appear in recall hits.
    #[serde(default)]
    pub recall_must_contain: Option<String>,
    /// Substring that must not appear in recall hits (supersede proof).
    #[serde(default)]
    pub recall_must_not_contain: Option<String>,
    /// ops_json must parse as WriterOp[] (when set).
    #[serde(default = "default_true")]
    pub expect_ops_parse: bool,
    /// Reject if any Supersede has empty prior_id (default true).
    #[serde(default = "default_true")]
    pub forbid_empty_prior: bool,
    /// When true, extract via MockLlm + CliChatWriter with `ops_json` as mock text
    /// (scores production fail-closed parse path).
    #[serde(default)]
    pub use_cli_writer_mock: bool,
    /// Apply these ops first. Then rewrite any supersede with `prior_id` `$seed_id`
    /// to the first stored seed id + version.
    #[serde(default)]
    pub seed_ops_json: Option<String>,
    /// After apply, list this category (must match stored rows).
    #[serde(default)]
    pub expect_list_category: Option<String>,
}

fn default_true() -> bool {
    true
}

/// Hard-recall multipass case.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardRecallCase {
    pub name: String,
    pub scope: String,
    /// Seed memories (text).
    pub memories: Vec<String>,
    pub query: String,
    /// Substring that must appear in fused hits.
    pub expect_contains: String,
    /// Require multipass (rewriter produces >1 query).
    #[serde(default)]
    pub expect_multipass: bool,
    /// After recall, run pack → record_inject → outcome and require those kinds.
    #[serde(default)]
    pub expect_inject_loop: bool,
}

/// Suite of offline C0 cases.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillEvalSuite {
    pub name: String,
    #[serde(default)]
    pub embedding_dim: usize,
    #[serde(default)]
    pub write_cases: Vec<WriteCase>,
    #[serde(default)]
    pub hard_recall_cases: Vec<HardRecallCase>,
}

impl Default for SkillEvalSuite {
    fn default() -> Self {
        Self {
            name: "empty".into(),
            embedding_dim: 32,
            write_cases: vec![],
            hard_recall_cases: vec![],
        }
    }
}

/// Result of one case.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseResult {
    pub name: String,
    pub track: String,
    pub passed: bool,
    pub detail: String,
}

/// Suite summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillSuiteResult {
    pub name: String,
    pub passed: bool,
    pub cases: Vec<CaseResult>,
}

/// Built-in Phase C0 suite (no external files required). Rust is CI source of truth;
/// `evals/skill_c0_suite.json` is a loadable mirror (see fixture sync test).
pub fn skill_suite_c0() -> SkillEvalSuite {
    SkillEvalSuite {
        name: "skill_c0_offline_v1".into(),
        embedding_dim: 32,
        write_cases: vec![
            WriteCase {
                name: "parse_valid_put".into(),
                scope: "org/eval/skill".into(),
                episode: "unused when ops_json set".into(),
                ops_json: Some(
                    r#"[{"op":"put","scope":"org/eval/skill","text":"Alice prefers TypeScript for agent tooling.","category":"semantic","confidence":0.9}]"#.into(),
                ),
                use_rule_writer: false,
                expect_stored: true,
                expect_noop_only: false,
                expect_preview_contains: Some("TypeScript".into()),
                recall_query: Some("TypeScript agent tooling".into()),
                recall_must_contain: Some("TypeScript".into()),
                recall_must_not_contain: None,
                expect_ops_parse: true,
                forbid_empty_prior: true,
                use_cli_writer_mock: false,
                seed_ops_json: None,
                expect_list_category: None,
            },
            WriteCase {
                name: "parse_noop_preferred".into(),
                scope: "org/eval/skill".into(),
                episode: "hi".into(),
                ops_json: Some(r#"[{"op":"noop","reason":"no durable fact"}]"#.into()),
                use_rule_writer: false,
                expect_stored: false,
                expect_noop_only: true,
                expect_preview_contains: None,
                recall_query: None,
                recall_must_contain: None,
                recall_must_not_contain: None,
                expect_ops_parse: true,
                forbid_empty_prior: true,
                use_cli_writer_mock: false,
                seed_ops_json: None,
                expect_list_category: None,
            },
            WriteCase {
                name: "rule_writer_fact_line".into(),
                scope: "org/eval/rule".into(),
                episode: "Alice prefers Rust for systems work.".into(),
                ops_json: None,
                use_rule_writer: true,
                expect_stored: true,
                expect_noop_only: false,
                expect_preview_contains: Some("Rust".into()),
                recall_query: Some("prefers Rust".into()),
                recall_must_contain: Some("Rust".into()),
                recall_must_not_contain: None,
                expect_ops_parse: true,
                forbid_empty_prior: true,
                use_cli_writer_mock: false,
                seed_ops_json: None,
                expect_list_category: None,
            },
            WriteCase {
                name: "rule_writer_empty".into(),
                scope: "org/eval/rule".into(),
                episode: "   ".into(),
                ops_json: None,
                use_rule_writer: true,
                expect_stored: false,
                expect_noop_only: true,
                expect_preview_contains: None,
                recall_query: None,
                recall_must_contain: None,
                recall_must_not_contain: None,
                expect_ops_parse: true,
                forbid_empty_prior: true,
                use_cli_writer_mock: false,
                seed_ops_json: None,
                expect_list_category: None,
            },
            WriteCase {
                name: "parse_fenced_put".into(),
                scope: "org/eval/skill".into(),
                episode: "x".into(),
                ops_json: Some(
                    r#"Here:
```json
[{"op":"put","scope":"org/eval/skill","text":"Build failed with NOMI_SCOPE_DENIED_42.","category":"episodic","episodic":true}]
```
"#
                    .into(),
                ),
                use_rule_writer: false,
                expect_stored: true,
                expect_noop_only: false,
                expect_preview_contains: Some("NOMI_SCOPE_DENIED_42".into()),
                recall_query: Some("NOMI_SCOPE_DENIED_42".into()),
                recall_must_contain: Some("NOMI_SCOPE_DENIED_42".into()),
                recall_must_not_contain: None,
                expect_ops_parse: true,
                forbid_empty_prior: true,
                use_cli_writer_mock: false,
                seed_ops_json: None,
                expect_list_category: None,
            },
            WriteCase {
                name: "parse_garbage_expect_fail".into(),
                scope: "org/eval/skill".into(),
                episode: "x".into(),
                ops_json: Some("not json at all".into()),
                use_rule_writer: false,
                expect_stored: false,
                expect_noop_only: false,
                expect_preview_contains: None,
                recall_query: None,
                recall_must_contain: None,
                recall_must_not_contain: None,
                expect_ops_parse: false,
                forbid_empty_prior: true,
                use_cli_writer_mock: false,
                seed_ops_json: None,
                expect_list_category: None,
            },
            WriteCase {
                name: "fail_closed_cli_writer_prose".into(),
                scope: "org/eval/skill".into(),
                episode: "she likes cats".into(),
                ops_json: Some("I think you should store that she likes cats.".into()),
                use_rule_writer: false,
                expect_stored: false,
                expect_noop_only: true,
                expect_preview_contains: None,
                recall_query: None,
                recall_must_contain: None,
                recall_must_not_contain: None,
                expect_ops_parse: true,
                forbid_empty_prior: true,
                use_cli_writer_mock: true,
                seed_ops_json: None,
                expect_list_category: None,
            },
            WriteCase {
                name: "empty_prior_supersede_rejected".into(),
                scope: "org/eval/skill".into(),
                episode: "x".into(),
                ops_json: Some(
                    r#"[{"op":"supersede","prior_id":"","expected_version":1,"scope":"org/eval/skill","text":"bad"}]"#.into(),
                ),
                use_rule_writer: false,
                expect_stored: false,
                expect_noop_only: false,
                expect_preview_contains: None,
                recall_query: None,
                recall_must_contain: None,
                recall_must_not_contain: None,
                expect_ops_parse: true,
                forbid_empty_prior: true,
                use_cli_writer_mock: false,
                seed_ops_json: None,
                expect_list_category: None,
            },
            WriteCase {
                name: "parse_uncertainty_put".into(),
                scope: "org/eval/skill".into(),
                episode: "unused".into(),
                ops_json: Some(
                    r#"[{"op":"put","scope":"org/eval/skill","text":"Conflict: Alice prefers TypeScript vs Rust; no trusted prior.","category":"uncertainty","confidence":0.5}]"#.into(),
                ),
                use_rule_writer: false,
                expect_stored: true,
                expect_noop_only: false,
                expect_preview_contains: Some("TypeScript vs Rust".into()),
                recall_query: Some("Alice prefers TypeScript vs Rust".into()),
                recall_must_contain: Some("TypeScript vs Rust".into()),
                recall_must_not_contain: None,
                expect_ops_parse: true,
                forbid_empty_prior: true,
                use_cli_writer_mock: false,
                seed_ops_json: Some(
                    r#"[{"op":"put","scope":"org/eval/skill","text":"Alice prefers TypeScript for agent tooling.","category":"semantic","confidence":0.9}]"#.into(),
                ),
                expect_list_category: Some("uncertainty".into()),
            },
            WriteCase {
                name: "seeded_supersede_replaces_text".into(),
                scope: "org/eval/skill".into(),
                episode: "unused".into(),
                ops_json: Some(
                    r#"[{"op":"supersede","prior_id":"$seed_id","expected_version":1,"scope":"org/eval/skill","text":"Alice prefers Rust for agent tooling.","category":"semantic"}]"#.into(),
                ),
                use_rule_writer: false,
                expect_stored: true,
                expect_noop_only: false,
                expect_preview_contains: Some("prefers Rust".into()),
                recall_query: Some("prefers Rust agent tooling".into()),
                recall_must_contain: Some("prefers Rust".into()),
                recall_must_not_contain: Some("TypeScript".into()),
                expect_ops_parse: true,
                forbid_empty_prior: true,
                use_cli_writer_mock: false,
                seed_ops_json: Some(
                    r#"[{"op":"put","scope":"org/eval/skill","text":"Alice prefers TypeScript for agent tooling.","category":"semantic","confidence":0.9}]"#.into(),
                ),
                expect_list_category: None,
            },
        ],
        hard_recall_cases: vec![
            HardRecallCase {
                name: "multipass_error_code".into(),
                scope: "org/eval/hr".into(),
                memories: vec![
                    "Build failed with error NOMI_SCOPE_DENIED_42 on project foobar.".into(),
                    "Alice prefers TypeScript for agent tooling.".into(),
                ],
                query: "What is NOMI_SCOPE_DENIED_42 about?".into(),
                expect_contains: "NOMI_SCOPE_DENIED_42".into(),
                expect_multipass: true,
                expect_inject_loop: false,
            },
            HardRecallCase {
                name: "pack_inject_outcome_loop".into(),
                scope: "org/eval/fly".into(),
                memories: vec!["Alice prefers TypeScript for agent tooling.".into()],
                query: "TypeScript agent tooling".into(),
                expect_contains: "TypeScript".into(),
                expect_multipass: false,
                expect_inject_loop: true,
            },
        ],
    }
}

/// Load suite from JSON file.
pub fn load_suite_json(path: &Path) -> Result<SkillEvalSuite> {
    let raw = std::fs::read_to_string(path).map_err(|e| Error::io(e.to_string()))?;
    serde_json::from_str(&raw).map_err(|e| Error::invalid(format!("suite json: {e}")))
}

/// Load write cases from JSONL (one WriteCase per line).
pub fn load_write_jsonl(path: &Path) -> Result<Vec<WriteCase>> {
    let raw = std::fs::read_to_string(path).map_err(|e| Error::io(e.to_string()))?;
    let mut out = Vec::new();
    for (i, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let c: WriteCase = serde_json::from_str(line)
            .map_err(|e| Error::invalid(format!("write jsonl line {}: {e}", i + 1)))?;
        out.push(c);
    }
    Ok(out)
}

/// Run full skill suite offline.
pub async fn run_skill_suite(suite: &SkillEvalSuite) -> Result<SkillSuiteResult> {
    let dim = if suite.embedding_dim == 0 {
        32
    } else {
        suite.embedding_dim
    };
    let mut cases = Vec::new();

    for c in &suite.write_cases {
        cases.push(run_write_case(dim, c).await);
    }
    for c in &suite.hard_recall_cases {
        cases.push(run_hard_recall_case(dim, c).await);
    }

    let passed = cases.iter().all(|c| c.passed);
    Ok(SkillSuiteResult {
        name: suite.name.clone(),
        passed,
        cases,
    })
}

async fn run_write_case(dim: usize, case: &WriteCase) -> CaseResult {
    match run_write_case_inner(dim, case).await {
        Ok(r) => r,
        Err(e) => CaseResult {
            name: case.name.clone(),
            track: "write".into(),
            passed: false,
            detail: format!("error: {e}"),
        },
    }
}

async fn run_write_case_inner(dim: usize, case: &WriteCase) -> Result<CaseResult> {
    let track = "write".to_string();
    let emb = HashingEmbedder::new(dim).map_err(|e| Error::invalid(e.to_string()))?;
    let vp = Vegapunk::connect_memory(dim)
        .await?
        .with_profile(Profile::CodingAgent)
        .with_embedder(Arc::new(emb))
        .with_writer(Arc::new(RuleWriter));

    let seed_id_ver = if let Some(raw) = &case.seed_ops_json {
        let seed_ops = parse_writer_ops_from_model(raw)?;
        let seed_scope = seed_ops
            .iter()
            .find_map(crate::writer::op_scope)
            .unwrap_or(case.scope.as_str())
            .to_string();
        let seed_results = apply_ops(vp.client(), vp.policy(), &seed_scope, &seed_ops).await?;
        seed_results.iter().find_map(|r| match r.ok_result() {
            Some(ApplyResult::Stored(o)) => Some((o.id.clone(), o.version)),
            _ => None,
        })
    } else {
        None
    };
    if case.seed_ops_json.is_some() && seed_id_ver.is_none() {
        return Ok(CaseResult {
            name: case.name.clone(),
            track,
            passed: false,
            detail: "seed_ops_json produced no stored id".into(),
        });
    }

    let mut ops: Vec<WriterOp> = if case.use_cli_writer_mock {
        let mock_text = case.ops_json.clone().unwrap_or_else(|| "not json".into());
        let llm = Arc::new(MockLlm { text: mock_text });
        CliChatWriter::new(llm)
            .extract(&WriteEpisode {
                scope: case.scope.clone(),
                text: case.episode.clone(),
                source: Some(format!("eval:{}", case.name)),
            })
            .await?
    } else if case.use_rule_writer {
        RuleWriter
            .extract(&WriteEpisode {
                scope: case.scope.clone(),
                text: case.episode.clone(),
                source: Some(format!("eval:{}", case.name)),
            })
            .await?
    } else if let Some(raw) = &case.ops_json {
        match parse_writer_ops_from_model(raw) {
            Ok(ops) => {
                if case.expect_ops_parse && ops.is_empty() && !case.expect_noop_only {
                    return Ok(CaseResult {
                        name: case.name.clone(),
                        track,
                        passed: false,
                        detail: "ops_json parsed empty".into(),
                    });
                }
                ops
            }
            Err(e) => {
                return Ok(CaseResult {
                    name: case.name.clone(),
                    track,
                    passed: !case.expect_ops_parse,
                    detail: format!("ops parse: {e}"),
                });
            }
        }
    } else {
        return Ok(CaseResult {
            name: case.name.clone(),
            track,
            passed: false,
            detail: "case needs ops_json, use_rule_writer, or use_cli_writer_mock".into(),
        });
    };

    if let Some((id, ver)) = &seed_id_ver {
        for op in &mut ops {
            if let WriterOp::Supersede {
                prior_id,
                expected_version,
                ..
            } = op
            {
                if prior_id.as_str() == "$seed_id" {
                    *prior_id = id.clone();
                    *expected_version = *ver;
                }
            }
        }
    }

    if case.forbid_empty_prior {
        for op in &ops {
            if let WriterOp::Supersede { prior_id, .. } = op {
                if prior_id.as_str().trim().is_empty() {
                    // Detection of empty prior is a pass for pure-rejection cases
                    // (expect_stored=false); a fail if the case expected a store.
                    return Ok(CaseResult {
                        name: case.name.clone(),
                        track,
                        passed: !case.expect_stored,
                        detail: "empty prior_id in supersede (rejected)".into(),
                    });
                }
            }
        }
    }

    let active_scope = ops
        .iter()
        .find_map(crate::writer::op_scope)
        .unwrap_or("org/eval")
        .to_string();
    let results = apply_ops(vp.client(), vp.policy(), &active_scope, &ops).await?;
    let stored_n = results
        .iter()
        .filter(|r| matches!(r.ok_result(), Some(ApplyResult::Stored(_))))
        .count();
    let noop_only = !results.is_empty()
        && results
            .iter()
            .all(|r| matches!(r.ok_result(), Some(ApplyResult::Noop { .. })));

    if case.expect_stored && stored_n == 0 {
        return Ok(CaseResult {
            name: case.name.clone(),
            track,
            passed: false,
            detail: format!("expected stored, got {results:?}"),
        });
    }
    if case.expect_noop_only && !noop_only {
        return Ok(CaseResult {
            name: case.name.clone(),
            track,
            passed: false,
            detail: format!("expected noop only, got {results:?}"),
        });
    }
    if let Some(sub) = &case.expect_preview_contains {
        let ok = results.iter().any(|r| match r.ok_result() {
            Some(ApplyResult::Stored(o)) => o.preview.contains(sub.as_str()),
            _ => false,
        });
        if !ok {
            return Ok(CaseResult {
                name: case.name.clone(),
                track,
                passed: false,
                detail: format!("preview missing '{sub}'"),
            });
        }
    }

    if let Some(q) = &case.recall_query {
        let hits = vp.recall(&case.scope, q).await?;
        if let Some(must) = &case.recall_must_contain {
            if !hits.iter().any(|h| h.preview.contains(must.as_str())) {
                return Ok(CaseResult {
                    name: case.name.clone(),
                    track,
                    passed: false,
                    detail: format!(
                        "recall miss for '{must}'; hits={:?}",
                        hits.iter().map(|h| &h.preview).collect::<Vec<_>>()
                    ),
                });
            }
        }
        if let Some(forbid) = &case.recall_must_not_contain {
            if hits.iter().any(|h| h.preview.contains(forbid.as_str())) {
                return Ok(CaseResult {
                    name: case.name.clone(),
                    track,
                    passed: false,
                    detail: format!(
                        "recall still has '{forbid}'; hits={:?}",
                        hits.iter().map(|h| &h.preview).collect::<Vec<_>>()
                    ),
                });
            }
            let listed = vp.list(&case.scope, None, None).await?;
            if listed
                .items
                .iter()
                .any(|i| i.record.content.text.contains(forbid.as_str()))
            {
                return Ok(CaseResult {
                    name: case.name.clone(),
                    track,
                    passed: false,
                    detail: format!("unfiltered list still has '{forbid}'"),
                });
            }
        }
    }

    if let Some(cat_s) = &case.expect_list_category {
        let Some(cat) = nomiso::Category::parse(cat_s) else {
            return Ok(CaseResult {
                name: case.name.clone(),
                track,
                passed: false,
                detail: format!("bad expect_list_category '{cat_s}'"),
            });
        };
        let page = vp
            .list_with(
                &case.scope,
                EnumerateOptions {
                    categories: Some(vec![cat]),
                    ..Default::default()
                },
            )
            .await?;
        if page.items.iter().any(|i| i.record.category != cat) {
            return Ok(CaseResult {
                name: case.name.clone(),
                track,
                passed: false,
                detail: format!("list category leak: {:?}", page.items),
            });
        }
        if let Some(sub) = case
            .expect_preview_contains
            .as_ref()
            .or(case.recall_must_contain.as_ref())
        {
            if !page
                .items
                .iter()
                .any(|i| i.record.content.text.contains(sub.as_str()))
            {
                return Ok(CaseResult {
                    name: case.name.clone(),
                    track,
                    passed: false,
                    detail: format!("list/{cat_s} missing '{sub}'"),
                });
            }
        }
    }

    Ok(CaseResult {
        name: case.name.clone(),
        track,
        passed: true,
        detail: format!("ok stored={stored_n} ops={}", ops.len()),
    })
}

async fn run_hard_recall_case(dim: usize, case: &HardRecallCase) -> CaseResult {
    match run_hard_recall_case_inner(dim, case).await {
        Ok(r) => r,
        Err(e) => CaseResult {
            name: case.name.clone(),
            track: "hard_recall".into(),
            passed: false,
            detail: format!("error: {e}"),
        },
    }
}

async fn run_hard_recall_case_inner(dim: usize, case: &HardRecallCase) -> Result<CaseResult> {
    let track = "hard_recall".to_string();
    let emb = HashingEmbedder::new(dim).map_err(|e| Error::invalid(e.to_string()))?;
    let vp = Vegapunk::connect_memory(dim)
        .await?
        .with_profile(Profile::CodingAgent)
        .with_embedder(Arc::new(emb));

    for (i, text) in case.memories.iter().enumerate() {
        vp.remember(RememberInput {
            scope: case.scope.clone(),
            text: text.clone(),
            category: None,
            confidence: Some(0.9),
            source: Some(format!("eval:{}:{i}", case.name)),
            embedding: None,
            episodic: false,
            idempotency_key: None,
        })
        .await?;
    }

    let rewriter = RuleQueryRewriter;
    let opts = HardRecallOptions::from_policy(vp.policy());
    let hr = hard_recall(vp.client(), &case.scope, &case.query, opts, &rewriter).await?;

    if case.expect_multipass && hr.queries.len() < 2 {
        return Ok(CaseResult {
            name: case.name.clone(),
            track,
            passed: false,
            detail: format!("expected multipass, queries={:?}", hr.queries),
        });
    }
    if hr.abstained {
        return Ok(CaseResult {
            name: case.name.clone(),
            track,
            passed: false,
            detail: format!("abstained: {:?}", hr.abstain_reason),
        });
    }
    let ok = hr
        .hits
        .iter()
        .any(|h| h.preview.contains(&case.expect_contains));
    if !ok {
        return Ok(CaseResult {
            name: case.name.clone(),
            track,
            passed: false,
            detail: format!(
                "missing '{}' in hits {:?}",
                case.expect_contains,
                hr.hits.iter().map(|h| &h.preview).collect::<Vec<_>>()
            ),
        });
    }

    if case.expect_inject_loop {
        let (hrp, pack) = vp.hard_recall_pack(&case.scope, &case.query).await?;
        let Some(tid) = hrp.trace_id.clone() else {
            return Ok(CaseResult {
                name: case.name.clone(),
                track,
                passed: false,
                detail: "pack did not emit trace_id".into(),
            });
        };
        let ids: Vec<_> = pack
            .cards
            .iter()
            .map(|c| nomiso::MemoryId::new(c.id.clone()))
            .collect();
        if ids.is_empty() {
            return Ok(CaseResult {
                name: case.name.clone(),
                track,
                passed: false,
                detail: "pack empty; cannot record inject".into(),
            });
        }
        vp.record_inject(&case.scope, &tid, &ids, Some("c0 host inject"))
            .await?;
        vp.record_trace_outcome(&case.scope, &tid, TraceOutcome::Helped, None)
            .await?;
        let bundle = vp.get_trace(&case.scope, &tid).await?;
        let kinds: Vec<_> = bundle.events.iter().map(|e| e.kind.as_str()).collect();
        for need in ["search", "pack", "inject", "outcome"] {
            if !kinds.contains(&need) {
                return Ok(CaseResult {
                    name: case.name.clone(),
                    track,
                    passed: false,
                    detail: format!("flywheel missing {need}: {kinds:?}"),
                });
            }
        }
    }

    Ok(CaseResult {
        name: case.name.clone(),
        track,
        passed: true,
        detail: format!("ok queries={} hits={}", hr.queries.len(), hr.hits.len()),
    })
}

/// Parse-only validation of a batch of ops_json strings (no store).
pub fn validate_ops_json_batch(raws: &[&str]) -> HashMap<usize, Result<Vec<WriterOp>>> {
    let mut m = HashMap::new();
    for (i, raw) in raws.iter().enumerate() {
        m.insert(i, parse_writer_ops_from_model(raw));
    }
    m
}

/// Case names in the built-in C0 suite (for fixture sync checks).
pub fn skill_suite_c0_case_names() -> Vec<String> {
    let s = skill_suite_c0();
    let mut names: Vec<String> = s.write_cases.iter().map(|c| c.name.clone()).collect();
    names.extend(s.hard_recall_cases.iter().map(|c| c.name.clone()));
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn skill_c0_suite_passes() {
        let suite = skill_suite_c0();
        let res = run_skill_suite(&suite).await.expect("run");
        for c in &res.cases {
            assert!(c.passed, "{} ({}): {}", c.name, c.track, c.detail);
        }
        assert!(res.passed);
    }

    #[test]
    fn validate_ops_batch_ok() {
        let m = validate_ops_json_batch(&[
            r#"[{"op":"noop","reason":"x"}]"#,
            r#"not json"#,
            r#"{"op":"noop","reason":"single object"}"#,
        ]);
        assert!(m.get(&0).unwrap().is_ok());
        assert!(m.get(&1).unwrap().is_err());
        assert!(m.get(&2).unwrap().is_ok());
    }

    #[test]
    fn fixture_mirror_case_names() {
        // Loadable mirror under evals/ — soft check when present (CI checkout has it).
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../evals/skill_c0_suite.json");
        if !path.exists() {
            return;
        }
        let file = load_suite_json(&path).expect("load fixture");
        let mut builtin = skill_suite_c0_case_names();
        let mut file_names: Vec<String> = file.write_cases.iter().map(|c| c.name.clone()).collect();
        file_names.extend(file.hard_recall_cases.iter().map(|c| c.name.clone()));
        builtin.sort();
        file_names.sort();
        assert_eq!(
            file_names, builtin,
            "evals/skill_c0_suite.json must mirror skill_suite_c0() names"
        );
        assert_eq!(file.name, skill_suite_c0().name);
    }
}
