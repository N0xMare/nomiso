//! Memory **writer** plane: extract (BYOM) → typed ops → Nomiso commit.
//!
//! No LLM is bundled. Attach a [`MemoryWriter`] that calls your model and returns
//! [`WriterOp`]s. [`apply_ops`] always validates and commits via Nomiso only.
//!
//! **Phase 1 safety (lib-level):**
//! - active-scope pin on every surface (CLI/HTTP/MCP inherit)
//! - pre-flight validate-all before any commit
//! - per-op outcomes: [`apply_ops`] keeps a committed prefix and marks trailing
//!   ops `not_attempted`; [`apply_ops_atomic`] is deprecated and rejects before effects
//! - hard-forget default-deny / require-version from [`MemoryPolicy`]

use async_trait::async_trait;
use nomiso_core::{ForgetRequest, MemoryId, SupersedeRequest};
use nomiso_service::NomisoClient;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::policy::MemoryPolicy;
use crate::remember::{prepare_put, RememberInput};
use crate::types::RememberOutcome;

/// Raw episode / transcript unit for a writer model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteEpisode {
    /// Scope for produced ops.
    pub scope: String,
    /// Free text (conversation snippet, tool summary, …).
    pub text: String,
    /// Optional provenance source.
    #[serde(default)]
    pub source: Option<String>,
}

/// Typed mutation proposed by a writer (never free-form SQL).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum WriterOp {
    /// No action.
    Noop {
        #[serde(default)]
        reason: Option<String>,
    },
    /// Create a memory.
    Put {
        #[serde(flatten)]
        input: RememberInput,
    },
    /// Supersede a prior fact.
    Supersede {
        prior_id: MemoryId,
        expected_version: u64,
        #[serde(flatten)]
        new: RememberInput,
    },
    /// Soft or hard forget.
    Forget {
        id: MemoryId,
        /// Scope owning the record (required by plane).
        scope: String,
        expected_version: Option<u64>,
        #[serde(default)]
        hard: bool,
    },
}

impl<'de> Deserialize<'de> for WriterOp {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct OpVisitor;
        impl<'de> serde::de::Visitor<'de> for OpVisitor {
            type Value = WriterOp;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a writer operation object")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> std::result::Result<WriterOp, M::Error> {
                let mut fields = serde_json::Map::new();
                while let Some((key, value)) = map.next_entry::<String, serde_json::Value>()? {
                    if fields.insert(key, value).is_some() {
                        return Err(serde::de::Error::custom("duplicate writer operation field"));
                    }
                }
                decode_writer_op(fields).map_err(serde::de::Error::custom)
            }
        }
        deserializer.deserialize_map(OpVisitor)
    }
}

fn decode_writer_op(
    mut fields: serde_json::Map<String, serde_json::Value>,
) -> std::result::Result<WriterOp, String> {
    fn take<T: serde::de::DeserializeOwned>(
        fields: &mut serde_json::Map<String, serde_json::Value>,
        key: &str,
    ) -> std::result::Result<T, String> {
        serde_json::from_value(fields.remove(key).unwrap_or(serde_json::Value::Null))
            .map_err(|_| format!("invalid or missing writer field: {key}"))
    }
    let op: String = take(&mut fields, "op")?;
    let allowed: &[&str] = match op.as_str() {
        "noop" => &["reason"],
        "put" => &[
            "scope",
            "text",
            "category",
            "confidence",
            "source",
            "embedding",
            "episodic",
            "idempotency_key",
        ],
        "supersede" => &[
            "prior_id",
            "expected_version",
            "scope",
            "text",
            "category",
            "confidence",
            "source",
            "embedding",
            "episodic",
            "idempotency_key",
        ],
        "forget" => &["id", "scope", "expected_version", "hard"],
        _ => return Err("unknown writer operation".into()),
    };
    if fields.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err("unknown writer operation field".into());
    }
    match op.as_str() {
        "noop" => Ok(WriterOp::Noop {
            reason: take(&mut fields, "reason")?,
        }),
        "put" => Ok(WriterOp::Put {
            input: serde_json::from_value(serde_json::Value::Object(fields))
                .map_err(|_| "invalid put input".to_string())?,
        }),
        "supersede" => {
            let prior_id = take(&mut fields, "prior_id")?;
            let expected_version = take(&mut fields, "expected_version")?;
            let new = serde_json::from_value(serde_json::Value::Object(fields))
                .map_err(|_| "invalid supersede input".to_string())?;
            Ok(WriterOp::Supersede {
                prior_id,
                expected_version,
                new,
            })
        }
        "forget" => {
            let id = take(&mut fields, "id")?;
            let scope = take(&mut fields, "scope")?;
            let expected_version = take(&mut fields, "expected_version")?;
            let hard: Option<bool> = take(&mut fields, "hard")?;
            Ok(WriterOp::Forget {
                id,
                scope,
                expected_version,
                hard: hard.unwrap_or(false),
            })
        }
        _ => Err("unknown writer operation".into()),
    }
}

/// Result of applying one writer op successfully.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ApplyResult {
    Noop {
        reason: String,
    },
    Stored(RememberOutcome),
    Forgotten {
        id: MemoryId,
        /// True when hard was requested but policy downgraded to soft.
        #[serde(default)]
        soft_downgraded: bool,
    },
}

/// Per-op outcome: success or failure without discarding the batch prefix
/// ([`apply_ops`]). Ops never attempted after a failure are reported with code
/// `not_attempted`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ApplyOpOutcome {
    /// Op applied successfully.
    Ok { index: usize, result: ApplyResult },
    /// Op failed after earlier ops may have committed, or was never attempted.
    Err {
        index: usize,
        error: String,
        code: String,
        /// Legacy field retained for response compatibility; always empty
        /// because batch rollback is unsupported.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        rolled_back: Vec<nomiso_core::MemoryId>,
        /// Legacy field retained for response compatibility; always empty.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        rollback_failed: Vec<nomiso_core::MemoryId>,
    },
}

impl ApplyOpOutcome {
    /// Whether this outcome is Ok.
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok { .. })
    }

    /// Extract success result if present.
    pub fn ok_result(&self) -> Option<&ApplyResult> {
        match self {
            Self::Ok { result, .. } => Some(result),
            Self::Err { .. } => None,
        }
    }
}

/// BYOM memory writer: episode → structured ops.
#[async_trait]
pub trait MemoryWriter: Send + Sync {
    async fn extract(&self, episode: &WriteEpisode) -> Result<Vec<WriterOp>>;
}

/// Rule-based writer (no LLM): one op per non-empty line.
#[derive(Debug, Default, Clone)]
pub struct RuleWriter;

#[async_trait]
impl MemoryWriter for RuleWriter {
    async fn extract(&self, episode: &WriteEpisode) -> Result<Vec<WriterOp>> {
        let text = episode.text.trim();
        if text.is_empty() {
            return Ok(vec![WriterOp::Noop {
                reason: Some("empty episode".into()),
            }]);
        }
        let mut ops = Vec::new();
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let episodic = !looks_like_writer_fact(line);
            ops.push(WriterOp::Put {
                input: RememberInput {
                    scope: episode.scope.clone(),
                    text: line.to_string(),
                    category: None,
                    confidence: Some(0.85),
                    source: episode
                        .source
                        .clone()
                        .or_else(|| Some("rule-writer".into())),
                    embedding: None,
                    episodic,
                    idempotency_key: None,
                },
            });
        }
        if ops.is_empty() {
            ops.push(WriterOp::Noop {
                reason: Some("no lines".into()),
            });
        }
        Ok(ops)
    }
}

fn looks_like_writer_fact(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    [
        "prefer", "prefers", "must ", "should ", "decided", "always ", "never ",
    ]
    .iter()
    .any(|c| t.contains(c))
}

/// Scope of a mutating op (None for noop).
pub fn op_scope(op: &WriterOp) -> Option<&str> {
    match op {
        WriterOp::Put { input } => Some(input.scope.as_str()),
        WriterOp::Supersede { new, .. } => Some(new.scope.as_str()),
        WriterOp::Forget { scope, .. } => Some(scope.as_str()),
        WriterOp::Noop { .. } => None,
    }
}

/// Pre-flight: active-scope pin + structural/policy checks (no store I/O).
pub fn preflight_ops(
    active_scope: &str,
    policy: impl Into<MemoryPolicy>,
    ops: &[WriterOp],
) -> Result<()> {
    let policy = policy.into();
    let active = active_scope.trim();
    if active.is_empty() {
        return Err(Error::invalid(
            "active_scope is required for apply_ops (lib-level scope pin)",
        ));
    }
    for (i, op) in ops.iter().enumerate() {
        if let Some(s) = op_scope(op) {
            let s = s.trim();
            if s.is_empty() {
                return Err(Error::invalid(format!(
                    "op[{i}]: empty scope (active scope is {active})"
                )));
            }
            if s != active {
                return Err(Error::Policy(format!(
                    "op[{i}]: scope '{s}' != active scope '{active}' \
                     (model/host must not target foreign scopes)"
                )));
            }
        }
        match op {
            WriterOp::Put { input } | WriterOp::Supersede { new: input, .. } => {
                if input.text.trim().is_empty() {
                    return Err(Error::invalid(format!("op[{i}]: text must be non-empty")));
                }
            }
            WriterOp::Forget {
                hard,
                expected_version,
                id,
                ..
            } => {
                if id.as_str().trim().is_empty() {
                    return Err(Error::invalid(format!("op[{i}]: forget requires id")));
                }
                if policy.require_version_on_forget && expected_version.is_none() {
                    return Err(Error::Policy(format!(
                        "op[{i}]: forget requires expected_version \
                         (MemoryPolicy.require_version_on_forget=true)"
                    )));
                }
                if *hard && !policy.allow_hard_forget {
                    // Allowed: will soft-downgrade at apply time (not a preflight hard fail).
                }
            }
            WriterOp::Noop { .. } => {}
        }
    }
    Ok(())
}

/// Apply writer ops through Nomiso with lib-level safety.
///
/// 1. **Pre-flight** validates all ops (scope pin, policy, structure). On failure:
///    nothing is committed; returns `Err`.
/// 2. **Apply** each op sequentially. Mid-batch store failures yield
///    [`ApplyOpOutcome::Err`] for that index **and continue is not used** —
///    remaining ops are not applied; each gets an explicit `not_attempted` Err
///    outcome so the prefix/tail split is observable in `Ok(vec)`.
///
/// Callers must inspect per-op outcomes; do not re-run the whole batch blindly.
pub async fn apply_ops(
    client: &NomisoClient,
    policy: impl Into<MemoryPolicy>,
    active_scope: &str,
    ops: &[WriterOp],
) -> Result<Vec<ApplyOpOutcome>> {
    let policy = policy.into();
    preflight_ops(active_scope, policy, ops)?;
    if ops.len() > 128 {
        return Err(Error::Invalid("writer batch exceeds 128 operations".into()));
    }
    let limits = client.store().limits();
    for op in ops {
        match op {
            WriterOp::Put { input } => {
                nomiso_core::validate::validate_put(&prepare_put(input, policy, false)?, &limits)?;
            }
            WriterOp::Supersede {
                prior_id,
                expected_version,
                new,
            } => {
                nomiso_core::validate::validate_supersede(
                    &SupersedeRequest {
                        prior_id: prior_id.clone(),
                        expected_version: *expected_version,
                        new: prepare_put(new, policy, true)?,
                        close_at: None,
                    },
                    &limits,
                )?;
            }
            WriterOp::Forget {
                id,
                scope,
                expected_version,
                hard,
            } => {
                nomiso_core::validate::validate_forget(&ForgetRequest {
                    id: id.clone(),
                    scope: scope.clone(),
                    expected_version: *expected_version,
                    hard: *hard,
                    at: None,
                })?;
            }
            WriterOp::Noop { .. } => {}
        }
    }
    let mut out = Vec::with_capacity(ops.len());
    for (index, op) in ops.iter().enumerate() {
        match apply_one(client, policy, op).await {
            Ok(result) => out.push(ApplyOpOutcome::Ok { index, result }),
            Err(e) => {
                out.push(ApplyOpOutcome::Err {
                    index,
                    code: error_code(&e),
                    error: e.public_message(),
                    rolled_back: vec![],
                    rollback_failed: vec![],
                });
                // Stop applying further ops but report every unattempted tail.
                out.extend(not_attempted_outcomes(index + 1, ops.len()));
                break;
            }
        }
    }
    Ok(out)
}

/// Deprecated compatibility symbol: atomic writer batches are unsupported.
///
/// Runs the shared pre-flight checks, then rejects with [`Error::Policy`]
/// **before any effect**. Use prefix-preserving [`apply_ops`] instead — callers
/// must inspect per-op outcomes rather than assume all-or-nothing semantics.
#[deprecated(
    note = "atomic writer batches are unsupported; use apply_ops for prefix-preserving writes"
)]
pub async fn apply_ops_atomic(
    client: &NomisoClient,
    policy: impl Into<MemoryPolicy>,
    active_scope: &str,
    ops: &[WriterOp],
) -> Result<Vec<ApplyOpOutcome>> {
    let _ = client;
    preflight_ops(active_scope, policy, ops)?;
    Err(Error::Policy(
        "atomic writer batches are unsupported; use apply_ops for explicit prefix-preserving writes"
            .into(),
    ))
}

/// Emit `not_attempted` outcomes for ops never tried after a prefix failure.
fn not_attempted_outcomes(start: usize, end: usize) -> Vec<ApplyOpOutcome> {
    (start..end)
        .map(|index| ApplyOpOutcome::Err {
            index,
            code: "not_attempted".into(),
            error: "not attempted because a preceding operation failed".into(),
            rolled_back: vec![],
            rollback_failed: vec![],
        })
        .collect()
}

fn error_code(e: &Error) -> String {
    e.code().to_string()
}

async fn apply_one(
    client: &NomisoClient,
    policy: MemoryPolicy,
    op: &WriterOp,
) -> Result<ApplyResult> {
    match op {
        WriterOp::Noop { reason } => Ok(ApplyResult::Noop {
            reason: reason.clone().unwrap_or_else(|| "noop".into()),
        }),
        WriterOp::Put { input } => {
            let outcome = crate::remember::remember(client, policy, input.clone()).await?;
            Ok(ApplyResult::Stored(outcome))
        }
        WriterOp::Supersede {
            prior_id,
            expected_version,
            new,
        } => {
            let outcome =
                supersede_remember(client, policy, prior_id, *expected_version, new.clone())
                    .await?;
            Ok(ApplyResult::Stored(outcome))
        }
        WriterOp::Forget {
            id,
            scope,
            expected_version,
            hard,
        } => {
            let mut hard = *hard;
            let mut soft_downgraded = false;
            if hard && !policy.allow_hard_forget {
                hard = false;
                soft_downgraded = true;
            }
            client
                .forget(ForgetRequest {
                    id: id.clone(),
                    scope: scope.clone(),
                    expected_version: *expected_version,
                    hard,
                    at: None,
                })
                .await?;
            Ok(ApplyResult::Forgotten {
                id: id.clone(),
                soft_downgraded,
            })
        }
    }
}

/// Supersede with a [`RememberInput`] body.
pub async fn supersede_remember(
    client: &NomisoClient,
    policy: impl Into<MemoryPolicy>,
    prior_id: &MemoryId,
    expected_version: u64,
    new: RememberInput,
) -> Result<RememberOutcome> {
    let text = new.text.trim();
    if text.is_empty() {
        return Err(Error::invalid("supersede text must be non-empty"));
    }
    let req = prepare_put(&new, policy, true)?;
    let category = req.category;
    let scope = req.scope.clone();
    let wr = client
        .supersede(SupersedeRequest {
            prior_id: prior_id.clone(),
            expected_version,
            new: req,
            close_at: None,
        })
        .await?;
    Ok(RememberOutcome {
        replayed: wr.replayed,
        id: wr.id,
        version: wr.version,
        category,
        scope,
        skipped: false,
        preview: text.to_string(),
    })
}

/// Extract with a writer then apply all ops under the episode scope pin.
pub async fn write_episode(
    client: &NomisoClient,
    policy: impl Into<MemoryPolicy>,
    writer: &dyn MemoryWriter,
    episode: &WriteEpisode,
) -> Result<Vec<ApplyOpOutcome>> {
    let ops = writer.extract(episode).await?;
    apply_ops(client, policy, &episode.scope, &ops).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::MemoryPolicy;
    use nomiso_store::StoreConfig;

    async fn mem_client() -> NomisoClient {
        NomisoClient::connect(StoreConfig::memory_test(8))
            .await
            .expect("connect")
    }

    #[test]
    fn writer_ops_reject_unknown_and_duplicate_fields() {
        for raw in [
            r#"{"op":"put","scope":"org/t","text":"fact","idempotencyKey":"typo"}"#,
            r#"{"op":"put","scope":"org/t","text":"first","text":"second"}"#,
            r#"{"op":"forget","id":"x","scope":"org/t","idempotency_key":"unsupported"}"#,
        ] {
            assert!(serde_json::from_str::<WriterOp>(raw).is_err());
        }
        let op: WriterOp = serde_json::from_str(
            r#"{"op":"put","scope":"org/t","text":"fact","idempotency_key":"valid"}"#,
        )
        .unwrap();
        assert!(
            matches!(op, WriterOp::Put { input } if input.idempotency_key.as_deref() == Some("valid"))
        );
    }

    #[tokio::test]
    async fn preflight_rejects_foreign_scope() {
        let policy = MemoryPolicy::default();
        let ops = vec![WriterOp::Put {
            input: RememberInput {
                scope: "org/other".into(),
                text: "secret".into(),
                category: None,
                confidence: Some(0.9),
                source: None,
                embedding: None,
                episodic: false,
                idempotency_key: None,
            },
        }];
        let err = preflight_ops("org/active", policy, &ops).unwrap_err();
        assert!(
            matches!(err, Error::Policy(_)),
            "expected Policy, got {err:?}"
        );
    }

    #[tokio::test]
    async fn preflight_requires_version_on_forget() {
        let policy = MemoryPolicy::default();
        assert!(policy.require_version_on_forget);
        let ops = vec![WriterOp::Forget {
            id: MemoryId::new("memory:x"),
            scope: "org/active".into(),
            expected_version: None,
            hard: false,
        }];
        let err = preflight_ops("org/active", policy, &ops).unwrap_err();
        assert!(matches!(err, Error::Policy(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn hard_forget_default_soft_downgrade() {
        let client = mem_client().await;
        let policy = MemoryPolicy::default();
        assert!(!policy.allow_hard_forget);
        // Seed a memory via put path
        let put = WriterOp::Put {
            input: RememberInput {
                scope: "org/hf".into(),
                text: "hard-forget-target-token".into(),
                category: None,
                confidence: Some(0.9),
                source: Some("test".into()),
                embedding: None,
                episodic: false,
                idempotency_key: None,
            },
        };
        let seeded = apply_ops(&client, policy, "org/hf", &[put]).await.unwrap();
        let id = match seeded[0].ok_result() {
            Some(ApplyResult::Stored(o)) => o.id.clone(),
            other => panic!("expected stored, got {other:?}"),
        };
        let forget = WriterOp::Forget {
            id: id.clone(),
            scope: "org/hf".into(),
            expected_version: Some(1),
            hard: true,
        };
        let out = apply_ops(&client, policy, "org/hf", &[forget])
            .await
            .unwrap();
        match out[0].ok_result() {
            Some(ApplyResult::Forgotten {
                soft_downgraded: true,
                ..
            }) => {}
            other => panic!("expected soft-downgraded forget, got {other:?}"),
        }
        // Soft-forgotten: still readable by id but not in search at now.
        // (soft sets valid_until)
    }

    #[tokio::test]
    async fn mid_batch_failure_preserves_prefix_results() {
        let client = mem_client().await;
        let policy = MemoryPolicy::default();
        let ops = vec![
            WriterOp::Put {
                input: RememberInput {
                    scope: "org/batch".into(),
                    text: "first committed fact".into(),
                    category: None,
                    confidence: Some(0.9),
                    source: None,
                    embedding: None,
                    episodic: false,
                    idempotency_key: None,
                },
            },
            // Forget missing id → store NotFound after first commit
            WriterOp::Forget {
                id: MemoryId::new("memory:does-not-exist-zzzz"),
                scope: "org/batch".into(),
                expected_version: Some(1),
                hard: false,
            },
            WriterOp::Put {
                input: RememberInput {
                    scope: "org/batch".into(),
                    text: "should not be applied".into(),
                    category: None,
                    confidence: Some(0.9),
                    source: None,
                    embedding: None,
                    episodic: false,
                    idempotency_key: None,
                },
            },
        ];
        let out = apply_ops(&client, policy, "org/batch", &ops)
            .await
            .expect("preflight ok; runtime returns per-op outcomes");
        assert_eq!(out.len(), 3, "every op needs an outcome; got {out:?}");
        assert!(out[0].is_ok(), "prefix committed: {out:?}");
        match &out[1] {
            ApplyOpOutcome::Err { index: 1, code, .. } => {
                assert_eq!(code, "not_found");
            }
            other => panic!("expected Err at index 1, got {other:?}"),
        }
        match &out[2] {
            ApplyOpOutcome::Err {
                index: 2,
                code,
                rolled_back,
                rollback_failed,
                ..
            } => {
                assert_eq!(code, "not_attempted");
                assert!(rolled_back.is_empty() && rollback_failed.is_empty());
            }
            other => panic!("expected not_attempted Err at index 2, got {other:?}"),
        }
        let hits = client
            .search(nomiso_core::SearchQuery {
                query: "committed".into(),
                scope: "org/batch".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(
            hits.iter()
                .any(|h| h.preview.contains("first committed fact")),
            "prefix must remain valid-now: {hits:?}"
        );
        assert!(
            !hits
                .iter()
                .any(|h| h.preview.contains("should not be applied")),
            "unattempted tail must not land: {hits:?}"
        );
        let n = client
            .count(nomiso_core::CountRequest {
                scope: "org/batch".into(),
                scope_match: Default::default(),
                categories: None,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
                text: None,
            })
            .await
            .unwrap();
        assert_eq!(n, 1, "only the committed prefix may remain in scope");
    }

    #[tokio::test]
    async fn prefix_supersede_failure_keeps_successor() {
        let client = mem_client().await;
        let policy = MemoryPolicy::default();
        let seeded = apply_ops(
            &client,
            policy,
            "org/prefix-sup",
            &[WriterOp::Put {
                input: RememberInput {
                    scope: "org/prefix-sup".into(),
                    text: "Alice prefers JavaScript for tooling.".into(),
                    category: None,
                    confidence: Some(0.9),
                    source: None,
                    embedding: None,
                    episodic: false,
                    idempotency_key: None,
                },
            }],
        )
        .await
        .unwrap();
        let prior = match seeded[0].ok_result() {
            Some(ApplyResult::Stored(o)) => o.clone(),
            other => panic!("expected stored, got {other:?}"),
        };
        let out = apply_ops(
            &client,
            policy,
            "org/prefix-sup",
            &[
                WriterOp::Supersede {
                    prior_id: prior.id.clone(),
                    expected_version: prior.version,
                    new: RememberInput {
                        scope: "org/prefix-sup".into(),
                        text: "Alice prefers TypeScript for tooling.".into(),
                        category: None,
                        confidence: Some(0.9),
                        source: None,
                        embedding: None,
                        episodic: false,
                        idempotency_key: None,
                    },
                },
                WriterOp::Forget {
                    id: MemoryId::new("memory:does-not-exist"),
                    scope: "org/prefix-sup".into(),
                    expected_version: Some(1),
                    hard: false,
                },
                WriterOp::Put {
                    input: RememberInput {
                        scope: "org/prefix-sup".into(),
                        text: "third op tail fact must not land".into(),
                        category: None,
                        confidence: Some(0.9),
                        source: None,
                        embedding: None,
                        episodic: false,
                        idempotency_key: None,
                    },
                },
            ],
        )
        .await
        .unwrap();
        assert_eq!(out.len(), 3, "{out:?}");
        let successor = match out[0].ok_result() {
            Some(ApplyResult::Stored(o)) => o.clone(),
            other => panic!("expected supersede stored, got {other:?}"),
        };
        match &out[1] {
            ApplyOpOutcome::Err { code, .. } => assert_eq!(code, "not_found"),
            other => panic!("expected not_found, got {other:?}"),
        }
        match &out[2] {
            ApplyOpOutcome::Err { code, .. } => assert_eq!(code, "not_attempted"),
            other => panic!("expected not_attempted tail, got {other:?}"),
        }
        let now = client
            .search(nomiso_core::SearchQuery {
                query: "prefers".into(),
                scope: "org/prefix-sup".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(
            now.iter().any(|h| h.id == successor.id),
            "committed supersede successor stays current: {now:?}"
        );
        assert!(
            now.iter().all(|h| h.id != prior.id),
            "superseded predecessor must be closed: {now:?}"
        );
        assert!(
            now.iter().all(|h| !h.preview.contains("tail fact")),
            "not_attempted tail must not land: {now:?}"
        );
        let n = client
            .count(nomiso_core::CountRequest {
                scope: "org/prefix-sup".into(),
                scope_match: Default::default(),
                categories: None,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
                text: None,
            })
            .await
            .unwrap();
        assert_eq!(
            n, 1,
            "scope must hold only the supersede successor, not the tail put"
        );
    }

    #[tokio::test]
    #[allow(deprecated)]
    async fn deprecated_atomic_rejected_before_effects() {
        let client = mem_client().await;
        let policy = MemoryPolicy::default();
        let ops = vec![
            WriterOp::Put {
                input: RememberInput {
                    scope: "org/atom".into(),
                    text: "Alice prefers TypeScript for tooling.".into(),
                    category: None,
                    confidence: Some(0.9),
                    source: None,
                    embedding: None,
                    episodic: false,
                    idempotency_key: None,
                },
            },
            WriterOp::Forget {
                id: nomiso_core::MemoryId::new("memory:does-not-exist"),
                scope: "org/atom".into(),
                expected_version: Some(1),
                hard: false,
            },
        ];
        let err = apply_ops_atomic(&client, policy, "org/atom", &ops)
            .await
            .expect_err("deprecated atomic path must reject before effects");
        assert!(
            matches!(err, Error::Policy(_)),
            "expected Policy, got {err:?}"
        );
        let hits = client
            .search(nomiso_core::SearchQuery {
                query: "TypeScript".into(),
                scope: "org/atom".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(
            hits.is_empty(),
            "rejected atomic batch must not commit the put: {hits:?}"
        );
    }

    #[tokio::test]
    #[allow(deprecated)]
    async fn deprecated_atomic_rejects_before_supersede_effects() {
        let client = mem_client().await;
        let policy = MemoryPolicy::default();
        let first = apply_ops(
            &client,
            policy,
            "org/sup",
            &[WriterOp::Put {
                input: RememberInput {
                    scope: "org/sup".into(),
                    text: "Alice prefers JavaScript for tooling.".into(),
                    category: None,
                    confidence: Some(0.9),
                    source: None,
                    embedding: None,
                    episodic: false,
                    idempotency_key: None,
                },
            }],
        )
        .await
        .unwrap();
        let prior = match first[0].ok_result() {
            Some(ApplyResult::Stored(o)) => o.clone(),
            other => panic!("expected stored, got {other:?}"),
        };
        let err = apply_ops_atomic(
            &client,
            policy,
            "org/sup",
            &[
                WriterOp::Supersede {
                    prior_id: prior.id.clone(),
                    expected_version: prior.version,
                    new: RememberInput {
                        scope: "org/sup".into(),
                        text: "Alice prefers TypeScript for tooling.".into(),
                        category: None,
                        confidence: Some(0.9),
                        source: None,
                        embedding: None,
                        episodic: false,
                        idempotency_key: None,
                    },
                },
                WriterOp::Forget {
                    id: MemoryId::new("memory:does-not-exist"),
                    scope: "org/sup".into(),
                    expected_version: Some(1),
                    hard: false,
                },
            ],
        )
        .await
        .expect_err("deprecated atomic path must reject before effects");
        assert!(
            matches!(err, Error::Policy(_)),
            "expected Policy, got {err:?}"
        );
        let now = client
            .search(nomiso_core::SearchQuery {
                query: "prefers".into(),
                scope: "org/sup".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(
            now.iter()
                .any(|h| h.id == prior.id && h.version == prior.version),
            "prior must remain open and unchanged: {now:?}"
        );
        assert!(
            now.iter().all(|h| !h.preview.contains("TypeScript")),
            "rejected batch must not create a successor: {now:?}"
        );
    }

    #[tokio::test]
    async fn malformed_tail_rejected_before_prefix_or_provider() {
        for mode in 0..3 {
            let client = mem_client().await;
            let policy = MemoryPolicy::default();
            let first = WriterOp::Put {
                input: RememberInput::fact("org/preflight", "first fact"),
            };
            let mut input = RememberInput::fact("org/preflight", "replacement");
            let tail = match mode {
                0 => {
                    input.text = "x".repeat(9000);
                    WriterOp::Put { input }
                }
                1 => {
                    input.embedding = Some(vec![1.0; 2]);
                    WriterOp::Put { input }
                }
                _ => {
                    input.idempotency_key = Some("unsupported".into());
                    WriterOp::Supersede {
                        prior_id: MemoryId::new("missing"),
                        expected_version: 1,
                        new: input,
                    }
                }
            };
            assert!(apply_ops(&client, policy, "org/preflight", &[first, tail])
                .await
                .is_err());
            assert_eq!(
                client
                    .count(nomiso_core::CountRequest {
                        scope: "org/preflight".into(),
                        scope_match: Default::default(),
                        categories: None,
                        as_of: None,
                        known_as_of: None,
                        sys_as_of: None,
                        text: None,
                    })
                    .await
                    .unwrap(),
                0
            );
        }
    }

    #[tokio::test]
    async fn foreign_scope_rejected_at_lib_without_cli() {
        let client = mem_client().await;
        let policy = MemoryPolicy::default();
        let ops = vec![WriterOp::Put {
            input: RememberInput {
                scope: "org/evil".into(),
                text: "should not land".into(),
                category: None,
                confidence: Some(0.9),
                source: None,
                embedding: None,
                episodic: false,
                idempotency_key: None,
            },
        }];
        let err = apply_ops(&client, policy, "org/good", &ops)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Policy(_)), "got {err:?}");
    }
}
