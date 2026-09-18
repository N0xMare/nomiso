//! Tact-shaped global memory on the Nomiso plane.
//!
//! Product contract from clabby/tact `docs/memory.md` as a production adapter:
//! explicit tools, root-only mutation, bounds, BM25-first scan, no auto-inject.
//!
//! **Nomiso-native improvements (documented, intentional):**
//! - Replace uses supersede (history retained under plane; new plane id).
//! - Product `logical_id` is stable normalized identity across supersession.
//! - Optional soft-forget prune for audit-friendly expiry (default hard for Tact parity).

#![forbid(unsafe_code)]

mod limits;
mod secrets;
mod types;

pub use limits::TactLimits;
pub use types::*;

use std::sync::atomic::{AtomicBool, Ordering};

use jiff::Timestamp;
use nomiso::{
    AnnotateRequest, Category, Content, ForgetRequest, MemoryId, NomisoClient, Provenance,
    PutRequest, ReadRequest, ScopeMatch, SearchQuery, StoreConfig, SupersedeRequest,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use tracing::instrument;

/// Errors for the Tact adapter.
#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Nomiso(#[from] nomiso::Error),

    #[error("memory is disabled (set enabled=true in TactMemoryConfig)")]
    Disabled,

    #[error("memory mutation is only available to root agents")]
    RootOnly,

    #[error("scan memory before storing a conclusion")]
    ScanRequired,

    #[error("memory content rejected: {0}")]
    Rejected(String),

    #[error("capacity exceeded: {0}")]
    Capacity(String),

    #[error("duplicate identity")]
    Duplicate,

    #[error("invalid: {0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Adapter configuration (Tact product contract).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TactMemoryConfig {
    /// Product gate (true = memory tools active). Set false for kill-switch / harness disable.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Fixed global scope (Tact: one corpus, no workspace filter).
    pub scope: String,
    /// Capacity limits.
    #[serde(default)]
    pub limits: TactLimits,
    /// Require successful scan before put (Tact tool latch).
    #[serde(default = "default_true")]
    pub require_scan_before_put: bool,
    /// When true, probation prune uses soft-forget (audit); false = hard delete (Tact v1).
    #[serde(default)]
    pub soft_prune: bool,
}

fn default_enabled() -> bool {
    true
}
fn default_true() -> bool {
    true
}

impl Default for TactMemoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            scope: "tact/global".into(),
            limits: TactLimits::default(),
            require_scan_before_put: true,
            soft_prune: false,
        }
    }
}

mod attr {
    pub const IDENTITY: &str = "tact_normalized_identity";
    pub const SCAN_COUNT: &str = "tact_scan_count";
    pub const USE_COUNT: &str = "tact_use_count";
    pub const PROBATION_UNTIL: &str = "tact_probation_until";
    pub const LOGICAL: &str = "tact_system";
}

/// Tact memory facade over Nomiso.
pub struct TactMemory {
    client: NomisoClient,
    config: TactMemoryConfig,
    /// Successful scan arms one put (root).
    scanned: AtomicBool,
}

impl TactMemory {
    /// Connect Nomiso and wrap as Tact memory.
    pub async fn connect(store: StoreConfig, config: TactMemoryConfig) -> Result<Self> {
        config.limits.validate().map_err(Error::Invalid)?;
        let client = NomisoClient::connect(store).await?;
        Self::from_client(client, config)
    }

    /// From existing client. Validates limits (serde `TactMemoryConfig` can set zeros).
    pub fn from_client(client: NomisoClient, config: TactMemoryConfig) -> Result<Self> {
        config.limits.validate().map_err(Error::Invalid)?;
        Ok(Self {
            client,
            config,
            scanned: AtomicBool::new(false),
        })
    }

    pub fn config(&self) -> &TactMemoryConfig {
        &self.config
    }

    pub fn client(&self) -> &NomisoClient {
        &self.client
    }

    fn ensure_enabled(&self) -> Result<()> {
        if !self.config.enabled {
            return Err(Error::Disabled);
        }
        Ok(())
    }

    /// Content-free review checkpoint (harness-only; never injects stored records).
    pub fn review_checkpoint_text() -> &'static str {
        "<memory_review_checkpoint>\n\
         Tact control: review the conversation for durable conclusions. \
         If warranted, call memory put/replace/delete; otherwise make no memory call. \
         Do not store transcripts, secrets, or transient plans.\n\
         </memory_review_checkpoint>"
    }

    /// Append the review checkpoint after a user message for harness injection.
    ///
    /// Returns `user_text` + blank line + checkpoint. Does **not** inject corpus content.
    pub fn append_review_checkpoint(user_text: &str) -> String {
        format!(
            "{}\n\n{}",
            user_text.trim_end(),
            Self::review_checkpoint_text()
        )
    }

    /// Scan with BM25 only (vector channel off). Arms put latch. Updates scan telemetry.
    #[instrument(skip(self))]
    pub async fn scan(&self, actor: Actor, query: &str, limit: u32) -> Result<MemoryScan> {
        let _ = actor;
        self.ensure_enabled()?;
        self.prune_probation().await?;

        let q = query.trim();
        if q.is_empty() || !has_searchable_term(q) {
            return Ok(MemoryScan {
                abstained: true,
                candidates: vec![],
            });
        }
        if q.len() > self.config.limits.max_query_bytes {
            return Err(Error::Invalid(format!(
                "query exceeds {} bytes",
                self.config.limits.max_query_bytes
            )));
        }

        let limit = limit.clamp(1, self.config.limits.max_scan_results);
        let hits = self
            .client
            .search(SearchQuery {
                query: q.to_string(),
                scope: self.config.scope.clone(),
                scope_match: ScopeMatch::Exact,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
                categories: Some(vec![Category::Semantic]),
                limit: Some(limit),
                embedding: None,
                graph_enrich: Some(false),
                graph_expand: None,
            })
            .await?;

        // Any completed scan arms put (Tact: agent must scan before storing).
        self.scanned.store(true, Ordering::SeqCst);

        if hits.is_empty() {
            return Ok(MemoryScan {
                abstained: true,
                candidates: vec![],
            });
        }

        let mut candidates = Vec::with_capacity(hits.len());
        for h in hits {
            // Best-effort scan telemetry (no version bump).
            let _ = self.bump_scan_telemetry(&h.id).await;
            let rows = self
                .client
                .read(self.live_read_request(vec![h.id.clone()]))
                .await
                .unwrap_or_default();
            let logical = rows
                .first()
                .and_then(|r| attr_str(r, attr::IDENTITY))
                .unwrap_or_default();
            candidates.push(MemoryCandidate {
                key: MemoryKey {
                    id: h.id,
                    version: h.version,
                },
                logical_id: logical,
                preview: utf8_prefix(&h.preview, self.config.limits.max_preview_bytes),
                score: h.score,
            });
        }

        Ok(MemoryScan {
            abstained: false,
            candidates,
        })
    }

    /// Read full records by id. Updates use (read) telemetry; clears probation via use_count.
    #[instrument(skip(self))]
    pub async fn read(&self, actor: Actor, ids: &[MemoryId]) -> Result<Vec<MemoryRecord>> {
        let _ = actor;
        self.ensure_enabled()?;
        self.prune_probation().await?;
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let rows = self
            .client
            .read(self.live_read_request(ids.to_vec()))
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let rec = self.bump_use_telemetry(r).await?;
            out.push(to_tact_record(&rec)?);
        }
        Ok(out)
    }

    /// Put or replace (root only). Replace uses Nomiso supersede (history retained).
    #[instrument(skip(self, content))]
    pub async fn put(
        &self,
        actor: Actor,
        content: &str,
        replace: Option<ReplaceTarget>,
    ) -> Result<PutResult> {
        self.ensure_enabled()?;
        if !matches!(actor, Actor::Root) {
            return Err(Error::RootOnly);
        }
        if self.config.require_scan_before_put && !self.scanned.load(Ordering::SeqCst) {
            return Err(Error::ScanRequired);
        }

        let content = content.trim();
        self.validate_content(content)?;
        self.prune_probation().await?;
        self.enforce_capacity_for_new(content.len(), replace.as_ref())
            .await?;

        let identity = normalize_identity(content);
        // Unique logical identity among active rows; on replace, allow self prior only.
        self.ensure_unique_identity(&identity, replace.as_ref().map(|r| &r.id))
            .await?;

        let now = Timestamp::now();
        let probation = now
            .checked_add(jiff::SignedDuration::from_secs(
                self.config.limits.probation_secs,
            ))
            .unwrap_or(now);
        let attrs = json!({
            attr::IDENTITY: identity,
            attr::SCAN_COUNT: 0u64,
            attr::USE_COUNT: 0u64,
            attr::PROBATION_UNTIL: probation.to_string(),
            attr::LOGICAL: "tact-on-nomiso",
        });

        let put_body = PutRequest {
            scope: self.config.scope.clone(),
            category: Category::Semantic,
            content: Content {
                text: content.to_string(),
                attrs: Some(attrs),
            },
            valid_from: Some(now),
            valid_until: None,
            known_at: Some(now),
            confidence: Some(0.9),
            provenance: Provenance {
                source: Some("tact-on-nomiso".into()),
                kind: Some("atomic_conclusion".into()),
                span: None,
            },
            entity_links: vec![],
            embedding: None,
            embedding_identity: None,
            idempotency_key: None,
            extractor_version: None,
            model_version: None,
            valid_rev_from: None,
            valid_rev_until: None,
        };

        let (record, replaced) = if let Some(rep) = replace {
            let wr = self
                .client
                .supersede(SupersedeRequest {
                    prior_id: rep.id,
                    expected_version: rep.expected_version,
                    new: put_body,
                    close_at: Some(now),
                })
                .await?;
            let rec = wr
                .record
                .ok_or_else(|| Error::Invalid("supersede returned no record".into()))?;
            (to_tact_record(&rec)?, true)
        } else {
            let wr = self.client.put(put_body).await?;
            let rec = wr
                .record
                .ok_or_else(|| Error::Invalid("put returned no record".into()))?;
            (to_tact_record(&rec)?, false)
        };

        self.scanned.store(false, Ordering::SeqCst);
        Ok(PutResult { record, replaced })
    }

    /// Hard delete (root only). Tact v1 physical remove from retrieval surface.
    #[instrument(skip(self))]
    pub async fn delete(&self, actor: Actor, id: MemoryId, expected_version: u64) -> Result<()> {
        self.ensure_enabled()?;
        if !matches!(actor, Actor::Root) {
            return Err(Error::RootOnly);
        }
        self.client
            .forget(ForgetRequest {
                id,
                scope: self.config.scope.clone(),
                expected_version: Some(expected_version),
                hard: true,
                at: None,
            })
            .await?;
        Ok(())
    }

    /// Count active semantic rows (capacity / health).
    pub async fn count_active(&self) -> Result<usize> {
        self.ensure_enabled()?;
        Ok(self.list_active().await?.len())
    }

    fn validate_content(&self, content: &str) -> Result<()> {
        if content.is_empty() {
            return Err(Error::Rejected("empty content".into()));
        }
        if content.len() > self.config.limits.max_content_bytes {
            return Err(Error::Rejected(format!(
                "content exceeds {} bytes",
                self.config.limits.max_content_bytes
            )));
        }
        if secrets::looks_like_secret(content) {
            return Err(Error::Rejected(
                "content looks like a secret (not stored)".into(),
            ));
        }
        Ok(())
    }

    /// Product point-read: valid-now only (closed / soft-forgotten rows stay off the Tact surface).
    fn live_read_request(&self, ids: Vec<MemoryId>) -> ReadRequest {
        ReadRequest {
            ids,
            scope: self.config.scope.clone(),
            scope_match: ScopeMatch::Exact,
            as_of: Some(Timestamp::now()),
            known_as_of: None,
            sys_as_of: None,
        }
    }

    async fn bump_scan_telemetry(&self, id: &MemoryId) -> Result<()> {
        let rows = self
            .client
            .read(self.live_read_request(vec![id.clone()]))
            .await?;
        let Some(r) = rows.into_iter().next() else {
            return Ok(());
        };
        let mut attrs = r.content.attrs.clone().unwrap_or_else(|| json!({}));
        if let Some(obj) = attrs.as_object_mut() {
            let n = obj
                .get(attr::SCAN_COUNT)
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
                + 1;
            obj.insert(attr::SCAN_COUNT.into(), json!(n));
        }
        let _ = self
            .client
            .annotate(AnnotateRequest {
                id: r.id,
                scope: self.config.scope.clone(),
                expected_version: None,
                confidence: None,
                provenance_source: None,
                provenance_kind: None,
                attrs: Some(attrs),
                bump_version: false,
            })
            .await;
        Ok(())
    }

    async fn bump_use_telemetry(&self, r: nomiso::MemoryRecord) -> Result<nomiso::MemoryRecord> {
        let mut attrs = r.content.attrs.clone().unwrap_or_else(|| json!({}));
        if let Some(obj) = attrs.as_object_mut() {
            let n = obj
                .get(attr::USE_COUNT)
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
                + 1;
            obj.insert(attr::USE_COUNT.into(), json!(n));
        }
        let wr = self
            .client
            .annotate(AnnotateRequest {
                id: r.id.clone(),
                scope: self.config.scope.clone(),
                expected_version: None,
                confidence: None,
                provenance_source: None,
                provenance_kind: None,
                attrs: Some(attrs),
                bump_version: false,
            })
            .await?;
        Ok(wr.record.unwrap_or(r))
    }

    async fn enforce_capacity_for_new(
        &self,
        new_len: usize,
        replace: Option<&ReplaceTarget>,
    ) -> Result<()> {
        let active = self.list_active().await?;
        if replace.is_none() && active.len() >= self.config.limits.max_rows {
            return Err(Error::Capacity(format!(
                "max rows {} reached",
                self.config.limits.max_rows
            )));
        }
        // Total content: new puts add; replace substitutes prior length (if found).
        let prior_len = if let Some(rep) = replace {
            active
                .iter()
                .find(|r| r.id.bare_key() == rep.id.bare_key() || r.id.as_str() == rep.id.as_str())
                .map(|r| r.content.text.len())
                .unwrap_or(0)
        } else {
            0
        };
        let total: usize = active.iter().map(|r| r.content.text.len()).sum();
        let projected = total.saturating_sub(prior_len).saturating_add(new_len);
        if projected > self.config.limits.max_total_content_bytes {
            return Err(Error::Capacity("max total content budget exceeded".into()));
        }
        Ok(())
    }

    /// Exact identity uniqueness via full active list (not BM25-only probe).
    ///
    /// When `except` is set (replace prior), that plane id may keep/change identity;
    /// any *other* active row with the same identity is a conflict.
    async fn ensure_unique_identity(
        &self,
        identity: &str,
        except: Option<&MemoryId>,
    ) -> Result<()> {
        for r in self.list_active().await? {
            if except
                .is_some_and(|e| e.bare_key() == r.id.bare_key() || e.as_str() == r.id.as_str())
            {
                continue;
            }
            if attr_str(&r, attr::IDENTITY).as_deref() == Some(identity) {
                return Err(Error::Duplicate);
            }
        }
        Ok(())
    }

    async fn list_active(&self) -> Result<Vec<nomiso::MemoryRecord>> {
        let page_limit = self.config.limits.max_rows.min(512) as u32;
        let mut out = Vec::new();
        let mut cursor = None;
        for _ in 0..32 {
            let page = self
                .client
                .list(nomiso::ListRequest {
                    scope: self.config.scope.clone(),
                    scope_match: ScopeMatch::Exact,
                    categories: Some(vec![Category::Semantic]),
                    as_of: None,
                    known_as_of: None,
                    sys_as_of: None,
                    text: None,
                    limit: Some(page_limit.max(1)),
                    cursor,
                })
                .await?;
            out.extend(page.items.into_iter().map(|i| i.record));
            if out.len() >= self.config.limits.max_rows {
                out.truncate(self.config.limits.max_rows);
                break;
            }
            match page.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        Ok(out)
    }

    async fn prune_probation(&self) -> Result<()> {
        let now = Timestamp::now();
        let active = self.list_active().await?;
        for r in active {
            let use_count = attr_u64(&r, attr::USE_COUNT).unwrap_or(0);
            if use_count > 0 {
                continue;
            }
            if let Some(until_s) = attr_str(&r, attr::PROBATION_UNTIL) {
                if let Ok(until) = until_s.parse::<Timestamp>() {
                    if until <= now {
                        let _ = self
                            .client
                            .forget(ForgetRequest {
                                id: r.id,
                                scope: self.config.scope.clone(),
                                expected_version: Some(r.version),
                                hard: !self.config.soft_prune,
                                at: None,
                            })
                            .await;
                    }
                }
            }
        }
        Ok(())
    }
}

fn has_searchable_term(q: &str) -> bool {
    q.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|t| t.len() > 1)
}

fn normalize_identity(content: &str) -> String {
    content
        .split_whitespace()
        .map(|s| s.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" ")
}

fn utf8_prefix(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

fn attr_str(r: &nomiso::MemoryRecord, key: &str) -> Option<String> {
    r.content
        .attrs
        .as_ref()?
        .get(key)?
        .as_str()
        .map(|s| s.to_string())
}

fn attr_u64(r: &nomiso::MemoryRecord, key: &str) -> Option<u64> {
    r.content.attrs.as_ref()?.get(key)?.as_u64()
}

fn to_tact_record(r: &nomiso::MemoryRecord) -> Result<MemoryRecord> {
    let probation = attr_str(r, attr::PROBATION_UNTIL).and_then(|s| s.parse().ok());
    let logical_id =
        attr_str(r, attr::IDENTITY).unwrap_or_else(|| normalize_identity(&r.content.text));
    Ok(MemoryRecord {
        key: MemoryKey {
            id: r.id.clone(),
            version: r.version,
        },
        logical_id,
        content: r.content.text.clone(),
        created_at: r.known_at,
        updated_at: r.valid_from,
        scan_count: attr_u64(r, attr::SCAN_COUNT).unwrap_or(0),
        use_count: attr_u64(r, attr::USE_COUNT).unwrap_or(0),
        probation_until: probation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn mem() -> TactMemory {
        TactMemory::connect(
            StoreConfig::memory_test(8),
            TactMemoryConfig {
                require_scan_before_put: true,
                ..Default::default()
            },
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn connect_rejects_invalid_limits() {
        let err = TactMemory::connect(
            StoreConfig::memory_test(8),
            TactMemoryConfig {
                limits: TactLimits {
                    max_scan_results: 0,
                    ..TactLimits::default()
                },
                ..Default::default()
            },
        )
        .await;
        assert!(
            matches!(err, Err(Error::Invalid(_))),
            "expected Invalid limits"
        );
    }

    #[tokio::test]
    async fn disabled_rejects() {
        let m = TactMemory::connect(
            StoreConfig::memory_test(8),
            TactMemoryConfig {
                enabled: false,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let err = m.scan(Actor::Root, "x", 5).await;
        assert!(matches!(err, Err(Error::Disabled)));
    }

    #[tokio::test]
    async fn child_cannot_put() {
        let m = mem().await;
        let err = m
            .put(Actor::Child, "Prefer TypeScript for tooling.", None)
            .await;
        assert!(matches!(err, Err(Error::RootOnly)));
    }

    #[tokio::test]
    async fn put_requires_scan() {
        let m = mem().await;
        let err = m
            .put(Actor::Root, "Prefer TypeScript for tooling.", None)
            .await;
        assert!(matches!(err, Err(Error::ScanRequired)));
    }

    #[tokio::test]
    async fn scan_put_read_roundtrip_and_telemetry() {
        let m = mem().await;
        let _ = m.scan(Actor::Root, "TypeScript tooling", 5).await.unwrap();
        let put = m
            .put(Actor::Root, "Prefer TypeScript for agent tooling.", None)
            .await
            .unwrap();
        assert!(!put.replaced);
        assert!(!put.record.logical_id.is_empty());

        let scan = m.scan(Actor::Child, "TypeScript", 5).await.unwrap();
        assert!(!scan.abstained);
        assert!(!scan.candidates.is_empty());

        let full = m
            .read(Actor::Child, std::slice::from_ref(&put.record.key.id))
            .await
            .unwrap();
        assert_eq!(full.len(), 1);
        assert!(full[0].content.contains("TypeScript"));
        // Read increments use_count; scan increments scan_count.
        assert!(full[0].use_count >= 1);
        assert_eq!(full[0].key.version, 1, "telemetry must not bump version");
    }

    #[tokio::test]
    async fn exact_identity_duplicate_rejected() {
        let m = mem().await;
        let body = "Unique conclusion about widget-alpha-42 only.";
        let _ = m.scan(Actor::Root, "widget-alpha", 5).await.unwrap();
        m.put(Actor::Root, body, None).await.unwrap();
        let _ = m.scan(Actor::Root, "widget-alpha", 5).await.unwrap();
        let err = m.put(Actor::Root, body, None).await;
        assert!(matches!(err, Err(Error::Duplicate)), "{err:?}");
    }

    #[tokio::test]
    async fn probation_prunes_unread() {
        let m = TactMemory::connect(
            StoreConfig::memory_test(8),
            TactMemoryConfig {
                limits: TactLimits {
                    probation_secs: 0, // already expired
                    ..TactLimits::default()
                },
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let _ = m.scan(Actor::Root, "probation-zzz", 5).await.unwrap();
        let put = m
            .put(
                Actor::Root,
                "probation-zzz unread fact that should expire.",
                None,
            )
            .await
            .unwrap();
        // Next scan prunes expired unread.
        let _ = m.scan(Actor::Root, "probation-zzz", 5).await.unwrap();
        let rows = m
            .read(Actor::Root, std::slice::from_ref(&put.record.key.id))
            .await
            .unwrap();
        assert!(rows.is_empty(), "expired unread should be pruned: {rows:?}");
    }

    #[tokio::test]
    async fn rejects_secret() {
        let m = mem().await;
        let _ = m.scan(Actor::Root, "token", 5).await.unwrap();
        let err = m
            .put(Actor::Root, "api_key=sk-abc1234567890secret", None)
            .await;
        assert!(matches!(err, Err(Error::Rejected(_))));
    }

    #[tokio::test]
    async fn rejects_oversized() {
        let m = mem().await;
        let _ = m.scan(Actor::Root, "big", 5).await.unwrap();
        let big = "x".repeat(2000);
        let err = m.put(Actor::Root, &big, None).await;
        assert!(matches!(err, Err(Error::Rejected(_))));
    }

    #[tokio::test]
    async fn delete_root_only() {
        let m = mem().await;
        let _ = m
            .scan(Actor::Root, "delete-me unique-fact-zzz", 5)
            .await
            .unwrap();
        let put = m
            .put(
                Actor::Root,
                "unique-fact-zzz delete target conclusion.",
                None,
            )
            .await
            .unwrap();
        let err = m
            .delete(
                Actor::Child,
                put.record.key.id.clone(),
                put.record.key.version,
            )
            .await;
        assert!(matches!(err, Err(Error::RootOnly)));
        m.delete(Actor::Root, put.record.key.id, put.record.key.version)
            .await
            .unwrap();
    }

    #[test]
    fn checkpoint_is_content_free() {
        let t = TactMemory::review_checkpoint_text();
        assert!(t.contains("memory_review_checkpoint"));
        assert!(!t.contains("memory:"));
        let wrapped = TactMemory::append_review_checkpoint("hello user");
        assert!(wrapped.starts_with("hello user"));
        assert!(wrapped.contains("memory_review_checkpoint"));
    }

    #[test]
    fn empty_query_has_no_searchable_term() {
        assert!(!has_searchable_term("   "));
        assert!(!has_searchable_term("a"));
        assert!(has_searchable_term("ab"));
    }

    #[tokio::test]
    async fn capacity_max_rows_enforced() {
        let m = TactMemory::connect(
            StoreConfig::memory_test(8),
            TactMemoryConfig {
                limits: TactLimits {
                    max_rows: 2,
                    max_total_content_bytes: 256 * 1024,
                    ..TactLimits::default()
                },
                require_scan_before_put: false,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        m.put(Actor::Root, "capacity row one unique-alpha.", None)
            .await
            .unwrap();
        m.put(Actor::Root, "capacity row two unique-beta.", None)
            .await
            .unwrap();
        let err = m
            .put(Actor::Root, "capacity row three unique-gamma.", None)
            .await;
        assert!(matches!(err, Err(Error::Capacity(_))), "{err:?}");
    }

    #[tokio::test]
    async fn capacity_total_bytes_enforced() {
        let m = TactMemory::connect(
            StoreConfig::memory_test(8),
            TactMemoryConfig {
                limits: TactLimits {
                    max_rows: 100,
                    max_total_content_bytes: 80,
                    max_content_bytes: 60,
                    ..TactLimits::default()
                },
                require_scan_before_put: false,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let a = "a".repeat(50);
        m.put(Actor::Root, &a, None).await.unwrap();
        let b = "b".repeat(50);
        let err = m.put(Actor::Root, &b, None).await;
        assert!(matches!(err, Err(Error::Capacity(_))), "{err:?}");
    }

    #[tokio::test]
    async fn replace_supersede_and_version_conflict() {
        let m = mem().await;
        let _ = m.scan(Actor::Root, "replace-target", 5).await.unwrap();
        let put = m
            .put(
                Actor::Root,
                "replace-target original conclusion for version check.",
                None,
            )
            .await
            .unwrap();
        let _ = m.scan(Actor::Root, "replace-target", 5).await.unwrap();
        let bad = m
            .put(
                Actor::Root,
                "replace-target revised conclusion with wrong version.",
                Some(ReplaceTarget {
                    id: put.record.key.id.clone(),
                    expected_version: 99,
                }),
            )
            .await;
        assert!(
            matches!(bad, Err(Error::Nomiso(nomiso::Error::Conflict { .. }))),
            "{bad:?}"
        );
        let _ = m.scan(Actor::Root, "replace-target", 5).await.unwrap();
        let ok = m
            .put(
                Actor::Root,
                "replace-target revised conclusion with correct version.",
                Some(ReplaceTarget {
                    id: put.record.key.id.clone(),
                    expected_version: put.record.key.version,
                }),
            )
            .await
            .unwrap();
        assert!(ok.replaced);
        assert_ne!(ok.record.key.id.as_str(), put.record.key.id.as_str());
        // Logical identity tracks content, not plane id.
        assert!(!ok.record.logical_id.is_empty());
    }

    #[tokio::test]
    async fn replace_total_budget_rechecked() {
        let m = TactMemory::connect(
            StoreConfig::memory_test(8),
            TactMemoryConfig {
                limits: TactLimits {
                    max_rows: 10,
                    max_total_content_bytes: 60,
                    max_content_bytes: 80,
                    ..TactLimits::default()
                },
                require_scan_before_put: false,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let short = "short-budget-aaaaaaaaaa"; // 22 chars
        let put = m.put(Actor::Root, short, None).await.unwrap();
        // Another active row fills most of the budget.
        m.put(Actor::Root, "other-budget-bbbbbbbbbb", None)
            .await
            .unwrap();
        // Replace first with much longer text → projected total exceeds.
        let long = "L".repeat(50);
        let err = m
            .put(
                Actor::Root,
                &long,
                Some(ReplaceTarget {
                    id: put.record.key.id,
                    expected_version: put.record.key.version,
                }),
            )
            .await;
        assert!(matches!(err, Err(Error::Capacity(_))), "{err:?}");
    }

    #[tokio::test]
    async fn put_requires_re_scan_after_successful_put() {
        let m = mem().await;
        let _ = m.scan(Actor::Root, "rearm", 5).await.unwrap();
        m.put(Actor::Root, "rearm first conclusion about latch.", None)
            .await
            .unwrap();
        // Latch consumed — second put without scan fails.
        let err = m
            .put(Actor::Root, "rearm second without scan should fail.", None)
            .await;
        assert!(matches!(err, Err(Error::ScanRequired)), "{err:?}");
    }

    #[tokio::test]
    async fn read_valid_now_excludes_superseded() {
        let m = mem().await;
        let _ = m.scan(Actor::Root, "valid-now-prior", 5).await.unwrap();
        let put = m
            .put(
                Actor::Root,
                "valid-now-prior original conclusion for closed-read check.",
                None,
            )
            .await
            .unwrap();
        let old_id = put.record.key.id.clone();
        let _ = m.scan(Actor::Root, "valid-now-prior", 5).await.unwrap();
        let replaced = m
            .put(
                Actor::Root,
                "valid-now-prior revised conclusion after supersede.",
                Some(ReplaceTarget {
                    id: old_id.clone(),
                    expected_version: put.record.key.version,
                }),
            )
            .await
            .unwrap();
        assert!(replaced.replaced);
        let closed = m
            .read(Actor::Root, std::slice::from_ref(&old_id))
            .await
            .unwrap();
        assert!(
            closed.is_empty(),
            "superseded prior must not be a live Tact read: {closed:?}"
        );
        let live = m
            .read(Actor::Root, std::slice::from_ref(&replaced.record.key.id))
            .await
            .unwrap();
        assert_eq!(live.len(), 1);
        assert!(live[0].content.contains("revised"));
    }
}
