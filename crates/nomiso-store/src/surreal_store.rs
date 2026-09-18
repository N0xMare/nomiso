//! SurrealDB-backed MemoryStore implementation (SurrealDB SDK 3.x).

use std::sync::Arc;

use async_trait::async_trait;
use nomiso_core::embedding::{
    DeclareGenerationRequest, EmbeddingGeneration, EmbeddingIdentity, EmbeddingNormalization,
    EmbeddingState, GenerationStatus, SourceFrontier, StagedEmbedding,
};
use nomiso_core::error::{Error, Result};
use nomiso_core::job::{
    validate_enqueue, validate_job_body, ClaimJobRequest, EnqueueJobRequest, EnqueueJobResult,
    JobAttempt, JobError, JobInput, JobIntent, JobLease, JobRecord, JobState, JobSummary,
    ListJobsRequest, WriteWithJobsResult, JOB_HISTORY_MAX,
};
use nomiso_core::ops::{
    ForgetRequest, PutRequest, ReadRequest, SearchQuery, SupersedeRequest, WriteResult,
};
use nomiso_core::scope::{ScopeMatch, ScopePath};
use nomiso_core::store::MemoryStore;
use nomiso_core::types::{MemoryId, ScoreKind, SearchHit, SearchSignals};
// ScoreKind used by list() text path and search honesty.
use nomiso_core::validate::{
    check_version, validate_forget, validate_interval_after_defaults, validate_put, validate_read,
    validate_search, validate_supersede,
};
use nomiso_schema::{migrations, MigrateOptions, SCHEMA_VERSION};
use serde_json::{json, Value};
use surrealdb::engine::any::{self, Any};
use surrealdb::{IndexedResults, Surreal};
use tracing::{debug, instrument};
use uuid::Uuid;

use crate::config::StoreConfig;
use crate::mapping::{
    now, record_id_to_string, row_to_record, search_row_to_hit, ts_param, value_to_memory_row,
    value_to_search_row, MemoryRow,
};

/// Production store wrapping a SurrealDB client.
#[derive(Clone)]
pub struct SurrealMemoryStore {
    db: Arc<Surreal<Any>>,
    config: StoreConfig,
}

impl SurrealMemoryStore {
    /// Connect using config (supports `memory`, `ws://…`, file engines via any).
    pub async fn connect(config: StoreConfig) -> Result<Self> {
        let config = config.normalized();
        let endpoint = normalize_endpoint(&config.endpoint);
        let db = any::connect(&endpoint)
            .await
            .map_err(|e| Error::store(format!("connect {endpoint}: {e}")))?;
        db.use_ns(&config.namespace)
            .use_db(&config.database)
            .await
            .map_err(|e| Error::store(format!("use_ns/db: {e}")))?;

        if let (Some(user), Some(pass)) = (&config.username, &config.password) {
            db.signin(surrealdb::opt::auth::Root {
                username: user.clone(),
                password: pass.clone(),
            })
            .await
            .map_err(|e| Error::store(format!("signin: {e}")))?;
            db.use_ns(&config.namespace)
                .use_db(&config.database)
                .await
                .map_err(|e| Error::store(format!("use_ns/db after signin: {e}")))?;
        }

        Ok(Self {
            db: Arc::new(db),
            config,
        })
    }

    /// Access config.
    pub fn config(&self) -> &StoreConfig {
        &self.config
    }

    async fn fetch_memory_raw(&self, id: &MemoryId) -> Result<Option<MemoryRow>> {
        let key = id.bare_key().to_string();
        let sql = "SELECT * FROM type::record('memory', $key);";
        let mut response = self
            .db
            .query(sql)
            .bind(("key", key))
            .await
            .map_err(|e| Error::store(e.to_string()))?;
        ensure_ok(&mut response)?;
        let rows = take_json_rows(&mut response, 0)?;
        if let Some(v) = rows.into_iter().next() {
            Ok(Some(value_to_memory_row(v)?))
        } else {
            Ok(None)
        }
    }

    async fn conflict_from_memory(&self, id: &MemoryId, expected: u64) -> Error {
        match self.fetch_memory_raw(id).await {
            Ok(Some(row)) => Error::Conflict {
                expected,
                found: row.version,
            },
            Ok(None) => Error::NotFound(id.to_string()),
            Err(e) => e,
        }
    }

    async fn task_state_conflict(&self, scope: &str, slot: &str, expected: Option<u64>) -> Error {
        let found = match self
            .get_task_state(nomiso_core::task_state::GetTaskStateRequest {
                scope: scope.into(),
                slot: slot.into(),
            })
            .await
        {
            Ok(Some(rec)) => rec.version,
            Ok(None) => 0,
            Err(_) => 0,
        };
        Error::Conflict {
            expected: expected.unwrap_or(0),
            found,
        }
    }

    fn idempotency_slot_id(scope: &str, key: &str) -> String {
        // Record-id safe hex (scope + unit separator + key). Sidecar PK, not NONE-unique.
        let mut out = String::with_capacity((scope.len() + key.len() + 1) * 2);
        for b in scope.as_bytes() {
            out.push_str(&format!("{b:02x}"));
        }
        out.push_str("1f");
        for b in key.as_bytes() {
            out.push_str(&format!("{b:02x}"));
        }
        out
    }

    fn scope_allowed(stored: &str, query_scope: &ScopePath, mode: ScopeMatch) -> bool {
        query_scope.matches(stored, mode)
    }

    /// Decode one `embedding_generation` row into the typed record.
    fn decode_generation(v: &Value) -> Result<EmbeddingGeneration> {
        let get = |k: &str| v.get(k).cloned().unwrap_or(Value::Null);
        let num = |k: &str| -> Result<u64> {
            get(k)
                .as_u64()
                .or_else(|| get(k).as_i64().map(|i| i.max(0) as u64))
                .ok_or_else(|| Error::store(format!("embedding_generation missing {k}")))
        };
        let opt_num = |k: &str| -> Option<u64> {
            get(k)
                .as_u64()
                .or_else(|| get(k).as_i64().map(|i| i.max(0) as u64))
        };
        let status = match get("status").as_str().unwrap_or_default() {
            "building" => GenerationStatus::Building,
            "active" => GenerationStatus::Active,
            "retired" => GenerationStatus::Retired,
            "failed" => GenerationStatus::Failed,
            s => return Err(Error::store(format!("unknown generation status '{s}'"))),
        };
        let normalization = match get("normalization").as_str().unwrap_or("unknown") {
            "l2" => EmbeddingNormalization::L2,
            "none" => EmbeddingNormalization::None,
            _ => EmbeddingNormalization::Unknown,
        };
        let opt_ts = |k: &str| -> Result<Option<jiff::Timestamp>> {
            match get(k) {
                Value::Null => Ok(None),
                v => Ok(Some(crate::mapping::parse_timestamp(&v)?)),
            }
        };
        let source_frontier = opt_ts("frontier_captured_at")?.map(|captured_at| SourceFrontier {
            captured_at,
            generation: opt_num("frontier_generation"),
            expected_count: opt_num("expected_count").unwrap_or(0),
        });
        Ok(EmbeddingGeneration {
            generation: num("generation")?,
            identity: EmbeddingIdentity {
                family: get("family").as_str().unwrap_or_default().to_string(),
                model: get("model").as_str().unwrap_or_default().to_string(),
                dimension: num("dimension")? as u32,
                normalization,
                encoding: get("encoding").as_str().unwrap_or("f32").to_string(),
                limitation: match get("limitation") {
                    Value::Null => None,
                    v => v.as_str().map(|s| s.to_string()),
                },
            },
            status,
            source_frontier,
            embedded_count: opt_num("embedded_count").unwrap_or(0),
            note: match get("note") {
                Value::Null => None,
                v => v.as_str().map(|s| s.to_string()),
            },
            created_at: opt_ts("created_at")?.unwrap_or_else(now),
            activated_at: opt_ts("activated_at")?,
        })
    }

    /// All generations ordered by generation number.
    async fn list_generations(&self) -> Result<Vec<EmbeddingGeneration>> {
        let mut response = self
            .db
            .query("SELECT * FROM embedding_generation ORDER BY generation;")
            .await
            .map_err(|e| Error::store(format!("list generations: {e}")))?;
        ensure_ok(&mut response)?;
        let rows: Vec<Value> = response
            .take(0)
            .map_err(|e| Error::store(format!("decode generations: {e}")))?;
        rows.iter().map(Self::decode_generation).collect()
    }

    /// The single active generation, if any.
    async fn active_generation(&self) -> Result<Option<EmbeddingGeneration>> {
        let mut response = self
            .db
            .query(
                "SELECT * FROM embedding_generation WHERE status = 'active' ORDER BY generation DESC LIMIT 1;",
            )
            .await
            .map_err(|e| Error::store(format!("active generation: {e}")))?;
        ensure_ok(&mut response)?;
        let rows: Vec<Value> = response
            .take(0)
            .map_err(|e| Error::store(format!("decode active generation: {e}")))?;
        rows.first().map(Self::decode_generation).transpose()
    }

    /// Count of memory rows currently carrying a vector.
    async fn embedded_memory_count(&self) -> Result<u64> {
        let mut response = self
            .db
            .query("SELECT count() AS c FROM memory WHERE embedding IS NOT NONE GROUP ALL;")
            .await
            .map_err(|e| Error::store(format!("embedded count: {e}")))?;
        ensure_ok(&mut response)?;
        let rows: Vec<Value> = response
            .take(0)
            .map_err(|e| Error::store(format!("decode embedded count: {e}")))?;
        Ok(rows
            .first()
            .and_then(|r| r.get("c"))
            .and_then(|c| c.as_u64().or_else(|| c.as_i64().map(|i| i.max(0) as u64)))
            .unwrap_or(0))
    }

    /// MIG-004/005 bootstrap + declared-identity guard, run inside `migrate`
    /// after schema DDL and before the marker advances.
    async fn ensure_embedding_generation(&self) -> Result<()> {
        if let Some(declared) = &self.config.embedding_identity {
            if declared.dimension as usize != self.config.embedding_dim {
                return Err(Error::invalid(format!(
                    "declared embedding identity dimension {} does not match store embedding_dim {}",
                    declared.dimension, self.config.embedding_dim
                )));
            }
        }
        let gens = self.list_generations().await?;
        if let Some(active) = gens.iter().find(|g| g.status == GenerationStatus::Active) {
            // Fail closed on any incompatible declared identity — including
            // declaring a named model over an unknown/legacy generation. The
            // fix path is attest_embedding_identity or a new generation.
            if let Some(declared) = &self.config.embedding_identity {
                if !declared.compatible_with(&active.identity) {
                    return Err(Error::IncompatibleStore(format!(
                        "declared embedding identity {}/{} (norm {:?}) does not match active generation {} ({}/{}, norm {:?}); attest the legacy identity or declare a new generation",
                        declared.family,
                        declared.model,
                        declared.normalization,
                        active.generation,
                        active.identity.family,
                        active.identity.model,
                        active.identity.normalization,
                    )));
                }
            }
            return Ok(());
        }
        if !gens.is_empty() {
            // Generations exist but none is active — leave resolution to an
            // explicit activate call rather than picking one implicitly.
            return Ok(());
        }
        // No generation rows: first bootstrap captures the current frontier.
        let embedded = self.embedded_memory_count().await?;
        let identity = self
            .config
            .embedding_identity
            .clone()
            .unwrap_or_else(|| EmbeddingIdentity::unknown(self.config.embedding_dim as u32));
        let mut response = self
            .db
            .query(
                r#"
                CREATE type::record('embedding_generation', 'gen-1') SET
                    generation = 1,
                    family = $family,
                    model = $model,
                    dimension = $dim,
                    normalization = $norm,
                    encoding = $encoding,
                    limitation = $limitation,
                    status = 'active',
                    frontier_captured_at = NONE,
                    frontier_generation = NONE,
                    expected_count = $expected,
                    embedded_count = $expected,
                    note = 'initial generation',
                    created_at = time::now(),
                    activated_at = time::now();
                "#,
            )
            .bind(("family", identity.family.clone()))
            .bind(("model", identity.model.clone()))
            .bind(("dim", identity.dimension as i64))
            .bind((
                "norm",
                match identity.normalization {
                    EmbeddingNormalization::L2 => "l2",
                    EmbeddingNormalization::None => "none",
                    EmbeddingNormalization::Unknown => "unknown",
                },
            ))
            .bind(("encoding", identity.encoding.clone()))
            .bind(("limitation", identity.limitation.clone()))
            .bind(("expected", embedded as i64))
            .await
            .map_err(|e| Error::store(format!("create generation 1: {e}")))?;
        match ensure_ok(&mut response) {
            Ok(()) => Ok(()),
            // A concurrent migrator won the claim — its row is authoritative;
            // re-run the check so a winner's identity is validated the same way.
            Err(e) if is_state_create_conflict(&e) => {
                Box::pin(self.ensure_embedding_generation()).await
            }
            Err(e) => Err(e),
        }
    }

    /// Resolve which generation a written vector belongs to (MIG-004).
    ///
    /// - No claim: stamped with the current active generation (None when the
    ///   store predates generation tracking).
    /// - Claim matching the active identity: stamped.
    /// - Claim against an unknown active identity with zero embedded rows:
    ///   adopted into the generation (first-write bootstrap).
    /// - Anything else: fails closed — never silently mixes.
    async fn resolve_embedding_for_write(
        &self,
        claimed: Option<&EmbeddingIdentity>,
    ) -> Result<Option<i64>> {
        let active = self.active_generation().await?;
        match (claimed, active) {
            (None, gen) => Ok(gen.map(|g| g.generation as i64)),
            (Some(c), Some(gen)) => {
                if c.compatible_with(&gen.identity) {
                    return Ok(Some(gen.generation as i64));
                }
                if gen.identity.is_unknown() {
                    if self.embedded_memory_count().await? > 0 {
                        return Err(Error::IncompatibleStore(format!(
                            "active embedding generation {} has unknown identity with {} embedded rows; call attest_embedding_identity or declare a new generation",
                            gen.generation,
                            self.embedded_memory_count().await?
                        )));
                    }
                    // Conditional adopt: only flip while still unknown.
                    let mut r = self
                        .db
                        .query(
                            r#"
                            UPDATE embedding_generation SET
                                family = $family,
                                model = $model,
                                normalization = $norm,
                                encoding = $encoding,
                                limitation = $limitation
                            WHERE generation = $gen
                              AND family = 'unknown' AND model = 'unknown';
                            "#,
                        )
                        .bind(("family", c.family.clone()))
                        .bind(("model", c.model.clone()))
                        .bind((
                            "norm",
                            match c.normalization {
                                EmbeddingNormalization::L2 => "l2",
                                EmbeddingNormalization::None => "none",
                                EmbeddingNormalization::Unknown => "unknown",
                            },
                        ))
                        .bind(("encoding", c.encoding.clone()))
                        .bind(("limitation", c.limitation.clone()))
                        .bind(("gen", gen.generation as i64))
                        .await
                        .map_err(|e| Error::store(format!("adopt embedding identity: {e}")))?;
                    ensure_ok(&mut r)?;
                    if let Some(now_active) = self.active_generation().await? {
                        if c.compatible_with(&now_active.identity) {
                            return Ok(Some(now_active.generation as i64));
                        }
                    }
                    return Err(Error::IncompatibleStore(
                        "concurrent embedding identity adoption produced a different identity"
                            .into(),
                    ));
                }
                Err(Error::IncompatibleStore(format!(
                    "claimed embedding identity {}/{} (norm {:?}) is incompatible with active generation {} ({}/{}, norm {:?})",
                    c.family,
                    c.model,
                    c.normalization,
                    gen.generation,
                    gen.identity.family,
                    gen.identity.model,
                    gen.identity.normalization,
                )))
            }
            (Some(c), None) => Err(Error::IncompatibleStore(format!(
                "no active embedding generation to accept claimed identity {}/{}",
                c.family, c.model
            ))),
        }
    }

    async fn create_memory(
        &self,
        req: &PutRequest,
        supersedes: Option<&str>,
    ) -> Result<nomiso_core::types::MemoryRecord> {
        self.create_memory_inner(req, supersedes, None, "", &[])
            .await
    }

    /// Create a memory row inside one transaction, optionally with a
    /// caller-assigned record key and extra in-transaction statement
    /// fragments (e.g. atomic job inserts, JOB-001).
    async fn create_memory_inner(
        &self,
        req: &PutRequest,
        supersedes: Option<&str>,
        key_override: Option<String>,
        extra_sql: &str,
        extra_binds: &[(String, Option<Value>)],
    ) -> Result<nomiso_core::types::MemoryRecord> {
        let key = key_override.unwrap_or_else(|| Uuid::now_v7().to_string());
        let valid_from_ts = req.valid_from.unwrap_or_else(now);
        validate_interval_after_defaults(valid_from_ts, req.valid_until)?;
        let valid_from = ts_param(valid_from_ts);
        let known_at = ts_param(req.known_at.unwrap_or_else(now));
        let valid_until = req.valid_until.map(ts_param);
        let mut content = json!({ "text": req.content.text });
        if let Some(attrs) = &req.content.attrs {
            content["attrs"] = attrs.clone();
        }
        let supersedes_key = supersedes.map(|s| s.to_string());
        let event_id = Uuid::now_v7().to_string();
        // MIG-004: stamp the generation the vector belongs to (validated
        // against claimed identity; fails closed on mismatch).
        let egen: Option<i64> = if req.embedding.is_some() {
            self.resolve_embedding_for_write(req.embedding_identity.as_ref())
                .await?
        } else {
            None
        };

        let create_stmt = if supersedes_key.is_some() {
            r#"
            CREATE type::record('memory', $key) SET
                category = $category,
                scope = $scope,
                content = $content,
                valid_from = type::datetime($valid_from),
                known_at = type::datetime($known_at),
                valid_until = IF $valid_until = NONE THEN NONE ELSE type::datetime($valid_until) END,
                confidence = $confidence,
                provenance = $provenance,
                version = 1,
                entity_links = $entity_links,
                embedding = $embedding,
                embedding_generation = $egen,
                supersedes = type::record('memory', $supersedes_key),
                extractor_version = $extractor_version,
                model_version = $model_version,
                idempotency_key = $idempotency_key,
                valid_rev_from = $valid_rev_from,
                valid_rev_until = $valid_rev_until,
                sys_created = time::now(),
                sys_updated = time::now()
            RETURN AFTER
            "#
        } else {
            r#"
            CREATE type::record('memory', $key) SET
                category = $category,
                scope = $scope,
                content = $content,
                valid_from = type::datetime($valid_from),
                known_at = type::datetime($known_at),
                valid_until = IF $valid_until = NONE THEN NONE ELSE type::datetime($valid_until) END,
                confidence = $confidence,
                provenance = $provenance,
                version = 1,
                entity_links = $entity_links,
                embedding = $embedding,
                embedding_generation = $egen,
                extractor_version = $extractor_version,
                model_version = $model_version,
                idempotency_key = $idempotency_key,
                valid_rev_from = $valid_rev_from,
                valid_rev_until = $valid_rev_until,
                sys_created = time::now(),
                sys_updated = time::now()
            RETURN AFTER
            "#
        };
        // WRITE-003/010: the required assert journal event commits with the
        // mutation or not at all — no best-effort post-commit insert.
        let sql = format!(
            r#"
            BEGIN TRANSACTION;
            LET $created = ({create_stmt});
            CREATE type::record('belief_event', $eid) SET
                scope = $scope,
                memory_id = $mid,
                kind = 'assert',
                at_sys = time::now(),
                payload = $event_payload;
            {extra_sql}
            RETURN $created;
            COMMIT TRANSACTION;
            "#
        );
        let mut q = self
            .db
            .query(sql)
            .bind(("key", key.clone()))
            .bind(("eid", event_id))
            .bind(("mid", format!("memory:{key}")))
            .bind(("event_payload", json!({ "version": 1 })))
            .bind(("category", req.category.as_str().to_string()))
            .bind(("scope", req.scope.clone()))
            .bind(("content", content))
            .bind(("valid_from", valid_from))
            .bind(("known_at", known_at))
            .bind(("valid_until", valid_until))
            .bind(("confidence", req.confidence))
            .bind(("provenance", json!(req.provenance)))
            .bind(("entity_links", req.entity_links.clone()))
            .bind(("embedding", req.embedding.clone()))
            .bind(("egen", egen))
            .bind(("extractor_version", req.extractor_version.clone()))
            .bind(("model_version", req.model_version.clone()))
            .bind(("idempotency_key", req.idempotency_key.clone()))
            .bind(("valid_rev_from", req.valid_rev_from.clone()))
            .bind(("valid_rev_until", req.valid_rev_until.clone()));
        if let Some(sk) = supersedes_key {
            q = q.bind(("supersedes_key", sk));
        }
        for (k, v) in extra_binds {
            q = q.bind((k.clone(), v.clone()));
        }
        let mut response = q.await.map_err(|e| Error::store(e.to_string()))?;
        ensure_ok(&mut response).map_err(job_txn_error)?;
        // Scan statement results for the created memory row (BEGIN/LET/CREATE
        // event slots precede the RETURNed row inside the transaction; the
        // index moves when in-transaction job fragments are appended).
        let n = response.num_statements();
        for i in 0..n {
            let Ok(rows) = take_json_rows(&mut response, i) else {
                continue;
            };
            for v in rows {
                let objs = match v {
                    Value::Array(a) => a,
                    other => vec![other],
                };
                for c in objs {
                    if let Ok(row) = value_to_memory_row(c) {
                        if let Ok(rec) = row_to_record(row) {
                            return Ok(rec);
                        }
                    }
                }
            }
        }
        Err(Error::store("create returned no row"))
    }

    /// Read a `nomiso_meta` singleton value (`schema` / `embedding_dim`).
    async fn meta_value(&self, key: &str) -> Result<Option<Value>> {
        let mut response = self
            .db
            .query("SELECT * FROM type::record('nomiso_meta', $k);")
            .bind(("k", key.to_string()))
            .await
            .map_err(|e| Error::store(format!("meta read {key}: {e}")))?;
        let _ = ensure_ok(&mut response);
        let rows = take_json_rows(&mut response, 0)
            .map_err(|e| Error::store(format!("meta decode {key}: {e}")))?;
        Ok(rows
            .into_iter()
            .next()
            .and_then(|v| v.get("value").cloned()))
    }

    /// Claim a migration row: create 'applying', steal stale claims, or verify
    /// an existing completed row. Returns true when this caller holds the claim.
    async fn claim_migration(&self, name: &str, checksum: &str) -> Result<bool> {
        use std::time::Instant;
        let deadline = Instant::now() + std::time::Duration::from_secs(30);
        loop {
            // Fast path: existing row decides.
            let mut read = self
                .db
                .query("SELECT * FROM type::record('schema_migration', $n);")
                .bind(("n", name.to_string()))
                .await
                .map_err(|e| Error::store(format!("ledger read {name}: {e}")))?;
            ensure_ok(&mut read)?;
            let rows = take_json_rows(&mut read, 0)?;
            match rows.into_iter().next() {
                Some(row) => {
                    let status = row.get("status").and_then(Value::as_str).unwrap_or("");
                    let stored = row.get("checksum").and_then(Value::as_str).unwrap_or("");
                    match status {
                        "applied" if stored == checksum => return Ok(false),
                        "applied" => {
                            return Err(Error::IncompatibleStore(format!(
                                "migration {name} was applied with different content \
                                 (stored checksum {stored}); refusing to relabel it"
                            )));
                        }
                        "applying" => {
                            // Steal a claim abandoned >120s; otherwise wait.
                            let mut steal = self
                                .db
                                .query(
                                    r#"
                                    UPDATE type::record('schema_migration', $n)
                                    SET started_at = time::now(), checksum = $c, error = NONE
                                    WHERE status = 'applying'
                                      AND started_at < time::now() - 120s
                                    RETURN AFTER;
                                    "#,
                                )
                                .bind(("n", name.to_string()))
                                .bind(("c", checksum.to_string()))
                                .await
                                .map_err(|e| Error::store(format!("ledger steal {name}: {e}")))?;
                            ensure_ok(&mut steal)?;
                            if !take_json_rows(&mut steal, 0)?.is_empty() {
                                return Ok(true);
                            }
                        }
                        "failed" => {
                            // Resume an interrupted migration: reclaim it.
                            let mut claim = self
                                .db
                                .query(
                                    r#"
                                    UPDATE type::record('schema_migration', $n)
                                    SET status = 'applying', started_at = time::now(),
                                        checksum = $c, error = NONE
                                    WHERE status = 'failed'
                                    RETURN AFTER;
                                    "#,
                                )
                                .bind(("n", name.to_string()))
                                .bind(("c", checksum.to_string()))
                                .await
                                .map_err(|e| Error::store(format!("ledger reclaim {name}: {e}")))?;
                            ensure_ok(&mut claim)?;
                            if !take_json_rows(&mut claim, 0)?.is_empty() {
                                return Ok(true);
                            }
                        }
                        other => {
                            return Err(Error::IncompatibleStore(format!(
                                "migration {name} has unknown ledger status '{other}'"
                            )));
                        }
                    }
                }
                None => {
                    let mut create = self
                        .db
                        .query(
                            r#"
                            CREATE type::record('schema_migration', $n) SET
                                checksum = $c,
                                status = 'applying',
                                started_at = time::now(),
                                applied_at = NONE,
                                error = NONE;
                            "#,
                        )
                        .bind(("n", name.to_string()))
                        .bind(("c", checksum.to_string()))
                        .await
                        .map_err(|e| Error::store(format!("ledger claim {name}: {e}")))?;
                    match ensure_ok(&mut create) {
                        Ok(()) => return Ok(true),
                        Err(e) if is_state_create_conflict(&e) => continue,
                        Err(e) => return Err(e),
                    }
                }
            }
            if Instant::now() > deadline {
                return Err(Error::DeadlineExceeded);
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    }

    fn filter_scope(
        &self,
        hits: Vec<SearchHit>,
        scope: &ScopePath,
        mode: ScopeMatch,
    ) -> Vec<SearchHit> {
        hits.into_iter()
            .filter(|h| Self::scope_allowed(&h.scope, scope, mode))
            .collect()
    }

    async fn filter_known_sys(
        &self,
        hits: Vec<SearchHit>,
        known_as_of: Option<nomiso_core::types::Timestamp>,
        sys_as_of: Option<nomiso_core::types::Timestamp>,
        scope: &str,
        scope_match: ScopeMatch,
    ) -> Result<Vec<SearchHit>> {
        if hits.is_empty() {
            return Ok(hits);
        }
        let ids: Vec<_> = hits.iter().map(|h| h.id.clone()).collect();
        let rows = self
            .read(ReadRequest {
                ids,
                scope: scope.into(),
                scope_match,
                as_of: None,
                known_as_of,
                sys_as_of,
            })
            .await?;
        let ok: std::collections::HashSet<_> = rows
            .into_iter()
            .map(|r| r.id.as_str().to_string())
            .collect();
        Ok(hits
            .into_iter()
            .filter(|h| ok.contains(h.id.as_str()) || ok.contains(h.id.bare_key()))
            .collect())
    }

    /// Graph candidate expansion (T5 experiment, spec 05 "Graph expansion
    /// policy"). Seeds = top direct hits; a bounded BFS over `active`
    /// relationship edges under the same scope/temporal/category filters
    /// discovers additional memory candidates. Discovery provenance merges
    /// onto direct hits; expansion-only candidates append below all direct
    /// hits with a labeled `RankFallback` score — association never becomes
    /// endorsement.
    async fn expand_candidates(
        &self,
        hits: &mut Vec<SearchHit>,
        ctx: &ExpandCtx<'_>,
    ) -> Result<nomiso_core::ops::ExpansionStats> {
        let query = ctx.query;
        let scope = ctx.scope;
        let as_of_s = ctx.as_of_s;
        let known_as_of = ctx.known_as_of;
        let sys_as_of = ctx.sys_as_of;
        let categories = ctx.categories;
        let gx = ctx.gx;
        use nomiso_core::ops::{
            GRAPH_EXPAND_MAX_CANDIDATES, GRAPH_EXPAND_MAX_DEPTH, GRAPH_EXPAND_MAX_EDGES,
            GRAPH_EXPAND_MAX_SEEDS,
        };
        use nomiso_core::relationship::{RelationPredicate, TraverseDirection};
        use std::collections::{HashMap, HashSet, VecDeque};
        use std::time::Instant;

        let started = Instant::now();
        let deadline = started
            + std::time::Duration::from_millis(nomiso_core::relationship::DEFAULT_DEADLINE_MS);
        let max_depth = gx.max_depth.unwrap_or(1).clamp(1, GRAPH_EXPAND_MAX_DEPTH);
        let max_seeds = gx.max_seeds.unwrap_or(8).clamp(1, GRAPH_EXPAND_MAX_SEEDS);
        let max_candidates = gx
            .max_candidates
            .unwrap_or(32)
            .clamp(1, GRAPH_EXPAND_MAX_CANDIDATES);

        let preds: Vec<RelationPredicate> = match &gx.predicates {
            Some(v) if !v.is_empty() => {
                if v.iter().any(|p| p.spec().foundation_only) {
                    return Err(Error::invalid(
                        "foundation-maintained predicates are not expandable",
                    ));
                }
                v.clone()
            }
            _ => vec![
                RelationPredicate::Supports,
                RelationPredicate::DerivedFrom,
                RelationPredicate::Contradicts,
                RelationPredicate::Mentions,
                RelationPredicate::DependsOn,
                RelationPredicate::AppliesTo,
                RelationPredicate::ObservedIn,
                RelationPredicate::Attempted,
                RelationPredicate::ResolvedBy,
            ],
        };
        let pred_names: Vec<String> = preds.iter().map(|p| p.as_str().to_string()).collect();
        let sym_names: Vec<String> = preds
            .iter()
            .filter(|p| p.spec().symmetric)
            .map(|p| p.as_str().to_string())
            .collect();
        let follow_where = match gx.direction.unwrap_or(TraverseDirection::Both) {
            TraverseDirection::Out => {
                "((predicate IN $preds AND subject_kind = 'memory' AND subject_id = $id)                  OR (predicate IN $sym_preds AND object_kind = 'memory' AND object_id = $id))"
            }
            TraverseDirection::In => {
                "((predicate IN $preds AND object_kind = 'memory' AND object_id = $id)                  OR (predicate IN $sym_preds AND subject_kind = 'memory' AND subject_id = $id))"
            }
            TraverseDirection::Both => {
                "(predicate IN $preds AND (                    (subject_kind = 'memory' AND subject_id = $id)                     OR (object_kind = 'memory' AND object_id = $id)))"
            }
        };
        let scope_clause = if matches!(query.scope_match, ScopeMatch::Exact) {
            "scope = $scope"
        } else {
            "(scope = $scope OR string::starts_with(scope, $scope_prefix))"
        };
        let edge_sql = format!(
            "SELECT id, subject_kind, subject_id, object_kind, object_id, predicate              FROM relationship              WHERE {scope_clause} AND state = 'active' AND predicate IN $preds                AND valid_from <= type::datetime($as_of)                AND (valid_until = NONE OR valid_until > type::datetime($as_of))                AND {follow_where}              ORDER BY id ASC LIMIT $edge_limit;"
        );

        struct Node {
            key: String,
            depth: u32,
            seed: String,
            preds: Vec<String>,
        }
        #[derive(Default)]
        struct Prov {
            from: Vec<String>,
            depth: u32,
            preds: Vec<String>,
        }
        let merge_prov = |prov: &mut Prov, seed: &str, depth: u32, pred: &str| {
            prov.depth = prov.depth.max(depth);
            if prov.from.len() < 3 && !prov.from.iter().any(|s| s == seed) {
                prov.from.push(seed.to_string());
            }
            if prov.preds.len() < 3 && !prov.preds.iter().any(|p| p == pred) {
                prov.preds.push(pred.to_string());
            }
        };

        // Bare-key → direct-hit index for provenance merge onto direct hits.
        let mut hit_idx: HashMap<String, usize> = HashMap::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<Node> = VecDeque::new();
        for (i, h) in hits.iter().enumerate() {
            let key = h.id.bare_key().to_string();
            hit_idx.insert(key.clone(), i);
            visited.insert(key.clone());
            if i < max_seeds as usize {
                queue.push_back(Node {
                    key: key.clone(),
                    depth: 0,
                    seed: key,
                    preds: Vec::new(),
                });
            }
        }
        let seeds_used = queue.len() as u32;

        // Discovery order is deterministic (edges scanned ORDER BY id ASC).
        let mut found_order: Vec<String> = Vec::new();
        let mut found: HashMap<String, Prov> = HashMap::new();
        let mut edges_scanned: u32 = 0;
        let mut truncated: Vec<String> = Vec::new();
        let mark = |s: &str, t: &mut Vec<String>| {
            if !t.iter().any(|x| x == s) {
                t.push(s.to_string());
            }
        };

        'bfs: while let Some(node) = queue.pop_front() {
            if node.depth >= max_depth {
                // Bounded depth-boundary probe (≤3 nodes): does the frontier
                // still have followable edges we refused to take?
                let mut probe = self
                    .db
                    .query(edge_sql.as_str())
                    .bind(("scope", scope.as_str().to_string()))
                    .bind(("scope_prefix", format!("{}/", scope.as_str())))
                    .bind(("preds", pred_names.clone()))
                    .bind(("sym_preds", sym_names.clone()))
                    .bind(("as_of", as_of_s.to_string()))
                    .bind(("id", node.key.clone()))
                    .bind(("edge_limit", 1i64))
                    .await
                    .map_err(|e| Error::store(format!("expand depth probe: {e}")))?;
                ensure_ok(&mut probe)?;
                if !take_json_rows(&mut probe, 0)?.is_empty() {
                    mark("depth", &mut truncated);
                }
                continue;
            }
            if Instant::now() > deadline {
                mark("deadline", &mut truncated);
                break;
            }
            let remaining = GRAPH_EXPAND_MAX_EDGES.saturating_sub(edges_scanned);
            if remaining == 0 {
                mark("edges", &mut truncated);
                break;
            }
            let mut response = self
                .db
                .query(edge_sql.as_str())
                .bind(("scope", scope.as_str().to_string()))
                .bind(("scope_prefix", format!("{}/", scope.as_str())))
                .bind(("preds", pred_names.clone()))
                .bind(("sym_preds", sym_names.clone()))
                .bind(("as_of", as_of_s.to_string()))
                .bind(("id", node.key.clone()))
                .bind(("edge_limit", remaining as i64 + 1))
                .await
                .map_err(|e| Error::store(format!("expand edge scan: {e}")))?;
            ensure_ok(&mut response)?;
            let rows = take_json_rows(&mut response, 0)?;
            if rows.len() as u32 > remaining {
                mark("edges", &mut truncated);
            }
            for row in rows.iter().take(remaining as usize) {
                edges_scanned += 1;
                let sk = row["subject_kind"].as_str().unwrap_or("");
                let sid = row["subject_id"].as_str().unwrap_or("");
                let ok_ = row["object_kind"].as_str().unwrap_or("");
                let oid = row["object_id"].as_str().unwrap_or("");
                let pred = row["predicate"].as_str().unwrap_or("").to_string();
                let (okind, okey) = if sk == "memory" && sid == node.key {
                    (ok_, oid)
                } else {
                    (sk, sid)
                };
                if okind != "memory" || okey.is_empty() || okey == node.key {
                    continue;
                }
                if let Some(&i) = hit_idx.get(okey) {
                    // Discovery path merges onto the direct hit (spec 05:5).
                    let h = &mut hits[i];
                    let info =
                        h.expansion
                            .get_or_insert_with(|| nomiso_core::types::ExpansionInfo {
                                from: Vec::new(),
                                depth: u32::MAX,
                                via_predicates: Vec::new(),
                            });
                    info.depth = info.depth.min(node.depth + 1);
                    let sid_mid = format!("memory:{}", node.seed);
                    if info.from.len() < 3 && !info.from.iter().any(|m| m.as_str() == sid_mid) {
                        info.from.push(MemoryId(sid_mid));
                    }
                    if info.via_predicates.len() < 3 && !info.via_predicates.contains(&pred) {
                        info.via_predicates.push(pred.clone());
                    }
                    continue;
                }
                match found.get_mut(okey) {
                    Some(prov) => merge_prov(prov, &node.seed, node.depth + 1, &pred),
                    None => {
                        if found.len() >= max_candidates as usize {
                            mark("candidates", &mut truncated);
                            break 'bfs;
                        }
                        visited.insert(okey.to_string());
                        found_order.push(okey.to_string());
                        found.insert(
                            okey.to_string(),
                            Prov {
                                from: vec![node.seed.clone()],
                                depth: node.depth + 1,
                                preds: vec![pred.clone()],
                            },
                        );
                        let mut path = node.preds.clone();
                        if path.len() < 3 && !path.contains(&pred) {
                            path.push(pred.clone());
                        }
                        queue.push_back(Node {
                            key: okey.to_string(),
                            depth: node.depth + 1,
                            seed: node.seed.clone(),
                            preds: path,
                        });
                    }
                }
            }
        }

        let candidates_found = found.len() as u32;
        let mut candidates_added: u32 = 0;
        if !found.is_empty() {
            // Rank expansion candidates deterministically: shallower first;
            // stable sort preserves BFS discovery order within a depth.
            let mut keys = found_order;
            keys.sort_by_key(|k| found[k].depth);

            let fetch_sql = format!(
                "SELECT * FROM memory                  WHERE record::id(id) IN $keys                    AND {scope_clause}                    AND valid_from <= type::datetime($as_of)                    AND (valid_until = NONE OR valid_until > type::datetime($as_of))                    AND ($cats = NONE OR category IN $cats)                    AND ($known_as_of = NONE OR known_at <= type::datetime($known_as_of))                    AND ($sys_as_of = NONE OR (                        (IF sys_created = NONE THEN known_at ELSE sys_created END) <= type::datetime($sys_as_of)                        AND (sys_closed = NONE OR sys_closed > type::datetime($sys_as_of))                    ));"
            );
            let mut response = self
                .db
                .query(fetch_sql)
                .bind(("keys", keys.clone()))
                .bind(("scope", scope.as_str().to_string()))
                .bind(("scope_prefix", format!("{}/", scope.as_str())))
                .bind(("as_of", as_of_s.to_string()))
                .bind(("cats", categories.cloned()))
                .bind(("known_as_of", known_as_of.map(|s| s.to_string())))
                .bind(("sys_as_of", sys_as_of.map(|s| s.to_string())))
                .await
                .map_err(|e| Error::store(format!("expand fetch: {e}")))?;
            ensure_ok(&mut response)?;
            let rows = take_json_rows(&mut response, 0)?;
            let mut by_key: HashMap<String, Value> = HashMap::new();
            for row in rows {
                if let Ok(id_s) = record_id_to_string(&row["id"]) {
                    by_key.insert(MemoryId::new(id_s).bare_key().to_string(), row);
                }
            }
            for key in keys {
                let Some(row) = by_key.get(&key) else {
                    continue;
                };
                let mrow = match value_to_memory_row(row.clone()) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                let rec = match row_to_record(mrow) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                if !Self::scope_allowed(&rec.scope, scope, query.scope_match) {
                    continue;
                }
                let prov = &found[&key];
                let rank = hits.len();
                let score = 1.0 / (60.0 + rank as f64 + 1.0);
                hits.push(SearchHit {
                    id: rec.id,
                    score,
                    score_kind: ScoreKind::RankFallback,
                    signals: SearchSignals {
                        vector: false,
                        bm25: false,
                        graph: true,
                        expanded: true,
                    },
                    preview: rec.content.text,
                    category: rec.category,
                    scope: rec.scope,
                    valid_from: rec.valid_from,
                    valid_until: rec.valid_until,
                    provenance: rec.provenance,
                    version: rec.version,
                    entities: rec.entity_links,
                    embedding_generation: rec.embedding_generation,
                    expansion: Some(nomiso_core::types::ExpansionInfo {
                        from: prov
                            .from
                            .iter()
                            .map(|s| MemoryId(format!("memory:{s}")))
                            .collect(),
                        depth: prov.depth,
                        via_predicates: prov.preds.clone(),
                    }),
                });
                candidates_added += 1;
            }
        }

        Ok(nomiso_core::ops::ExpansionStats {
            seeds: seeds_used,
            edges_scanned,
            candidates_found,
            candidates_added,
            truncated,
            elapsed_ms: started.elapsed().as_millis() as u64,
        })
    }

    /// Page through `list` with a page cap; `count` calls this with 10_000.
    async fn count_with_page_limit(
        &self,
        req: nomiso_core::list::CountRequest,
        max_pages: usize,
    ) -> Result<u64> {
        let scope = ScopePath::parse(&req.scope)?;
        let scope = scope.as_str().to_string();
        let page_size = self.config.limits.max_search_limit.max(1);
        // Pin the valid-time lens once so every page shares the same cursor context.
        let as_of = Some(req.as_of.unwrap_or_else(now));
        let mut total = 0u64;
        let mut cursor = None;
        for _ in 0..max_pages {
            let sent = cursor.clone();
            let page = self
                .list(nomiso_core::list::ListRequest {
                    scope: scope.clone(),
                    scope_match: req.scope_match,
                    categories: req.categories.clone(),
                    as_of,
                    known_as_of: req.known_as_of,
                    sys_as_of: req.sys_as_of,
                    text: req.text.clone(),
                    limit: Some(page_size),
                    cursor,
                })
                .await?;
            total = total
                .checked_add(page.items.len() as u64)
                .ok_or_else(|| Error::internal("count overflow"))?;
            match page.next_cursor {
                None => return Ok(total),
                Some(c) if page.items.is_empty() || sent.as_ref() == Some(&c) => {
                    return Err(Error::internal("count pagination did not advance"));
                }
                Some(c) => cursor = Some(c),
            }
        }
        Err(Error::PayloadTooLarge(
            "count exceeded the pagination safety limit".into(),
        ))
    }

    async fn find_by_idempotency(
        &self,
        scope: &str,
        key: &str,
    ) -> Result<Option<nomiso_core::types::MemoryRecord>> {
        let sql = r#"
            SELECT * FROM memory
            WHERE scope = $scope AND idempotency_key = $key
            LIMIT 1;
        "#;
        let mut response = self
            .db
            .query(sql)
            .bind(("scope", scope.to_string()))
            .bind(("key", key.to_string()))
            .await
            .map_err(|e| Error::store(e.to_string()))?;
        ensure_ok(&mut response)?;
        let rows = take_json_rows(&mut response, 0)?;
        if let Some(v) = rows.into_iter().next() {
            Ok(Some(row_to_record(value_to_memory_row(v)?)?))
        } else {
            Ok(None)
        }
    }

    async fn put_idempotent(
        &self,
        original: &PutRequest,
        req: PutRequest,
        key: String,
    ) -> Result<WriteResult> {
        let request_identity = put_request_identity(original)?;
        let mem_key = Uuid::now_v7().to_string();
        self.put_idempotent_inner(
            original,
            req,
            key,
            KeyedTxn {
                request_identity,
                mem_key,
                extra_sql: "",
                extra_binds: &[],
            },
        )
        .await
    }

    /// Keyed put with caller-provided request identity, record key, and extra
    /// in-transaction fragments (atomic job inserts — JOB-001). The slot
    /// stores `request_identity` so replays compare the full declared intent.
    async fn put_idempotent_inner(
        &self,
        original: &PutRequest,
        req: PutRequest,
        key: String,
        extras: KeyedTxn<'_>,
    ) -> Result<WriteResult> {
        let KeyedTxn {
            request_identity,
            mem_key,
            extra_sql,
            extra_binds,
        } = extras;
        let event_id = Uuid::now_v7().to_string();
        let slot_id = Self::idempotency_slot_id(&req.scope, &key);
        // MIG-004: resolve the vector's generation before claiming the slot.
        let egen: Option<i64> = if req.embedding.is_some() {
            self.resolve_embedding_for_write(req.embedding_identity.as_ref())
                .await?
        } else {
            None
        };
        let valid_from_ts = req.valid_from.unwrap_or_else(now);
        validate_interval_after_defaults(valid_from_ts, req.valid_until)?;
        let valid_from = ts_param(valid_from_ts);
        let known_at = ts_param(req.known_at.unwrap_or_else(now));
        let valid_until = req.valid_until.map(ts_param);
        let mut content = json!({ "text": req.content.text });
        if let Some(attrs) = &req.content.attrs {
            content["attrs"] = attrs.clone();
        }
        let sql = r#"
            BEGIN TRANSACTION;
            LET $slot = type::record('idempotency_slot', $slot_id);
            LET $ex = (SELECT id FROM $slot);
            IF array::len($ex) > 0 {
                THROW 'nomiso_conflict';
            };
            CREATE $slot SET scope = $scope, k = $k, memory_key = $mem_key, request_identity = $request_identity;
            LET $created = (CREATE type::record('memory', $mem_key) SET
                category = $category,
                scope = $scope,
                content = $content,
                valid_from = type::datetime($valid_from),
                known_at = type::datetime($known_at),
                valid_until = IF $valid_until = NONE THEN NONE ELSE type::datetime($valid_until) END,
                confidence = $confidence,
                provenance = $provenance,
                version = 1,
                entity_links = $entity_links,
                embedding = $embedding,
                embedding_generation = $egen,
                extractor_version = $extractor_version,
                model_version = $model_version,
                idempotency_key = $k,
                valid_rev_from = $valid_rev_from,
                valid_rev_until = $valid_rev_until,
                sys_created = time::now(),
                sys_updated = time::now()
            RETURN AFTER);
            UPDATE $slot SET receipt = $created[0];
            CREATE type::record('belief_event', $eid) SET
                scope = $scope,
                memory_id = $mid,
                kind = 'assert',
                at_sys = time::now(),
                payload = $event_payload;
            RETURN $created;
            COMMIT TRANSACTION;
        "#;
        let sql = if extra_sql.is_empty() {
            sql.to_string()
        } else {
            sql.replace(
                "RETURN $created;\n            COMMIT TRANSACTION;",
                &format!(
                    "{extra_sql}\n            RETURN $created;\n            COMMIT TRANSACTION;"
                ),
            )
        };
        let sql = sql.as_str();
        let mut last_err: Option<Error> = None;
        for _ in 0..3 {
            let q = self
                .db
                .query(sql)
                .bind(("slot_id", slot_id.clone()))
                .bind(("mem_key", mem_key.clone()))
                .bind(("eid", event_id.clone()))
                .bind(("mid", format!("memory:{mem_key}")))
                .bind(("event_payload", json!({ "version": 1, "key": key })))
                .bind(("scope", req.scope.clone()))
                .bind(("k", key.clone()))
                .bind(("request_identity", request_identity.clone()))
                .bind(("category", req.category.as_str().to_string()))
                .bind(("content", content.clone()))
                .bind(("valid_from", valid_from.clone()))
                .bind(("known_at", known_at.clone()))
                .bind(("valid_until", valid_until.clone()))
                .bind(("confidence", req.confidence))
                .bind(("provenance", json!(req.provenance)))
                .bind(("entity_links", req.entity_links.clone()))
                .bind(("embedding", req.embedding.clone()))
                .bind(("egen", egen))
                .bind(("extractor_version", req.extractor_version.clone()))
                .bind(("model_version", req.model_version.clone()))
                .bind(("valid_rev_from", req.valid_rev_from.clone()))
                .bind(("valid_rev_until", req.valid_rev_until.clone()));
            let mut qq = q;
            for (k, v) in extra_binds {
                qq = qq.bind((k.clone(), v.clone()));
            }
            let mut response = match qq.await {
                Ok(r) => r,
                Err(e) => {
                    let err = Error::store(e.to_string());
                    if is_kv_write_conflict(&err) || is_conflict_store_err(&err) {
                        last_err = Some(err);
                        continue;
                    }
                    return Err(err);
                }
            };
            match ensure_ok(&mut response).map_err(job_txn_error) {
                Ok(()) => {
                    let mut receipt = self
                        .lookup_put_receipt_expect(original, &request_identity)
                        .await?
                        .ok_or_else(|| {
                            Error::store(
                                "committed put receipt unavailable; reconcile using the same key",
                            )
                        })?;
                    receipt.replayed = false;
                    return Ok(receipt);
                }
                Err(e) if is_kv_write_conflict(&e) || is_conflict_store_err(&e) => {
                    last_err = Some(e);
                    if let Some(receipt) = self
                        .lookup_put_receipt_expect(original, &request_identity)
                        .await?
                    {
                        return Ok(receipt);
                    }
                }
                Err(e) => return Err(e),
            }
        }
        if let Some(receipt) = self
            .lookup_put_receipt_expect(original, &request_identity)
            .await?
        {
            return Ok(receipt);
        }
        Err(last_err.unwrap_or_else(|| Error::store("idempotent put failed")))
    }

    /// Resolve an endpoint's `(scope, version)`; `None` when absent.
    /// Artifact/span rows carry no version → `version` is `None`.
    async fn endpoint_scope_version(
        &self,
        kind: nomiso_core::relationship::EndpointKind,
        key: &str,
    ) -> Result<Option<(String, Option<u64>)>> {
        let table = kind.as_str();
        let mut response = self
            .db
            .query("SELECT scope, version FROM type::record($table, $key);")
            .bind(("table", table.to_string()))
            .bind(("key", key.to_string()))
            .await
            .map_err(|e| Error::store(format!("endpoint {table}:{key}: {e}")))?;
        ensure_ok(&mut response)?;
        let rows = take_json_rows(&mut response, 0)?;
        Ok(rows.into_iter().next().map(|row| {
            let scope = row
                .get("scope")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let version = row.get("version").and_then(|v| v.as_u64());
            (scope, version)
        }))
    }

    /// Validate one endpoint for `put_relationship`/traversal seeds: the record
    /// must exist and carry the exact owning scope (REL-001).
    async fn check_endpoint(
        &self,
        kind: nomiso_core::relationship::EndpointKind,
        key: &str,
        scope: &str,
    ) -> Result<Option<u64>> {
        let Some((ep_scope, version)) = self.endpoint_scope_version(kind, key).await? else {
            return Err(Error::NotFound(format!("{}:{}", kind.as_str(), key)));
        };
        if ep_scope != scope {
            return Err(Error::ScopeDenied(format!(
                "endpoint {}:{key} is not in scope {scope}",
                kind.as_str()
            )));
        }
        Ok(version)
    }

    /// Fetch the single edge holding an active `live_key` (dedupe identity).
    async fn relationship_by_live_key(
        &self,
        live_key: &str,
    ) -> Result<Option<nomiso_core::relationship::RelationshipRecord>> {
        let mut response = self
            .db
            .query("SELECT * FROM relationship WHERE live_key = $lk LIMIT 1;")
            .bind(("lk", live_key.to_string()))
            .await
            .map_err(|e| Error::store(format!("relationship dedupe read: {e}")))?;
        ensure_ok(&mut response)?;
        let rows = take_json_rows(&mut response, 0)?;
        rows.into_iter()
            .next()
            .map(|v| decode_relationship(&v))
            .transpose()
    }

    /// Fetch a job row by bare key regardless of scope (scope checks are the
    /// caller's job — public methods pass through `get_job`-style filters).
    async fn job_by_key(&self, id: &str) -> Result<JobRecord> {
        let key = job_bare_key(id)?;
        let mut response = self
            .db
            .query(
                "SELECT * FROM nomiso_job
                 WHERE <string> record::id(id) = $id LIMIT 1;",
            )
            .bind(("id", key.clone()))
            .await
            .map_err(|e| Error::store(format!("get job: {e}")))?;
        ensure_ok(&mut response)?;
        let rows = take_json_rows(&mut response, 0)?;
        rows.into_iter()
            .next()
            .map(|v| decode_job(&v))
            .transpose()?
            .ok_or_else(|| Error::NotFound(format!("job {key}")))
    }

    /// Core supersede transaction. `intents` are durable job intents
    /// committed atomically with the close+successor (JOB-001); `self_input`
    /// pins the successor at revision 1. Returns the write plus one outcome
    /// slot per intent (deduplicated vs pending in-transaction insert).
    async fn supersede_inner(
        &self,
        req: SupersedeRequest,
        intents: &[JobIntent],
    ) -> Result<(WriteResult, Vec<Option<PendingJob>>)> {
        let mut req = req;
        let scope = validate_supersede(&req, &self.config.limits)?;
        req.new.scope = scope.as_str().to_string();
        let prior_key = req.prior_id.bare_key().to_string();

        let prior_row = self
            .fetch_memory_raw(&req.prior_id)
            .await?
            .ok_or_else(|| Error::NotFound(req.prior_id.to_string()))?;
        // Early TOCTOU pre-check (authoritative guard is SQL WHERE inside the txn).
        check_version(req.expected_version, prior_row.version)?;

        if prior_row.scope != req.new.scope {
            return Err(Error::ScopeDenied(format!(
                "supersede scope '{}' does not match prior scope '{}'",
                req.new.scope, prior_row.scope
            )));
        }

        // Reject re-supersede of an already-closed chain (superseded_by set).
        if !matches!(prior_row.superseded_by.as_ref(), None | Some(Value::Null)) {
            return Err(Error::Conflict {
                expected: req.expected_version,
                found: prior_row.version,
            });
        }

        let prior_valid_from = crate::mapping::parse_timestamp(&prior_row.valid_from)?;
        // One instant for close + successor valid_from when callers omit both.
        let instant = now();
        let close_at = req.close_at.or(req.new.valid_from).unwrap_or(instant);
        if close_at < prior_valid_from {
            return Err(Error::invalid(format!(
                "close_at ({close_at}) is before prior valid_from ({prior_valid_from})"
            )));
        }
        if let Some(Value::Null) | None = prior_row.valid_until.as_ref() {
            // open interval — ok
        } else if let Some(ref vu) = prior_row.valid_until {
            if let Ok(until) = crate::mapping::parse_timestamp(vu) {
                if close_at > until {
                    return Err(Error::invalid(format!(
                        "close_at ({close_at}) is after prior valid_until ({until})"
                    )));
                }
            }
        }

        // Single ACID transaction: conditional close + create successor + link.
        // Zero-row close → typed Conflict (checked version guard).
        let new_key = Uuid::now_v7().to_string();
        let (job_sql, job_binds, slots) = self
            .resolve_job_intents(intents, scope.as_str(), &new_key)
            .await?;
        let mut new_req = req.new.clone();
        if new_req.valid_from.is_none() {
            new_req.valid_from = Some(close_at);
        }
        if new_req.known_at.is_none() {
            new_req.known_at = Some(instant);
        }
        let valid_from_ts = new_req.valid_from.unwrap_or(close_at);
        validate_interval_after_defaults(valid_from_ts, new_req.valid_until)?;
        let valid_from = ts_param(valid_from_ts);
        let known_at = ts_param(new_req.known_at.unwrap_or(instant));
        let valid_until = new_req.valid_until.map(ts_param);
        let mut content = json!({ "text": new_req.content.text });
        if let Some(attrs) = &new_req.content.attrs {
            content["attrs"] = attrs.clone();
        }
        // MIG-004: the successor's vector is stamped with its generation.
        let egen: Option<i64> = if new_req.embedding.is_some() {
            self.resolve_embedding_for_write(new_req.embedding_identity.as_ref())
                .await?
        } else {
            None
        };

        let sql = r#"
            BEGIN TRANSACTION;
            LET $prior = type::record('memory', $prior_key);
            LET $new = type::record('memory', $new_key);
            LET $closed = (
                UPDATE $prior SET
                    valid_until = type::datetime($close_at),
                    sys_closed = time::now(),
                    version = version + 1
                WHERE version = $expected_version
                  AND superseded_by = NONE
                  AND valid_until = NONE
                  AND sys_closed = NONE
                RETURN AFTER
            );
            IF array::len($closed) = 0 {
                THROW 'nomiso_conflict';
            };
            LET $created = (
                CREATE $new SET
                    category = $category,
                    scope = $scope,
                    content = $content,
                    valid_from = type::datetime($valid_from),
                    known_at = type::datetime($known_at),
                    valid_until = IF $valid_until = NONE THEN NONE ELSE type::datetime($valid_until) END,
                    confidence = $confidence,
                    provenance = $provenance,
                    version = 1,
                    entity_links = $entity_links,
                    embedding = $embedding,
                    embedding_generation = $egen,
                    supersedes = $prior,
                    extractor_version = $extractor_version,
                    model_version = $model_version,
                    idempotency_key = $idempotency_key,
                    valid_rev_from = $valid_rev_from,
                    valid_rev_until = $valid_rev_until,
                    sys_created = time::now(),
                    sys_updated = time::now()
                RETURN AFTER
            );
            UPDATE $prior SET superseded_by = $new;
            RELATE $new -> supersedes_edge -> $prior;
            // REL-004: dependents of the closed prior are marked stale in the
            // same commit, not via a losable post-commit callback.
            UPDATE memory SET stale = true
            WHERE scope = $scope AND id IN (
                SELECT VALUE in FROM derived_from_edge WHERE out = $prior
            );
            // REL-004: typed derived_from edges touching the closed prior are
            // marked stale in the same commit (both as source and as view).
            UPDATE relationship SET
                state = 'stale',
                state_reason = 'endpoint_superseded',
                live_key = 'X|' + <string> id,
                updated_at = time::now()
            WHERE scope = $scope AND state = 'active' AND predicate = 'derived_from'
              AND ((subject_kind = 'memory' AND subject_id = $prior_key)
                OR (object_kind = 'memory' AND object_id = $prior_key));
            CREATE type::record('belief_event', $close_eid) SET
                scope = $scope,
                memory_id = $prior_mid,
                kind = 'close',
                at_sys = time::now(),
                payload = $close_payload;
            CREATE type::record('belief_event', $assert_eid) SET
                scope = $scope,
                memory_id = $new_mid,
                kind = 'assert',
                at_sys = time::now(),
                payload = $assert_payload;
            RETURN $created;
            COMMIT TRANSACTION;
        "#;

        let sql = sql.replace(
            "RETURN $created;\n            COMMIT TRANSACTION;",
            &format!("{job_sql}            RETURN $created;\n            COMMIT TRANSACTION;"),
        );
        let close_eid = Uuid::now_v7().to_string();
        let assert_eid = Uuid::now_v7().to_string();
        let close_payload = json!({
            "via": "supersede",
            "successor": format!("memory:{new_key}"),
            "closed_version": req.expected_version + 1,
        });
        let assert_payload = json!({
            "via": "supersede",
            "prior": req.prior_id.to_string(),
            "version": 1,
        });
        let mut response = None;
        for attempt in 0..3 {
            let q = self
                .db
                .query(sql.as_str())
                .bind(("prior_key", prior_key.clone()))
                .bind(("new_key", new_key.clone()))
                .bind(("close_eid", close_eid.clone()))
                .bind(("assert_eid", assert_eid.clone()))
                .bind(("prior_mid", format!("memory:{prior_key}")))
                .bind(("new_mid", format!("memory:{new_key}")))
                .bind(("close_payload", close_payload.clone()))
                .bind(("assert_payload", assert_payload.clone()))
                .bind(("close_at", ts_param(close_at)))
                .bind(("expected_version", req.expected_version as i64))
                .bind(("category", new_req.category.as_str().to_string()))
                .bind(("scope", new_req.scope.clone()))
                .bind(("content", content.clone()))
                .bind(("valid_from", valid_from.clone()))
                .bind(("known_at", known_at.clone()))
                .bind(("valid_until", valid_until.clone()))
                .bind(("confidence", new_req.confidence))
                .bind(("provenance", json!(new_req.provenance)))
                .bind(("entity_links", new_req.entity_links.clone()))
                .bind(("embedding", new_req.embedding.clone()))
                .bind(("egen", egen))
                .bind(("extractor_version", new_req.extractor_version.clone()))
                .bind(("model_version", new_req.model_version.clone()))
                .bind(("idempotency_key", new_req.idempotency_key.clone()))
                .bind(("valid_rev_from", new_req.valid_rev_from.clone()))
                .bind(("valid_rev_until", new_req.valid_rev_until.clone()));
            let mut q = q;
            for (k, v) in &job_binds {
                q = q.bind((k.clone(), v.clone()));
            }
            let mut r = q
                .await
                .map_err(|e| Error::store(format!("supersede txn: {e}")))?;
            match ensure_ok(&mut r) {
                Ok(()) => {
                    response = Some(r);
                    break;
                }
                // Job markers must be read before the conflict matchers: an
                // in-transaction THROW aborts every other statement with
                // -32003 noise, so the aggregate always resembles a KV
                // conflict. A dedup-slot violation likewise surfaces inside
                // the noise — propagate it raw so the outer retry can dedup.
                Err(e) if err_text_lower(&e).contains("nomiso_bad_input") => {
                    return Err(job_txn_error(e));
                }
                Err(e) if is_state_create_conflict(&e) && !intents.is_empty() => {
                    return Err(e);
                }
                Err(e) if is_kv_write_conflict(&e) && attempt < 2 => {
                    continue;
                }
                Err(e) if is_conflict_store_err(&e) => {
                    return Err(self
                        .conflict_from_memory(&req.prior_id, req.expected_version)
                        .await);
                }
                Err(e) => return Err(e),
            }
        }
        let Some(mut response) = response else {
            return Err(self
                .conflict_from_memory(&req.prior_id, req.expected_version)
                .await);
        };

        // Collect MemoryRow-shaped results; prefer the successor (has supersedes).
        let mut candidates: Vec<nomiso_core::types::MemoryRecord> = Vec::new();
        let n = response.num_statements();
        for i in 0..n {
            let Ok(rows) = take_json_rows(&mut response, i) else {
                continue;
            };
            for v in rows {
                let objs = match v {
                    Value::Array(a) => a,
                    other => vec![other],
                };
                for c in objs {
                    if let Ok(row) = value_to_memory_row(c) {
                        if let Ok(rec) = row_to_record(row) {
                            candidates.push(rec);
                        }
                    }
                }
            }
        }
        let record = candidates
            .into_iter()
            .rev()
            .find(|r| r.supersedes.is_some())
            .ok_or_else(|| Error::store("supersede transaction returned no created row"))?;

        Ok((
            WriteResult {
                replayed: false,
                id: record.id.clone(),
                version: record.version,
                record: Some(record),
            },
            slots,
        ))
    }

    /// Receipt lookup against a caller-supplied request identity (plain puts
    /// pass `put_request_identity`; write+job composites pass an identity that
    /// also covers the declared intents — JOB-001).
    async fn lookup_put_receipt_expect(
        &self,
        req: &PutRequest,
        expected_identity: &str,
    ) -> Result<Option<WriteResult>> {
        let scope = nomiso_core::validate::validate_put_shape(req, &self.config.limits)?;
        let Some(key) = &req.idempotency_key else {
            return Ok(None);
        };
        let mut response = self
            .db
            .query("SELECT * FROM type::record('idempotency_slot', $slot_id);")
            .bind(("slot_id", Self::idempotency_slot_id(scope.as_str(), key)))
            .await
            .map_err(|e| Error::store(format!("lookup put receipt: {e}")))?;
        ensure_ok(&mut response)?;
        let rows: Vec<Value> = response
            .take(0)
            .map_err(|e| Error::store(format!("decode put receipt: {e}")))?;
        let slot = match rows.into_iter().next() {
            Some(s) => s,
            None => {
                if self
                    .find_by_idempotency(scope.as_str(), key)
                    .await?
                    .is_some()
                {
                    // The keyed write commits slot+memory+receipt atomically;
                    // finding the memory means the slot is committed too — the
                    // first read raced the commit. Re-read before concluding
                    // the receipt is genuinely absent (legacy/erased).
                    let mut again = self
                        .db
                        .query("SELECT * FROM type::record('idempotency_slot', $slot_id);")
                        .bind(("slot_id", Self::idempotency_slot_id(scope.as_str(), key)))
                        .await
                        .map_err(|e| Error::store(format!("re-read put receipt: {e}")))?;
                    ensure_ok(&mut again)?;
                    let rows2: Vec<Value> = again
                        .take(0)
                        .map_err(|e| Error::store(format!("decode put receipt: {e}")))?;
                    match rows2.into_iter().next() {
                        Some(s) => s,
                        None => return Err(Error::IdempotencyUnavailable),
                    }
                } else {
                    return Ok(None);
                }
            }
        };
        if slot.get("scope").and_then(Value::as_str) != Some(scope.as_str())
            || slot.get("k").and_then(Value::as_str) != Some(key.as_str())
        {
            return Err(Error::internal("put receipt scope/key mismatch"));
        }
        let identity = slot
            .get("request_identity")
            .and_then(Value::as_str)
            .ok_or(Error::IdempotencyUnavailable)?;
        if identity != expected_identity {
            return Err(Error::IdempotencyConflict);
        }
        let memory_key = slot
            .get("memory_key")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::internal("put receipt missing memory key"))?;
        let id = MemoryId::new(format!("memory:{memory_key}"));
        let current = self
            .fetch_memory_raw(&id)
            .await?
            .ok_or(Error::IdempotencyUnavailable)?;
        if current.scope != scope.as_str() {
            return Err(Error::internal("put receipt target scope mismatch"));
        }
        let snapshot = slot
            .get("receipt")
            .filter(|v| v.is_object())
            .ok_or(Error::IdempotencyUnavailable)?
            .clone();
        let record = row_to_record(value_to_memory_row(snapshot)?)?;
        if record.id.bare_key() != memory_key || record.scope != scope.as_str() {
            return Err(Error::internal("put receipt snapshot identity mismatch"));
        }
        Ok(Some(WriteResult {
            replayed: true,
            id: record.id.clone(),
            version: record.version,
            record: Some(record),
        }))
    }

    /// Live holder of a job dedup slot, if any.
    async fn find_live_job(&self, scope: &str, dedup_key: &str) -> Result<Option<JobRecord>> {
        let mut response = self
            .db
            .query(
                "SELECT * FROM nomiso_job
                 WHERE dedup_slot = $dk AND scope = $scope LIMIT 1;",
            )
            .bind(("dk", dedup_key.to_string()))
            .bind(("scope", scope.to_string()))
            .await
            .map_err(|e| Error::store(format!("job dedup read: {e}")))?;
        ensure_ok(&mut response)?;
        let rows = take_json_rows(&mut response, 0)?;
        rows.into_iter().next().map(|v| decode_job(&v)).transpose()
    }

    /// Dedup-slot conflict replay: the holder of the live slot is the durable
    /// intent this enqueue matched (JOB-001).
    async fn dedup_replay(&self, scope: &str, dedup_key: &str) -> Result<EnqueueJobResult> {
        match self.find_live_job(scope, dedup_key).await? {
            Some(job) => Ok(EnqueueJobResult {
                job,
                deduplicated: true,
            }),
            // The conflicting row committed between our create and this read,
            // or was tombstoned concurrently — one bounded retry via caller.
            None => Err(Error::Store(
                "job dedup slot conflicted but no live job found; retry enqueue".into(),
            )),
        }
    }

    /// Resolve declared intents for an atomic write+enqueue commit (JOB-001):
    /// validate each resolved request, dedup against live holders, and emit
    /// in-transaction insert fragments for the rest. `self_key` is the record
    /// key the write will create.
    async fn resolve_job_intents(
        &self,
        intents: &[JobIntent],
        scope: &str,
        self_key: &str,
    ) -> Result<(String, TxnBinds, Vec<Option<PendingJob>>)> {
        let mut sql = String::new();
        let mut binds: TxnBinds = Vec::new();
        let mut slots: Vec<Option<PendingJob>> = Vec::with_capacity(intents.len());
        for (i, it) in intents.iter().enumerate() {
            let r = it.resolve(scope, Some(self_key));
            validate_enqueue(&r)?;
            let dedup = job_dedup_key(&r, scope)?;
            if let Some(existing) = self.find_live_job(scope, &dedup).await? {
                slots.push(Some(PendingJob::Deduped(Box::new(EnqueueJobResult {
                    job: existing,
                    deduplicated: true,
                }))));
                continue;
            }
            let expires_at = r.budget.deadline_ms.map(|ms| ts_param(now_plus_ms(ms)));
            let (fsql, fbinds) = job_txn_fragment(i, &r, &dedup, &expires_at)?;
            sql.push_str(&fsql);
            binds.extend(fbinds);
            slots.push(Some(PendingJob::Inserted(dedup)));
        }
        Ok((sql, binds, slots))
    }

    /// Terminal-fail a pending job whose overall deadline passed (JOB-005).
    async fn mark_deadline_expired(&self, job: &JobRecord) -> Result<()> {
        let mut response = self
            .db
            .query(
                "UPDATE type::record('nomiso_job', $id) SET
                    state = 'failed',
                    terminal_reason = 'deadline exceeded',
                    dedup_slot = $tomb,
                    completed_at = time::now(),
                    updated_at = time::now()
                 WHERE state = 'pending' AND fence = $fence;",
            )
            .bind(("id", job_bare_key(&job.id)?))
            .bind(("tomb", format!("X|{}", job_bare_key(&job.id)?)))
            .bind(("fence", job.fence as i64))
            .await
            .map_err(|e| Error::store(format!("deadline sweep: {e}")))?;
        ensure_ok(&mut response)
    }
}

fn put_request_identity(req: &PutRequest) -> Result<String> {
    fn sorted(value: Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut entries: Vec<_> = map.into_iter().collect();
                entries.sort_by(|a, b| a.0.cmp(&b.0));
                Value::Object(entries.into_iter().map(|(k, v)| (k, sorted(v))).collect())
            }
            Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
            value => value,
        }
    }
    let value = serde_json::to_value(req)
        .map_err(|_| Error::invalid("put request cannot be serialized"))?;
    let encoded = serde_json::to_string(&sorted(value))
        .map_err(|_| Error::internal("put identity encoding failed"))?;
    Ok(format!("nomiso.put-request.v0:{encoded}"))
}

/// Parse a `major.minor.patch` schema marker into a comparable tuple.
fn parse_semver3(v: &str) -> Option<(u64, u64, u64)> {
    let mut it = v.trim().split('.');
    let a = it.next()?.parse().ok()?;
    let b = it.next()?.parse().ok()?;
    let c = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    Some((a, b, c))
}

fn normalize_endpoint(endpoint: &str) -> String {
    match endpoint {
        "mem://" | "memory://" | "mem" => "memory".into(),
        other => other.to_string(),
    }
}

fn ensure_ok(response: &mut IndexedResults) -> Result<()> {
    let errors = response.take_errors();
    if !errors.is_empty() {
        return Err(Error::store(format!("query errors: {errors:?}")));
    }
    Ok(())
}

fn err_text_lower(e: &Error) -> String {
    e.to_string().to_lowercase()
}

/// Surreal KV commit collision (untyped unless mapped).
fn is_kv_write_conflict(e: &Error) -> bool {
    let s = err_text_lower(e);
    s.contains("transaction conflict")
        || s.contains("write conflict")
        || s.contains("-32009")
        || s.contains("-32003")
}

fn is_nomiso_throw_conflict(e: &Error) -> bool {
    let s = err_text_lower(e);
    s.contains("nomiso_conflict") || (s.contains("throw") && s.contains("conflict"))
}

fn is_conflict_store_err(e: &Error) -> bool {
    is_nomiso_throw_conflict(e) || is_kv_write_conflict(e)
}

fn is_scope_denied_store_err(e: &Error) -> bool {
    let s = err_text_lower(e);
    s.contains("nomiso_scope_denied")
}

/// Unique-index/record-exists or KV conflict on a create claim.
fn is_state_create_conflict(e: &Error) -> bool {
    let s = err_text_lower(e);
    s.contains("unique")
        || s.contains("already contains")
        || s.contains("already exists")
        || is_kv_write_conflict(e)
}

/// Map engine/SQL conflict text to `Error::Conflict`.
/// `found` is a placeholder; callers that have a store must re-read.
#[allow(dead_code)]
fn map_conflict_store_err(e: Error, expected: u64) -> Error {
    if is_conflict_store_err(&e) {
        return Error::Conflict {
            expected,
            found: expected,
        };
    }
    e
}

fn expand_memory_id_index(ids: &[nomiso_core::types::MemoryId]) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for id in ids {
        for form in nomiso_core::trace::memory_id_index_forms(id) {
            if seen.insert(form.clone()) {
                out.push(form);
            }
        }
    }
    out
}

fn json_opt_string(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| {
        if let Some(s) = x.as_str() {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        } else if x.is_null() {
            None
        } else {
            let s = x.to_string().trim_matches('"').to_string();
            if s.is_empty() || s == "null" {
                None
            } else {
                Some(s)
            }
        }
    })
}

fn event_row_matches_memory(v: &Value, want: &MemoryId) -> bool {
    let want_bare = want.bare_key();
    if want_bare.is_empty() {
        return false;
    }
    if let Some(arr) = v.get("memory_ids").and_then(|x| x.as_array()) {
        for item in arr {
            if let Some(s) = item.as_str() {
                if MemoryId::new(s).bare_key() == want_bare {
                    return true;
                }
            }
        }
    }
    if let Some(p) = v.get("payload") {
        return nomiso_core::trace::memory_ids_from_payload(p)
            .iter()
            .any(|id| id.bare_key() == want_bare);
    }
    false
}

fn parse_trace_outcome_payload(
    payload: Option<&Value>,
) -> (Option<nomiso_core::trace::TraceOutcome>, Option<String>) {
    use nomiso_core::trace::TraceOutcome;
    let Some(p) = payload else {
        return (None, None);
    };
    let outcome = p
        .get("outcome")
        .and_then(|v| v.as_str())
        .and_then(TraceOutcome::parse)
        .or_else(|| p.as_str().and_then(TraceOutcome::parse));
    let note = p
        .get("note")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    (outcome, note)
}

fn take_json_rows(response: &mut IndexedResults, index: usize) -> Result<Vec<Value>> {
    // Prefer array of objects
    if let Ok(rows) = response.take::<Vec<Value>>(index) {
        return Ok(rows);
    }
    // Single object
    if let Ok(Some(v)) = response.take::<Option<Value>>(index) {
        return Ok(vec![v]);
    }
    // Empty / missing
    if let Ok(None) = response.take::<Option<Value>>(index) {
        return Ok(vec![]);
    }
    Ok(vec![])
}

/// Shared scope/temporal/category filter context for graph expansion.
struct ExpandCtx<'a> {
    query: &'a SearchQuery,
    scope: &'a ScopePath,
    as_of_s: &'a str,
    known_as_of: Option<&'a str>,
    sys_as_of: Option<&'a str>,
    categories: Option<&'a Vec<String>>,
    gx: &'a nomiso_core::ops::GraphExpand,
}

fn take_search_hits(
    response: &mut IndexedResults,
    branch_signals: SearchSignals,
) -> Result<Vec<SearchHit>> {
    for i in (0..12).rev() {
        let rows = match take_json_rows(response, i) {
            Ok(r) if !r.is_empty() => r,
            _ => continue,
        };
        let mut hits = Vec::with_capacity(rows.len());
        for (rank, v) in rows.into_iter().enumerate() {
            // Rank-position fallback is always labeled RankFallback when used.
            let rank_fallback = 1.0 / (60.0 + rank as f64 + 1.0);
            if let Ok(srow) = value_to_search_row(v.clone()) {
                hits.push(search_row_to_hit(
                    srow,
                    branch_signals.clone(),
                    rank_fallback,
                )?);
            } else if let Ok(mrow) = value_to_memory_row(v) {
                let rec = row_to_record(mrow)?;
                // Memory-row shape has no engine score keys → labeled fallback only.
                hits.push(SearchHit {
                    id: rec.id,
                    score: rank_fallback,
                    score_kind: ScoreKind::RankFallback,
                    signals: branch_signals.clone(),
                    preview: rec.content.text,
                    category: rec.category,
                    scope: rec.scope,
                    valid_from: rec.valid_from,
                    valid_until: rec.valid_until,
                    provenance: rec.provenance,
                    version: rec.version,
                    entities: rec.entity_links,
                    embedding_generation: rec.embedding_generation,
                    expansion: None,
                });
            }
        }
        if !hits.is_empty() {
            return Ok(hits);
        }
    }
    Ok(vec![])
}

#[async_trait]
impl MemoryStore for SurrealMemoryStore {
    #[instrument(skip(self))]
    async fn migrate(&self) -> Result<()> {
        let opts = MigrateOptions {
            embedding_dim: self.config.embedding_dim,
        };
        let pending = migrations(opts)?;

        // MIG-001: validate store compatibility BEFORE any DDL or marker write.
        if let Some(v) = self.meta_value("schema").await? {
            let stored = v.as_str().unwrap_or_default();
            match (parse_semver3(stored), parse_semver3(SCHEMA_VERSION)) {
                (Some(have), Some(mine)) if have > mine => {
                    return Err(Error::IncompatibleStore(format!(
                        "store schema {stored} is newer than this binary's {SCHEMA_VERSION}"
                    )));
                }
                (None, _) => {
                    return Err(Error::IncompatibleStore(format!(
                        "store schema marker '{stored}' is not a recognized version"
                    )));
                }
                _ => {}
            }
        }
        if let Some(v) = self.meta_value("embedding_dim").await? {
            let prev = v
                .as_u64()
                .or_else(|| v.as_i64().map(|i| i as u64))
                .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()));
            if let Some(prev) = prev {
                if prev as usize != self.config.embedding_dim {
                    return Err(Error::invalid(format!(
                        "embedding_dim mismatch: store has {prev}, config has {}",
                        self.config.embedding_dim
                    )));
                }
            }
        }

        // The ledger table itself must exist before migrations can be tracked.
        // Its DDL is a fixed idempotent bootstrap step (not self-ledgered).
        let ledger = pending
            .iter()
            .find(|m| m.name.starts_with("009"))
            .ok_or_else(|| Error::internal("009_schema_ledger migration missing"))?;
        let mut boot = self
            .db
            .query(ledger.source.clone())
            .await
            .map_err(|e| Error::store(format!("migrate ledger bootstrap: {e}")))?;
        ensure_ok(&mut boot).map_err(|e| Error::store(format!("migrate ledger bootstrap: {e}")))?;

        // Apply each migration under a ledger claim; record completion only
        // after its DDL succeeds so an interrupted run stays resumable.
        for m in &pending {
            let checksum = blake3::hash(m.source.as_bytes()).to_hex().to_string();
            if !self.claim_migration(&m.name, &checksum).await? {
                continue;
            }
            debug!(migration = %m.name, "applying nomiso migration");
            let mut response = self
                .db
                .query(m.source.clone())
                .await
                .map_err(|e| Error::store(format!("migrate {}: {e}", m.name)))?;
            let errors = response.take_errors();
            let serious: Vec<_> = errors
                .iter()
                .filter(|(_, e)| {
                    let s = e.to_string().to_lowercase();
                    !s.contains("already exists") && !s.contains("if not exists")
                })
                .collect();
            if !serious.is_empty() {
                let msg = format!("migrate {}: {serious:?}", m.name);
                let mut fail = self
                    .db
                    .query(
                        r#"
                        UPDATE type::record('schema_migration', $n)
                        SET status = 'failed', error = $e;
                        "#,
                    )
                    .bind(("n", m.name.clone()))
                    .bind(("e", msg.clone()))
                    .await
                    .map_err(|e| Error::store(format!("ledger fail-mark: {e}")))?;
                let _ = ensure_ok(&mut fail);
                return Err(Error::store(msg));
            }
            let mut done = self
                .db
                .query(
                    r#"
                    UPDATE type::record('schema_migration', $n)
                    SET status = 'applied', applied_at = time::now(), checksum = $c, error = NONE
                    WHERE status = 'applying';
                    "#,
                )
                .bind(("n", m.name.clone()))
                .bind(("c", checksum.clone()))
                .await
                .map_err(|e| Error::store(format!("ledger complete {}: {e}", m.name)))?;
            ensure_ok(&mut done)?;
        }

        // MIG-004/005: bootstrap generation 1 on first open, and fail closed
        // when the declared identity is incompatible with the active
        // generation. Runs after DDL (the table may not exist on older
        // stores) but before the marker advances.
        self.ensure_embedding_generation().await?;

        // Advance the markers only after every migration row is 'applied'.
        let mut meta = self
            .db
            .query(
                r#"
                UPSERT nomiso_meta:schema SET name = 'schema_version', value = $v;
                UPSERT nomiso_meta:embedding_dim SET name = 'embedding_dim', value = $dim;
                "#,
            )
            .bind(("v", SCHEMA_VERSION.to_string()))
            .bind(("dim", self.config.embedding_dim as i64))
            .await
            .map_err(|e| Error::store(format!("migrate meta: {e}")))?;
        ensure_ok(&mut meta)?;
        Ok(())
    }

    fn limits(&self) -> nomiso_core::validate::Limits {
        self.config.limits
    }

    #[instrument(skip(self, req))]
    async fn put(&self, req: PutRequest) -> Result<WriteResult> {
        self.put_prepared(req, None).await
    }

    async fn lookup_put_receipt(&self, req: &PutRequest) -> Result<Option<WriteResult>> {
        let identity = put_request_identity(req)?;
        self.lookup_put_receipt_expect(req, &identity).await
    }

    async fn put_prepared(
        &self,
        mut req: PutRequest,
        generated_embedding: Option<Vec<f32>>,
    ) -> Result<WriteResult> {
        let scope = nomiso_core::validate::validate_put_shape(&req, &self.config.limits)?;
        req.scope = scope.as_str().to_string();
        if req.embedding.is_some() && generated_embedding.is_some() {
            return Err(Error::invalid(
                "cannot supply both original and generated embeddings",
            ));
        }
        if let Some(receipt) = self.lookup_put_receipt(&req).await? {
            return Ok(receipt);
        }
        validate_put(&req, &self.config.limits)?;
        let original = req.clone();
        if generated_embedding.is_some() {
            req.embedding = generated_embedding;
            validate_put(&req, &self.config.limits)?;
        }
        // Idempotency: claim sidecar slot + create in one txn (UNIQUE without NONE).
        if let Some(key) = req.idempotency_key.clone() {
            return self.put_idempotent(&original, req, key).await;
        }
        let record = self.create_memory(&req, None).await?;
        Ok(WriteResult {
            replayed: false,
            id: record.id.clone(),
            version: record.version,
            record: Some(record),
        })
    }

    #[instrument(skip(self, req))]
    async fn supersede(&self, req: SupersedeRequest) -> Result<WriteResult> {
        Ok(self.supersede_inner(req, &[]).await?.0)
    }

    async fn lookup_put_receipt_with_intents(
        &self,
        req: &PutRequest,
        intents: &[JobIntent],
    ) -> Result<Option<WriteWithJobsResult>> {
        if req.idempotency_key.is_none() {
            return Ok(None);
        }
        let scope = nomiso_core::validate::validate_put_shape(req, &self.config.limits)?;
        let intents_hash = intents_identity(intents)?;
        // The slot identity covers the put AND the declared intents —
        // identical intents replay; different intents conflict (JOB-001).
        // On replay the self-input resolves to the receipt's record key, so
        // each intent's dedup matches the originally committed job.
        let identity = format!("{}|jobs:{intents_hash}", put_request_identity(req)?);
        let Some(receipt) = self.lookup_put_receipt_expect(req, &identity).await? else {
            return Ok(None);
        };
        let self_key = receipt.id.bare_key().to_string();
        let mut jobs = Vec::with_capacity(intents.len());
        for it in intents {
            let r = it.resolve(scope.as_str(), Some(&self_key));
            validate_enqueue(&r)?;
            jobs.push(self.enqueue_job(r).await?);
        }
        Ok(Some(WriteWithJobsResult {
            write: receipt,
            jobs,
        }))
    }

    async fn put_with_jobs(
        &self,
        mut req: PutRequest,
        generated_embedding: Option<Vec<f32>>,
        intents: Vec<JobIntent>,
    ) -> Result<WriteWithJobsResult> {
        let scope = nomiso_core::validate::validate_put_shape(&req, &self.config.limits)?;
        req.scope = scope.as_str().to_string();
        if intents.len() > 8 {
            return Err(Error::PayloadTooLarge("job intents exceed 8".into()));
        }
        if req.embedding.is_some() && generated_embedding.is_some() {
            return Err(Error::invalid(
                "cannot supply both original and generated embeddings",
            ));
        }
        let intents_hash = intents_identity(&intents)?;
        let original = req.clone();
        if let Some(hit) = self.lookup_put_receipt_with_intents(&req, &intents).await? {
            return Ok(hit);
        }
        validate_put(&req, &self.config.limits)?;
        if generated_embedding.is_some() {
            req.embedding = generated_embedding;
            validate_put(&req, &self.config.limits)?;
        }
        // One bounded retry covers a dedup-slot race: a concurrent identical
        // intent committed between the pre-check and our transaction aborts
        // the write; the retry then dedups against the live holder.
        for attempt in 0..2 {
            let mem_key = Uuid::now_v7().to_string();
            let (job_sql, job_binds, mut slots) = self
                .resolve_job_intents(&intents, scope.as_str(), &mem_key)
                .await?;
            let write = if let Some(key) = req.idempotency_key.clone() {
                let identity = format!("{}|jobs:{intents_hash}", put_request_identity(&original)?);
                match self
                    .put_idempotent_inner(
                        &original,
                        req.clone(),
                        key,
                        KeyedTxn {
                            request_identity: identity,
                            mem_key,
                            extra_sql: &job_sql,
                            extra_binds: &job_binds,
                        },
                    )
                    .await
                {
                    Ok(w) => w,
                    Err(e) if is_state_create_conflict(&e) && attempt == 0 => continue,
                    Err(e) => return Err(e),
                }
            } else {
                match self
                    .create_memory_inner(&req, None, Some(mem_key), &job_sql, &job_binds)
                    .await
                {
                    Ok(rec) => WriteResult {
                        replayed: false,
                        id: rec.id.clone(),
                        version: rec.version,
                        record: Some(rec),
                    },
                    Err(e) if is_state_create_conflict(&e) && attempt == 0 => continue,
                    Err(e) => return Err(e),
                }
            };
            let mut jobs = Vec::with_capacity(intents.len());
            for slot in slots.iter_mut() {
                match slot.take() {
                    Some(PendingJob::Deduped(r)) => jobs.push(*r),
                    Some(PendingJob::Inserted(dedup)) => {
                        let mut r = self.dedup_replay(scope.as_str(), &dedup).await?;
                        // The job is durable from this commit; on a replayed
                        // write it was committed by the original request.
                        r.deduplicated = write.replayed;
                        jobs.push(r);
                    }
                    None => return Err(Error::internal("job intent outcome missing")),
                }
            }
            return Ok(WriteWithJobsResult { write, jobs });
        }
        Err(Error::store(
            "put_with_jobs: job dedup conflict persisted across retry",
        ))
    }

    async fn supersede_with_jobs(
        &self,
        req: SupersedeRequest,
        intents: Vec<JobIntent>,
    ) -> Result<WriteWithJobsResult> {
        if intents.len() > 8 {
            return Err(Error::PayloadTooLarge("job intents exceed 8".into()));
        }
        for attempt in 0..2 {
            match self.supersede_inner(req.clone(), &intents).await {
                Ok((write, mut slots)) => {
                    let scope = write
                        .record
                        .as_ref()
                        .map(|r| r.scope.clone())
                        .unwrap_or_else(|| req.new.scope.clone());
                    let mut jobs = Vec::with_capacity(intents.len());
                    for slot in slots.iter_mut() {
                        match slot.take() {
                            Some(PendingJob::Deduped(r)) => jobs.push(*r),
                            Some(PendingJob::Inserted(dedup)) => {
                                let mut r = self.dedup_replay(&scope, &dedup).await?;
                                r.deduplicated = false;
                                jobs.push(r);
                            }
                            None => return Err(Error::internal("job intent outcome missing")),
                        }
                    }
                    return Ok(WriteWithJobsResult { write, jobs });
                }
                Err(e) if is_state_create_conflict(&e) && attempt == 0 => continue,
                Err(e) => return Err(e),
            }
        }
        Err(Error::store(
            "supersede_with_jobs: job dedup conflict persisted across retry",
        ))
    }

    #[instrument(skip(self, query))]
    async fn search_detailed(&self, query: SearchQuery) -> Result<nomiso_core::ops::SearchOutcome> {
        let mut query = query;
        let scope = validate_search(&query, &self.config.limits)?;
        query.scope = scope.as_str().to_string();
        let limit = query
            .limit
            .unwrap_or(self.config.search.default_limit)
            .min(self.config.search.max_search_limit)
            .max(1);
        let as_of = query.as_of.unwrap_or_else(now);
        let as_of_s = ts_param(as_of);
        let known_as_of = query.known_as_of.map(ts_param);
        let sys_as_of = query.sys_as_of.map(ts_param);
        let rrf_k = self.config.search.rrf_k;
        let has_text = !query.query.trim().is_empty();
        let has_vec = query
            .embedding
            .as_ref()
            .map(|v| !v.is_empty())
            .unwrap_or(false);
        // Temporal lenses are pushed into every SQL branch below; the post-filter
        // remains as defense-in-depth.
        let need_temporal_post = known_as_of.is_some() || sys_as_of.is_some();
        let fetch_limit = limit;
        let cand = self.config.search.candidate_limit.max(fetch_limit);

        let categories: Option<Vec<String>> = query
            .categories
            .as_ref()
            .map(|c| c.iter().map(|x| x.as_str().to_string()).collect());

        let scope_sql_exact = matches!(query.scope_match, ScopeMatch::Exact);
        let mut hits: Vec<SearchHit> = Vec::new();

        if has_text && has_vec {
            let k = cand;
            let ef = self.config.search.hnsw_ef;
            let lim = fetch_limit;
            let rk = rrf_k;
            let sql = if scope_sql_exact {
                format!(
                    r#"
                LET $as_of = type::datetime($as_of);
                LET $vs = (
                    SELECT id, category, scope, content, valid_from, valid_until, provenance, version, embedding_generation,
                           vector::distance::knn() AS distance
                    FROM memory
                    WHERE embedding <|{k},{ef}|> $qvec
                      AND scope = $scope
                      AND valid_from <= $as_of
                      AND (valid_until = NONE OR valid_until > $as_of)
                      AND ($cats = NONE OR category IN $cats)
                      AND ($known_as_of = NONE OR known_at <= type::datetime($known_as_of))
                      AND ($sys_as_of = NONE OR (
                          (IF sys_created = NONE THEN known_at ELSE sys_created END) <= type::datetime($sys_as_of)
                          AND (sys_closed = NONE OR sys_closed > type::datetime($sys_as_of))
                      ))
                );
                LET $ft = (
                    SELECT id, category, scope, content, valid_from, valid_until, provenance, version, embedding_generation,
                           search::score(0) AS score
                    FROM memory
                    WHERE content.text @0@ $qtext
                      AND scope = $scope
                      AND valid_from <= $as_of
                      AND (valid_until = NONE OR valid_until > $as_of)
                      AND ($cats = NONE OR category IN $cats)
                      AND ($known_as_of = NONE OR known_at <= type::datetime($known_as_of))
                      AND ($sys_as_of = NONE OR (
                          (IF sys_created = NONE THEN known_at ELSE sys_created END) <= type::datetime($sys_as_of)
                          AND (sys_closed = NONE OR sys_closed > type::datetime($sys_as_of))
                      ))
                    ORDER BY score DESC
                    LIMIT {k}
                );
                search::rrf([$vs, $ft], {lim}, {rk});
                "#
                )
            } else {
                format!(
                    r#"
                LET $as_of = type::datetime($as_of);
                LET $vs = (
                    SELECT id, category, scope, content, valid_from, valid_until, provenance, version, embedding_generation,
                           vector::distance::knn() AS distance
                    FROM memory
                    WHERE embedding <|{k},{ef}|> $qvec
                      AND (scope = $scope OR string::starts_with(scope, $scope_prefix))
                      AND valid_from <= $as_of
                      AND (valid_until = NONE OR valid_until > $as_of)
                      AND ($cats = NONE OR category IN $cats)
                      AND ($known_as_of = NONE OR known_at <= type::datetime($known_as_of))
                      AND ($sys_as_of = NONE OR (
                          (IF sys_created = NONE THEN known_at ELSE sys_created END) <= type::datetime($sys_as_of)
                          AND (sys_closed = NONE OR sys_closed > type::datetime($sys_as_of))
                      ))
                );
                LET $ft = (
                    SELECT id, category, scope, content, valid_from, valid_until, provenance, version, embedding_generation,
                           search::score(0) AS score
                    FROM memory
                    WHERE content.text @0@ $qtext
                      AND (scope = $scope OR string::starts_with(scope, $scope_prefix))
                      AND valid_from <= $as_of
                      AND (valid_until = NONE OR valid_until > $as_of)
                      AND ($cats = NONE OR category IN $cats)
                      AND ($known_as_of = NONE OR known_at <= type::datetime($known_as_of))
                      AND ($sys_as_of = NONE OR (
                          (IF sys_created = NONE THEN known_at ELSE sys_created END) <= type::datetime($sys_as_of)
                          AND (sys_closed = NONE OR sys_closed > type::datetime($sys_as_of))
                      ))
                    ORDER BY score DESC
                    LIMIT {k}
                );
                search::rrf([$vs, $ft], {lim}, {rk});
                "#
                )
            };

            let mut response = self
                .db
                .query(sql)
                .bind(("as_of", as_of_s.clone()))
                .bind(("qvec", query.embedding.clone().unwrap_or_default()))
                .bind(("scope", query.scope.clone()))
                .bind(("scope_prefix", format!("{}/", query.scope)))
                .bind(("qtext", query.query.clone()))
                .bind(("cats", categories.clone()))
                .bind(("known_as_of", known_as_of.clone()))
                .bind(("sys_as_of", sys_as_of.clone()))
                .await
                .map_err(|e| Error::store(format!("hybrid search: {e}")))?;
            ensure_ok(&mut response)?;
            hits = take_search_hits(
                &mut response,
                SearchSignals {
                    vector: true,
                    bm25: true,
                    graph: false,
                    expanded: false,
                },
            )?;
        } else if has_text {
            let sql = if scope_sql_exact {
                r#"
                LET $as_of = type::datetime($as_of);
                SELECT id, category, scope, content, valid_from, valid_until, provenance, version, embedding_generation,
                       search::score(0) AS score
                FROM memory
                WHERE content.text @0@ $qtext
                  AND scope = $scope
                  AND valid_from <= $as_of
                  AND (valid_until = NONE OR valid_until > $as_of)
                  AND ($cats = NONE OR category IN $cats)
                      AND ($known_as_of = NONE OR known_at <= type::datetime($known_as_of))
                      AND ($sys_as_of = NONE OR (
                          (IF sys_created = NONE THEN known_at ELSE sys_created END) <= type::datetime($sys_as_of)
                          AND (sys_closed = NONE OR sys_closed > type::datetime($sys_as_of))
                      ))
                ORDER BY score DESC
                LIMIT $limit;
                "#
            } else {
                r#"
                LET $as_of = type::datetime($as_of);
                SELECT id, category, scope, content, valid_from, valid_until, provenance, version, embedding_generation,
                       search::score(0) AS score
                FROM memory
                WHERE content.text @0@ $qtext
                  AND (scope = $scope OR string::starts_with(scope, $scope_prefix))
                  AND valid_from <= $as_of
                  AND (valid_until = NONE OR valid_until > $as_of)
                  AND ($cats = NONE OR category IN $cats)
                      AND ($known_as_of = NONE OR known_at <= type::datetime($known_as_of))
                      AND ($sys_as_of = NONE OR (
                          (IF sys_created = NONE THEN known_at ELSE sys_created END) <= type::datetime($sys_as_of)
                          AND (sys_closed = NONE OR sys_closed > type::datetime($sys_as_of))
                      ))
                ORDER BY score DESC
                LIMIT $limit;
                "#
            };
            let mut response = self
                .db
                .query(sql)
                .bind(("as_of", as_of_s.clone()))
                .bind(("qtext", query.query.clone()))
                .bind(("scope", query.scope.clone()))
                .bind(("scope_prefix", format!("{}/", query.scope)))
                .bind(("limit", fetch_limit as i64))
                .bind(("cats", categories.clone()))
                .bind(("known_as_of", known_as_of.clone()))
                .bind(("sys_as_of", sys_as_of.clone()))
                .await
                .map_err(|e| Error::store(format!("bm25 search: {e}")))?;
            ensure_ok(&mut response)?;
            hits = take_search_hits(
                &mut response,
                SearchSignals {
                    vector: false,
                    bm25: true,
                    graph: false,
                    expanded: false,
                },
            )?;
        } else if has_vec {
            // KNN operator requires literal integers for K (and optional ef).
            let k = fetch_limit;
            let ef = self.config.search.hnsw_ef;
            let sql = if scope_sql_exact {
                format!(
                    r#"
                LET $as_of = type::datetime($as_of);
                SELECT id, category, scope, content, valid_from, valid_until, provenance, version, embedding_generation,
                       vector::distance::knn() AS distance
                FROM memory
                WHERE embedding <|{k},{ef}|> $qvec
                  AND scope = $scope
                  AND valid_from <= $as_of
                  AND (valid_until = NONE OR valid_until > $as_of)
                  AND ($cats = NONE OR category IN $cats)
                      AND ($known_as_of = NONE OR known_at <= type::datetime($known_as_of))
                      AND ($sys_as_of = NONE OR (
                          (IF sys_created = NONE THEN known_at ELSE sys_created END) <= type::datetime($sys_as_of)
                          AND (sys_closed = NONE OR sys_closed > type::datetime($sys_as_of))
                      ));
                "#
                )
            } else {
                format!(
                    r#"
                LET $as_of = type::datetime($as_of);
                SELECT id, category, scope, content, valid_from, valid_until, provenance, version, embedding_generation,
                       vector::distance::knn() AS distance
                FROM memory
                WHERE embedding <|{k},{ef}|> $qvec
                  AND (scope = $scope OR string::starts_with(scope, $scope_prefix))
                  AND valid_from <= $as_of
                  AND (valid_until = NONE OR valid_until > $as_of)
                  AND ($cats = NONE OR category IN $cats)
                      AND ($known_as_of = NONE OR known_at <= type::datetime($known_as_of))
                      AND ($sys_as_of = NONE OR (
                          (IF sys_created = NONE THEN known_at ELSE sys_created END) <= type::datetime($sys_as_of)
                          AND (sys_closed = NONE OR sys_closed > type::datetime($sys_as_of))
                      ));
                "#
                )
            };
            let mut response = self
                .db
                .query(sql)
                .bind(("as_of", as_of_s.clone()))
                .bind(("qvec", query.embedding.clone().unwrap_or_default()))
                .bind(("scope", query.scope.clone()))
                .bind(("scope_prefix", format!("{}/", query.scope)))
                .bind(("cats", categories.clone()))
                .bind(("known_as_of", known_as_of.clone()))
                .bind(("sys_as_of", sys_as_of.clone()))
                .await
                .map_err(|e| Error::store(format!("vector search: {e}")))?;
            ensure_ok(&mut response)?;
            hits = take_search_hits(
                &mut response,
                SearchSignals {
                    vector: true,
                    bm25: false,
                    graph: false,
                    expanded: false,
                },
            )?;
        }

        hits = self.filter_scope(hits, &scope, query.scope_match);
        if need_temporal_post {
            hits = self
                .filter_known_sys(
                    hits,
                    query.known_as_of,
                    query.sys_as_of,
                    &query.scope,
                    query.scope_match,
                )
                .await?;
        }
        hits.truncate(limit as usize);

        // T5: opt-in graph candidate expansion. Seeds are the post-truncate
        // direct hits; expanded-only candidates append below them.
        let mut stats = nomiso_core::ops::SearchStats::default();
        if let Some(gx) = &query.graph_expand {
            if !hits.is_empty() {
                stats.expansion = Some(
                    self.expand_candidates(
                        &mut hits,
                        &ExpandCtx {
                            query: &query,
                            scope: &scope,
                            as_of_s: as_of_s.as_str(),
                            known_as_of: known_as_of.as_deref(),
                            sys_as_of: sys_as_of.as_deref(),
                            categories: categories.as_ref(),
                            gx,
                        },
                    )
                    .await?,
                );
            } else {
                stats.expansion = Some(nomiso_core::ops::ExpansionStats {
                    seeds: 0,
                    edges_scanned: 0,
                    candidates_found: 0,
                    candidates_added: 0,
                    truncated: Vec::new(),
                    elapsed_ms: 0,
                });
            }
        }

        let do_graph = query
            .graph_enrich
            .unwrap_or(self.config.search.enable_graph_enrich);
        if do_graph && !hits.is_empty() {
            for hit in hits.iter_mut() {
                if let Ok(Some(row)) = self.fetch_memory_raw(&hit.id).await {
                    if let Some(links) = row.entity_links {
                        if !links.is_empty() {
                            hit.entities = links;
                            hit.signals.graph = true;
                        }
                    }
                }
            }
        }

        Ok(nomiso_core::ops::SearchOutcome { hits, stats })
    }

    #[instrument(skip(self, req))]
    async fn read(&self, req: ReadRequest) -> Result<Vec<nomiso_core::types::MemoryRecord>> {
        let mut req = req;
        let scope = validate_read(&req, &self.config.limits)?;
        req.scope = scope.as_str().to_string();
        let as_of = req.as_of;
        let mut out = Vec::new();
        for id in &req.ids {
            let Some(row) = self.fetch_memory_raw(id).await? else {
                continue;
            };
            if !Self::scope_allowed(&row.scope, &scope, req.scope_match) {
                return Err(Error::ScopeDenied(format!(
                    "record {} not in scope {}",
                    id, req.scope
                )));
            }
            let rec = row_to_record(row)?;
            if let Some(as_of) = as_of {
                let valid =
                    rec.valid_from <= as_of && rec.valid_until.map(|u| u > as_of).unwrap_or(true);
                if !valid {
                    continue;
                }
            }
            if let Some(k) = req.known_as_of {
                if rec.known_at > k {
                    continue;
                }
            }
            if let Some(u) = req.sys_as_of {
                let created = rec.sys_created.unwrap_or(rec.known_at);
                if created > u {
                    continue;
                }
                if let Some(closed) = rec.sys_closed {
                    if closed <= u {
                        continue;
                    }
                }
            }
            out.push(rec);
        }
        Ok(out)
    }

    #[instrument(skip(self, req))]
    async fn forget(&self, req: ForgetRequest) -> Result<()> {
        let mut req = req;
        let scope = validate_forget(&req)?;
        req.scope = scope.as_str().to_string();
        let row = self
            .fetch_memory_raw(&req.id)
            .await?
            .ok_or_else(|| Error::NotFound(req.id.to_string()))?;
        if row.scope != req.scope {
            return Err(Error::ScopeDenied(format!(
                "forget requires exact owning scope, got '{}' for '{}'",
                req.scope, row.scope
            )));
        }
        // Early check when version supplied (SQL WHERE is authoritative for soft).
        if let Some(expected) = req.expected_version {
            check_version(expected, row.version)?;
        }
        let key = req.id.bare_key().to_string();
        // One-shot close: reject soft-forget if already closed.
        if !req.hard
            && (!matches!(row.valid_until.as_ref(), None | Some(Value::Null))
                || row.sys_closed.as_ref().is_some_and(|v| !v.is_null()))
        {
            return Err(Error::invalid(
                "valid interval already closed (one-shot close; use hard erase/purge to remove)",
            ));
        }
        if req.hard {
            // WRITE-010: the erase event records the committed deletion, inside
            // the same transaction — a failed delete produces no success event.
            let delete_stmt = if req.expected_version.is_some() {
                r#"
                DELETE type::record('memory', $key)
                WHERE version = $expected_version
                RETURN BEFORE
                "#
            } else {
                r#"
                DELETE type::record('memory', $key)
                RETURN BEFORE
                "#
            };
            let sql = format!(
                r#"
                BEGIN TRANSACTION;
                LET $deleted = ({delete_stmt});
                IF array::len($deleted) = 0 {{
                    THROW 'nomiso_conflict';
                }};
                LET $dv = $deleted[0].version;
                // REL-004: edges referencing an erased endpoint are marked
                // purged in the same commit — never silently reconnected.
                UPDATE relationship SET
                    state = 'purged',
                    state_reason = 'endpoint_erased',
                    live_key = 'X|' + <string> id,
                    updated_at = time::now()
                WHERE scope = $scope AND state != 'purged'
                  AND ((subject_kind = 'memory' AND subject_id = $key)
                    OR (object_kind = 'memory' AND object_id = $key));
                // MIG-005: staged reindex vectors for the erased row go with it.
                DELETE embedding_vector WHERE memory = type::record('memory', $key);
                CREATE type::record('belief_event', $eid) SET
                    scope = $scope,
                    memory_id = $mid,
                    kind = 'hard_erase',
                    at_sys = time::now(),
                    payload = {{ id: $mid, version: $dv }};
                RETURN $deleted;
                COMMIT TRANSACTION;
                "#
            );
            let mut q = self
                .db
                .query(sql)
                .bind(("key", key.clone()))
                .bind(("eid", Uuid::now_v7().to_string()))
                .bind(("mid", req.id.to_string()))
                .bind(("scope", req.scope.clone()));
            if let Some(ev) = req.expected_version {
                q = q.bind(("expected_version", ev as i64));
            }
            let mut response = q.await.map_err(|e| Error::store(e.to_string()))?;
            match ensure_ok(&mut response) {
                Ok(()) => {}
                Err(e) if is_conflict_store_err(&e) => {
                    return Err(self
                        .conflict_from_memory(&req.id, req.expected_version.unwrap_or(row.version))
                        .await);
                }
                Err(e) => return Err(e),
            }
        } else {
            let at = req.at.unwrap_or_else(now);
            // One-shot close must not invert the valid interval.
            if let Ok(vf) = crate::mapping::parse_timestamp(&row.valid_from) {
                if at < vf {
                    return Err(Error::invalid(format!(
                        "soft-forget at ({at}) is before valid_from ({vf})"
                    )));
                }
            }
            // Soft-forget: one-shot close + optional version guard, with the
            // journal event committed in the same transaction.
            let update_stmt = if req.expected_version.is_some() {
                r#"
                UPDATE type::record('memory', $key)
                SET valid_until = type::datetime($at),
                    sys_closed = time::now(),
                    version = version + 1
                WHERE version = $expected_version
                  AND valid_until = NONE
                  AND sys_closed = NONE
                RETURN AFTER
                "#
            } else {
                r#"
                UPDATE type::record('memory', $key)
                SET valid_until = type::datetime($at),
                    sys_closed = time::now(),
                    version = version + 1
                WHERE valid_until = NONE AND sys_closed = NONE
                RETURN AFTER
                "#
            };
            let sql = format!(
                r#"
                BEGIN TRANSACTION;
                LET $closed = ({update_stmt});
                IF array::len($closed) = 0 {{
                    THROW 'nomiso_conflict';
                }};
                LET $cv = $closed[0].version;
                CREATE type::record('belief_event', $eid) SET
                    scope = $scope,
                    memory_id = $mid,
                    kind = 'soft_forget',
                    at_sys = time::now(),
                    payload = {{ at: $at_str, version: $cv }};
                RETURN $closed;
                COMMIT TRANSACTION;
                "#
            );
            let mut q = self
                .db
                .query(sql)
                .bind(("key", key.clone()))
                .bind(("eid", Uuid::now_v7().to_string()))
                .bind(("mid", req.id.to_string()))
                .bind(("scope", req.scope.clone()))
                .bind(("at_str", at.to_string()))
                .bind(("at", ts_param(at)));
            if let Some(ev) = req.expected_version {
                q = q.bind(("expected_version", ev as i64));
            }
            let mut response = q.await.map_err(|e| Error::store(e.to_string()))?;
            match ensure_ok(&mut response) {
                Ok(()) => {}
                Err(e) if is_conflict_store_err(&e) => {
                    return Err(self
                        .conflict_from_memory(&req.id, req.expected_version.unwrap_or(0))
                        .await);
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    async fn health(&self) -> Result<()> {
        let mut response = self
            .db
            .query("RETURN true;")
            .await
            .map_err(|e| Error::store(e.to_string()))?;
        ensure_ok(&mut response)?;
        Ok(())
    }

    async fn append_trace_event(
        &self,
        req: nomiso_core::trace::AppendTraceEvent,
    ) -> Result<nomiso_core::trace::TraceEventRecord> {
        use nomiso_core::scope::ScopePath;
        use nomiso_core::trace::{TraceEventKind, TraceEventRecord};
        let mut req = req;
        req.scope = ScopePath::parse(&req.scope)?.as_str().to_string();
        let tid = req.trace_id.trim();
        if tid.is_empty() {
            return Err(Error::invalid("trace_id is required"));
        }
        if tid.len() > 128
            || !tid
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(Error::invalid("trace_id must be 1–128 chars [A-Za-z0-9_-]"));
        }
        if TraceEventKind::parse(req.kind.as_str()).is_none() {
            return Err(Error::invalid("invalid trace event kind"));
        }
        let created = now();
        let ev_key = Uuid::now_v7().to_string();
        let mut mem_ids = req.memory_ids.clone();
        if mem_ids.is_empty() {
            if let Some(p) = req.payload.as_ref() {
                mem_ids = nomiso_core::trace::memory_ids_from_payload(p);
            }
        }
        let memory_ids = expand_memory_id_index(&mem_ids);
        let memory_ids_bind: Option<Vec<String>> = if memory_ids.is_empty() {
            None
        } else {
            Some(memory_ids)
        };
        // Lazy-create parent; never overwrite an existing parent scope.
        let sql = r#"
            LET $parent = (SELECT * FROM type::record('trace', $tid));
            IF array::len($parent) > 0 AND $parent[0].scope != $scope {
                THROW 'nomiso_scope_denied';
            };
            UPSERT type::record('trace', $tid) SET
                scope = IF scope = NONE THEN $scope ELSE scope END,
                session_id = IF session_id = NONE THEN $session_id ELSE session_id END,
                turn_id = IF turn_id = NONE THEN $turn_id ELSE turn_id END,
                created_at = IF created_at = NONE THEN type::datetime($created) ELSE created_at END;
            CREATE type::record('trace_event', $ev_key) SET
                trace_id = $tid,
                scope = $scope,
                kind = $kind,
                session_id = $session_id,
                turn_id = $turn_id,
                payload = $payload,
                memory_ids = $memory_ids,
                created_at = type::datetime($created)
            RETURN AFTER;
        "#;
        let mut response = self
            .db
            .query(sql)
            .bind(("tid", req.trace_id.clone()))
            .bind(("ev_key", ev_key.clone()))
            .bind(("scope", req.scope.clone()))
            .bind(("session_id", req.session_id.clone()))
            .bind(("turn_id", req.turn_id.clone()))
            .bind(("kind", req.kind.as_str().to_string()))
            .bind(("payload", req.payload.clone()))
            .bind(("memory_ids", memory_ids_bind))
            .bind(("created", ts_param(created)))
            .await
            .map_err(|e| {
                let err = Error::store(format!("append_trace: {e}"));
                if is_scope_denied_store_err(&err) {
                    Error::ScopeDenied("trace_id already bound to another scope".into())
                } else {
                    err
                }
            })?;
        if let Err(e) = ensure_ok(&mut response) {
            if is_scope_denied_store_err(&e) {
                return Err(Error::ScopeDenied(
                    "trace_id already bound to another scope".into(),
                ));
            }
            return Err(e);
        }
        Ok(TraceEventRecord {
            id: format!("trace_event:{ev_key}"),
            trace_id: req.trace_id,
            scope: req.scope,
            kind: req.kind,
            session_id: req.session_id,
            turn_id: req.turn_id,
            payload: req.payload,
            created_at: created,
        })
    }

    async fn get_trace(
        &self,
        trace_id: &str,
        scope: &str,
    ) -> Result<nomiso_core::trace::TraceBundle> {
        use nomiso_core::trace::{TraceBundle, TraceEventKind, TraceEventRecord};
        let scope = nomiso_core::scope::ScopePath::parse(scope)?;
        let scope = scope.as_str();
        let sql = r#"
            SELECT * FROM type::record('trace', $tid);
            SELECT * FROM trace_event WHERE trace_id = $tid AND scope = $scope ORDER BY created_at ASC;
        "#;
        let mut response = self
            .db
            .query(sql)
            .bind(("tid", trace_id.to_string()))
            .bind(("scope", scope.to_string()))
            .await
            .map_err(|e| Error::store(format!("get_trace: {e}")))?;
        ensure_ok(&mut response)?;
        let parents = take_json_rows(&mut response, 0)?;
        if parents.is_empty() {
            // Still allow events-only if parent missing but events exist
        } else if let Some(p) = parents.first() {
            if let Some(ps) = p.get("scope").and_then(|v| v.as_str()) {
                if ps != scope {
                    return Err(Error::ScopeDenied(format!(
                        "trace {trace_id} not in scope {scope}"
                    )));
                }
            }
        }
        let rows = take_json_rows(&mut response, 1)?;
        let mut events = Vec::new();
        for v in rows {
            let kind_s = v.get("kind").and_then(|x| x.as_str()).unwrap_or("search");
            let kind = TraceEventKind::parse(kind_s)
                .ok_or_else(|| Error::store(format!("bad kind {kind_s}")))?;
            let id = v
                .get("id")
                .map(|x| x.to_string().trim_matches('"').to_string())
                .unwrap_or_default();
            let created_at = match v.get("created_at") {
                Some(c) => crate::mapping::parse_timestamp(c)?,
                None => now(),
            };
            events.push(TraceEventRecord {
                id,
                trace_id: trace_id.to_string(),
                scope: scope.to_string(),
                kind,
                session_id: v
                    .get("session_id")
                    .and_then(|x| x.as_str())
                    .map(|s| s.to_string()),
                turn_id: v
                    .get("turn_id")
                    .and_then(|x| x.as_str())
                    .map(|s| s.to_string()),
                payload: v.get("payload").cloned(),
                created_at,
            });
        }
        let (session_id, turn_id) = events
            .first()
            .map(|e| (e.session_id.clone(), e.turn_id.clone()))
            .unwrap_or((None, None));
        Ok(TraceBundle {
            trace_id: trace_id.to_string(),
            scope: scope.to_string(),
            session_id,
            turn_id,
            events,
        })
    }

    async fn list_traces(
        &self,
        req: nomiso_core::trace::ListTracesRequest,
    ) -> Result<Vec<nomiso_core::trace::TraceSummary>> {
        use nomiso_core::trace::TraceSummary;
        let mut req = req;
        req.scope = nomiso_core::scope::ScopePath::parse(&req.scope)?
            .as_str()
            .to_string();
        let limit = req.limit.unwrap_or(32).clamp(1, 128);
        let session_id = req
            .session_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let turn_id = req
            .turn_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let sql = r#"
            SELECT * FROM trace
            WHERE scope = $scope
              AND ($since = NONE OR created_at >= type::datetime($since))
              AND ($until = NONE OR created_at <= type::datetime($until))
              AND ($session_id = NONE OR session_id = $session_id)
              AND ($turn_id = NONE OR turn_id = $turn_id)
            ORDER BY created_at DESC
            LIMIT $limit;
        "#;
        let mut q = self
            .db
            .query(sql)
            .bind(("scope", req.scope.clone()))
            .bind(("limit", limit as i64));
        q = q.bind(("since", req.since.map(ts_param)));
        q = q.bind(("until", req.until.map(ts_param)));
        q = q.bind(("session_id", session_id));
        q = q.bind(("turn_id", turn_id));
        let mut response = q
            .await
            .map_err(|e| Error::store(format!("list_traces: {e}")))?;
        ensure_ok(&mut response)?;
        let rows = take_json_rows(&mut response, 0)?;
        let mut out = Vec::with_capacity(rows.len());
        for v in rows {
            let id = v
                .get("id")
                .map(crate::mapping::record_id_to_string)
                .transpose()?
                .unwrap_or_default();
            let tid = id
                .trim_start_matches("trace:")
                .trim_matches(|c| c == '`' || c == '⟨' || c == '⟩')
                .to_string();
            let created_at = match v.get("created_at") {
                Some(c) => crate::mapping::parse_timestamp(c)?,
                None => now(),
            };
            out.push(TraceSummary {
                trace_id: tid,
                scope: v
                    .get("scope")
                    .and_then(|x| x.as_str())
                    .unwrap_or(&req.scope)
                    .to_string(),
                session_id: v
                    .get("session_id")
                    .and_then(|x| x.as_str())
                    .map(str::to_string),
                turn_id: v
                    .get("turn_id")
                    .and_then(|x| x.as_str())
                    .map(str::to_string),
                created_at,
            });
        }
        Ok(out)
    }

    async fn list_traces_for_memory(
        &self,
        req: nomiso_core::trace::TracesByMemoryRequest,
    ) -> Result<Vec<nomiso_core::trace::TraceByMemory>> {
        use nomiso_core::trace::{TraceByMemory, TraceEventKind, TraceOutcome};
        let mut req = req;
        req.scope = nomiso_core::scope::ScopePath::parse(&req.scope)?
            .as_str()
            .to_string();
        let bare = req.memory_id.bare_key().to_string();
        if bare.is_empty() {
            return Ok(vec![]);
        }
        let limit = req.limit.unwrap_or(32).clamp(1, 128);
        let session_id = req
            .session_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let turn_id = req
            .turn_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let mid = req.memory_id.to_string();
        let mid2 = format!("memory:{bare}");
        let sql = r#"
            SELECT trace_id, kind, created_at, memory_ids, payload FROM trace_event
            WHERE scope = $scope
              AND (
                $mid IN memory_ids OR $mid2 IN memory_ids
                OR memory_ids = NONE OR memory_ids = []
              );
        "#;
        let mut response = self
            .db
            .query(sql)
            .bind(("scope", req.scope.clone()))
            .bind(("mid", mid.clone()))
            .bind(("mid2", mid2.clone()))
            .await
            .map_err(|e| Error::store(format!("list_traces_for_memory: {e}")))?;
        ensure_ok(&mut response)?;
        let ev_rows = take_json_rows(&mut response, 0)?;
        let mut kinds_by_tid: std::collections::HashMap<String, Vec<TraceEventKind>> =
            std::collections::HashMap::new();
        for v in ev_rows {
            if !event_row_matches_memory(&v, &req.memory_id) {
                continue;
            }
            let Some(tid) = json_opt_string(&v, "trace_id").filter(|s| !s.is_empty()) else {
                continue;
            };
            let Some(kind) = v
                .get("kind")
                .and_then(|x| x.as_str())
                .and_then(TraceEventKind::parse)
            else {
                continue;
            };
            let entry = kinds_by_tid.entry(tid).or_default();
            if !entry.contains(&kind) {
                entry.push(kind);
            }
        }
        if kinds_by_tid.is_empty() {
            return Ok(vec![]);
        }
        let parent_sql = r#"
            SELECT * FROM trace
            WHERE scope = $scope
              AND ($since = NONE OR created_at >= type::datetime($since))
              AND ($until = NONE OR created_at <= type::datetime($until))
              AND ($session_id = NONE OR session_id = $session_id)
              AND ($turn_id = NONE OR turn_id = $turn_id)
            ORDER BY created_at DESC;
        "#;
        let mut parent_resp = self
            .db
            .query(parent_sql)
            .bind(("scope", req.scope.clone()))
            .bind(("since", req.since.map(ts_param)))
            .bind(("until", req.until.map(ts_param)))
            .bind(("session_id", session_id))
            .bind(("turn_id", turn_id))
            .await
            .map_err(|e| Error::store(format!("list_traces_for_memory parents: {e}")))?;
        ensure_ok(&mut parent_resp)?;
        let parent_rows = take_json_rows(&mut parent_resp, 0)?;
        let mut parents = Vec::new();
        for v in parent_rows {
            let id = v
                .get("id")
                .map(crate::mapping::record_id_to_string)
                .transpose()?
                .unwrap_or_default();
            let tid = id
                .trim_start_matches("trace:")
                .trim_matches(|c| c == '`' || c == '⟨' || c == '⟩')
                .to_string();
            if !kinds_by_tid.contains_key(&tid) {
                continue;
            }
            let created_at = match v.get("created_at") {
                Some(c) => crate::mapping::parse_timestamp(c)?,
                None => now(),
            };
            parents.push((
                tid,
                v.get("session_id")
                    .and_then(|x| x.as_str())
                    .map(str::to_string),
                v.get("turn_id")
                    .and_then(|x| x.as_str())
                    .map(str::to_string),
                created_at,
            ));
            if parents.len() >= limit as usize {
                break;
            }
        }
        if parents.is_empty() {
            return Ok(vec![]);
        }
        let tids: Vec<String> = parents.iter().map(|(t, _, _, _)| t.clone()).collect();
        let mut outcome_resp = self
            .db
            .query(
                r#"
                SELECT trace_id, payload, created_at FROM trace_event
                WHERE scope = $scope AND kind = 'outcome' AND trace_id IN $tids
                ORDER BY created_at DESC;
                "#,
            )
            .bind(("scope", req.scope.clone()))
            .bind(("tids", tids))
            .await
            .map_err(|e| Error::store(format!("list_traces_for_memory outcomes: {e}")))?;
        ensure_ok(&mut outcome_resp)?;
        let outcome_rows = take_json_rows(&mut outcome_resp, 0)?;
        let mut latest_outcome: std::collections::HashMap<
            String,
            (Option<TraceOutcome>, Option<String>),
        > = std::collections::HashMap::new();
        for v in outcome_rows {
            let Some(tid) = json_opt_string(&v, "trace_id").filter(|s| !s.is_empty()) else {
                continue;
            };
            if latest_outcome.contains_key(&tid) {
                continue;
            }
            latest_outcome.insert(tid, parse_trace_outcome_payload(v.get("payload")));
        }
        let mut out = Vec::with_capacity(parents.len());
        for (tid, session_id, turn_id, created_at) in parents {
            let matched_kinds = kinds_by_tid.remove(&tid).unwrap_or_default();
            let (outcome, outcome_note) = latest_outcome.remove(&tid).unwrap_or((None, None));
            out.push(TraceByMemory {
                trace_id: tid,
                scope: req.scope.clone(),
                session_id,
                turn_id,
                created_at,
                matched_kinds,
                outcome,
                outcome_note,
            });
        }
        Ok(out)
    }

    async fn list(
        &self,
        req: nomiso_core::list::ListRequest,
    ) -> Result<nomiso_core::list::ListResponse> {
        use nomiso_core::list::{ListCursor, ListCursorQuery, ListItem, ListResponse};
        let mut req = req;
        let scope = nomiso_core::scope::ScopePath::parse(&req.scope)?;
        req.scope = scope.as_str().to_string();
        // A supplied cursor is only valid against an identical normalized query.
        let cursor_ctx = match &req.cursor {
            Some(c) => Some(
                c.query
                    .clone()
                    .ok_or_else(|| Error::invalid("list cursor is missing query context"))?,
            ),
            None => None,
        };
        let limit = req
            .limit
            .unwrap_or(32)
            .min(self.config.limits.max_search_limit)
            .max(1);
        // With a cursor, the effective valid-time lens is pinned to page one.
        let as_of = req
            .as_of
            .or_else(|| cursor_ctx.as_ref().map(|q| q.as_of))
            .unwrap_or_else(now);
        let as_of_s = ts_param(as_of);
        let cats: Option<Vec<String>> = req
            .categories
            .as_ref()
            .map(|c| c.iter().map(|x| x.as_str().to_string()).collect());
        let text = req
            .text
            .as_ref()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty());
        let exact = matches!(req.scope_match, ScopeMatch::Exact);
        let known_as_of = req.known_as_of;
        let sys_as_of = req.sys_as_of;

        let ctx = ListCursorQuery {
            scope: req.scope.clone(),
            scope_match: req.scope_match,
            categories: req.categories.clone(),
            as_of,
            known_as_of,
            sys_as_of,
            text: text.clone(),
        };
        if let Some(prev) = &cursor_ctx {
            if *prev != ctx {
                return Err(Error::invalid(
                    "list cursor does not match the request scope/filters/lenses",
                ));
            }
        }

        let select = if text.is_some() {
            "SELECT *, search::score(0) AS score FROM memory"
        } else {
            "SELECT * FROM memory"
        };
        let mut sql = format!(
            r#"
            LET $as_of = type::datetime($as_of);
            {select}
            WHERE valid_from <= $as_of
              AND (valid_until = NONE OR valid_until > $as_of)
              AND ($cats = NONE OR category IN $cats)
                      AND ($known_as_of = NONE OR known_at <= type::datetime($known_as_of))
                      AND ($sys_as_of = NONE OR (
                          (IF sys_created = NONE THEN known_at ELSE sys_created END) <= type::datetime($sys_as_of)
                          AND (sys_closed = NONE OR sys_closed > type::datetime($sys_as_of))
                      ))
            "#
        );
        if exact {
            sql.push_str(" AND scope = $scope ");
        } else {
            sql.push_str(" AND (scope = $scope OR string::starts_with(scope, $scope_prefix)) ");
        }
        if text.is_some() {
            sql.push_str(" AND content.text @0@ $qtext ");
        }
        if req.cursor.is_some() {
            sql.push_str(
                " AND (valid_from < type::datetime($cur_vf) OR (valid_from = type::datetime($cur_vf) AND id < type::record('memory', $cur_id))) ",
            );
        }
        // Stable keyset order (valid_from, id) even on the text path; `score`
        // is output only, never the pagination order.
        sql.push_str(" ORDER BY valid_from DESC, id DESC ");
        sql.push_str(" LIMIT $limit; ");

        let mut q = self
            .db
            .query(sql)
            .bind(("as_of", as_of_s))
            .bind(("scope", req.scope.clone()))
            .bind(("scope_prefix", format!("{}/", req.scope)))
            .bind(("cats", cats))
            .bind(("limit", (limit as i64) + 1))
            .bind(("known_as_of", known_as_of.map(ts_param)))
            .bind(("sys_as_of", sys_as_of.map(ts_param)));
        if let Some(t) = &text {
            q = q.bind(("qtext", t.clone()));
        }
        if let Some(cur) = &req.cursor {
            q = q
                .bind(("cur_vf", ts_param(cur.valid_from)))
                .bind(("cur_id", cur.id.bare_key().to_string()));
        }
        let mut response = q.await.map_err(|e| Error::store(format!("list: {e}")))?;
        ensure_ok(&mut response)?;
        let rows =
            take_json_rows(&mut response, 1).or_else(|_| take_json_rows(&mut response, 0))?;
        let mut items = Vec::new();
        for v in rows {
            let score = v.get("score").and_then(|s| s.as_f64());
            let mrow = value_to_memory_row(v)?;
            if !Self::scope_allowed(&mrow.scope, &scope, req.scope_match) {
                continue;
            }
            let rec = row_to_record(mrow)?;
            if let Some(k) = known_as_of {
                if rec.known_at > k {
                    continue;
                }
            }
            if let Some(u) = sys_as_of {
                let created = rec.sys_created.unwrap_or(rec.known_at);
                if created > u {
                    continue;
                }
                if let Some(closed) = rec.sys_closed {
                    if closed <= u {
                        continue;
                    }
                }
            }
            let (score, score_kind) = match score {
                Some(s) if s.is_finite() && s != 0.0 => (Some(s), Some(ScoreKind::Engine)),
                Some(s) if s.is_finite() && text.is_some() => (Some(s), Some(ScoreKind::Engine)),
                _ if text.is_some() => (None, None),
                _ => (None, None),
            };
            items.push(ListItem {
                record: rec,
                score,
                score_kind,
            });
        }
        let mut next_cursor = None;
        if items.len() as u32 > limit {
            items.truncate(limit as usize);
            if let Some(last) = items.last() {
                next_cursor = Some(ListCursor {
                    valid_from: last.record.valid_from,
                    id: last.record.id.clone(),
                    query: Some(ctx),
                });
            }
        }
        Ok(ListResponse { items, next_cursor })
    }

    async fn count(&self, req: nomiso_core::list::CountRequest) -> Result<u64> {
        // Page through list with keyset cursor so count is never capped at max_search_limit.
        // (Surreal SELECT count() shape varies by version; pagination is correct and tested.)
        self.count_with_page_limit(req, 10_000).await
    }

    async fn table_counts(&self) -> Result<std::collections::BTreeMap<String, u64>> {
        let mut out = std::collections::BTreeMap::new();
        for t in FRONTIER_TABLES {
            // Table names come from the constant whitelist — never user input.
            let mut response = self
                .db
                .query(format!("SELECT count() AS n FROM {t} GROUP ALL;"))
                .await
                .map_err(|e| Error::store(format!("table_counts {t}: {e}")))?;
            ensure_ok(&mut response)?;
            let rows = take_json_rows(&mut response, 0)?;
            let n = rows
                .first()
                .and_then(|v| v.get("n"))
                .and_then(|v| v.as_u64().or_else(|| v.as_i64().map(|x| x.max(0) as u64)))
                .unwrap_or(0);
            out.insert((*t).to_string(), n);
        }
        Ok(out)
    }

    async fn put_artifact(
        &self,
        req: nomiso_core::evidence::PutArtifactRequest,
    ) -> Result<nomiso_core::evidence::ArtifactRecord> {
        let mut req = req;
        req.scope = nomiso_core::scope::ScopePath::parse(&req.scope)?
            .as_str()
            .to_string();
        if req.blake3.len() != 64 || !req.blake3.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(Error::invalid("blake3 must be 64 hex chars"));
        }
        if req.location.trim().is_empty() {
            return Err(Error::invalid("location required"));
        }
        let hash = req.blake3.to_ascii_lowercase();
        // Dedup within (scope, blake3) only — never return another scope's metadata.
        let mut response = self
            .db
            .query("SELECT * FROM artifact WHERE scope = $scope AND blake3 = $h LIMIT 1;")
            .bind(("scope", req.scope.clone()))
            .bind(("h", hash.clone()))
            .await
            .map_err(|e| Error::store(e.to_string()))?;
        ensure_ok(&mut response)?;
        if let Some(v) = take_json_rows(&mut response, 0)?.into_iter().next() {
            return decode_artifact(v);
        }
        let key = Uuid::now_v7().to_string();
        let created = now();
        let mut response = self
            .db
            .query(
                r#"
                CREATE type::record('artifact', $key) SET
                    scope = $scope,
                    blake3 = $blake3,
                    location = $location,
                    media_type = $media_type,
                    source = $source,
                    trust = $trust,
                    created_at = type::datetime($created)
                RETURN AFTER;
                "#,
            )
            .bind(("key", key))
            .bind(("scope", req.scope.clone()))
            .bind(("blake3", hash.clone()))
            .bind(("location", req.location))
            .bind(("media_type", req.media_type))
            .bind(("source", req.source))
            .bind(("trust", req.trust))
            .bind(("created", ts_param(created)))
            .await
            .map_err(|e| Error::store(e.to_string()))?;
        if let Err(e) = ensure_ok(&mut response) {
            // Unique (scope, blake3) race: replay same-scope row only.
            if is_conflict_store_err(&e) || e.to_string().to_lowercase().contains("unique") {
                let mut again = self
                    .db
                    .query("SELECT * FROM artifact WHERE scope = $scope AND blake3 = $h LIMIT 1;")
                    .bind(("scope", req.scope))
                    .bind(("h", hash))
                    .await
                    .map_err(|e| Error::store(e.to_string()))?;
                ensure_ok(&mut again)?;
                if let Some(v) = take_json_rows(&mut again, 0)?.into_iter().next() {
                    return decode_artifact(v);
                }
            }
            return Err(e);
        }
        let v = take_json_rows(&mut response, 0)?
            .into_iter()
            .next()
            .ok_or_else(|| Error::store("put_artifact empty"))?;
        decode_artifact(v)
    }

    async fn list_artifacts(
        &self,
        scope: Option<&str>,
    ) -> Result<Vec<nomiso_core::evidence::ArtifactRecord>> {
        let mut response = if let Some(scope) = scope {
            let scope = nomiso_core::scope::ScopePath::parse(scope)?
                .as_str()
                .to_string();
            self.db
                .query("SELECT * FROM artifact WHERE scope = $scope ORDER BY id;")
                .bind(("scope", scope))
                .await
        } else {
            self.db.query("SELECT * FROM artifact ORDER BY id;").await
        }
        .map_err(|e| Error::store(e.to_string()))?;
        ensure_ok(&mut response)?;
        take_json_rows(&mut response, 0)?
            .into_iter()
            .map(decode_artifact)
            .collect()
    }

    async fn rebind_artifact_location(
        &self,
        id: &str,
        expected_location: &str,
        new_location: &str,
    ) -> Result<bool> {
        let key = artifact_bare_key(id);
        if new_location.trim().is_empty() {
            return Err(Error::invalid("rebind_artifact_location: empty location"));
        }
        // Guarded single-statement update: the row is rewritten only while it
        // still points at the location we verified — a concurrent rebind or
        // stale read yields zero rows instead of a blind overwrite.
        let mut response = self
            .db
            .query(
                "UPDATE type::record('artifact', $key) SET location = $new \
                 WHERE location = $expected RETURN AFTER;",
            )
            .bind(("key", key))
            .bind(("new", new_location.to_string()))
            .bind(("expected", expected_location.to_string()))
            .await
            .map_err(|e| Error::store(e.to_string()))?;
        ensure_ok(&mut response)?;
        Ok(!take_json_rows(&mut response, 0)?.is_empty())
    }

    async fn put_span(
        &self,
        req: nomiso_core::evidence::PutSpanRequest,
    ) -> Result<nomiso_core::evidence::SpanRecord> {
        use nomiso_core::evidence::SpanRecord;
        let mut req = req;
        req.scope = nomiso_core::scope::ScopePath::parse(&req.scope)?
            .as_str()
            .to_string();
        if req.end < req.start {
            return Err(Error::invalid("span end < start"));
        }
        let art_key = artifact_bare_key(&req.artifact_id);
        // E1: refuse dangling / cross-scope artifact ids.
        let mut art_q = self
            .db
            .query("SELECT * FROM type::record('artifact', $art);")
            .bind(("art", art_key.clone()))
            .await
            .map_err(|e| Error::store(e.to_string()))?;
        ensure_ok(&mut art_q)?;
        let art_row = take_json_rows(&mut art_q, 0)?
            .into_iter()
            .next()
            .ok_or_else(|| Error::NotFound(format!("artifact:{}", art_key)))?;
        let art_scope = art_row.get("scope").and_then(|x| x.as_str()).unwrap_or("");
        if art_scope != req.scope {
            return Err(Error::ScopeDenied("put_span artifact scope".into()));
        }
        let key = Uuid::now_v7().to_string();
        let created = now();
        let mut response = self
            .db
            .query(
                r#"
                CREATE type::record('span', $key) SET
                    artifact_id = type::record('artifact', $art),
                    scope = $scope,
                    start = $start,
                    end = $end,
                    unit = $unit,
                    created_at = type::datetime($created)
                RETURN AFTER;
                "#,
            )
            .bind(("key", key.clone()))
            .bind(("art", art_key.clone()))
            .bind(("scope", req.scope.clone()))
            .bind(("start", req.start as i64))
            .bind(("end", req.end as i64))
            .bind(("unit", req.unit.as_str().to_string()))
            .bind(("created", ts_param(created)))
            .await
            .map_err(|e| Error::store(e.to_string()))?;
        ensure_ok(&mut response)?;
        Ok(SpanRecord {
            id: format!("span:{key}"),
            artifact_id: format!("artifact:{art_key}"),
            scope: req.scope,
            start: req.start,
            end: req.end,
            unit: req.unit,
            created_at: created,
        })
    }

    async fn link_derived_from(
        &self,
        req: nomiso_core::evidence::LinkDerivedFromRequest,
    ) -> Result<()> {
        use nomiso_core::evidence::DerivedFromTarget;
        let req_scope = nomiso_core::scope::ScopePath::parse(&req.scope)?;
        let mem_key = req.memory_id.bare_key().to_string();
        // Scope check
        let row = self
            .fetch_memory_raw(&req.memory_id)
            .await?
            .ok_or_else(|| Error::NotFound(req.memory_id.to_string()))?;
        if row.scope != req_scope.as_str() {
            return Err(Error::ScopeDenied("link_derived_from scope".into()));
        }
        let (edge, tkey) = match &req.target {
            DerivedFromTarget::Artifact { id } => {
                let k = id.strip_prefix("artifact:").unwrap_or(id).to_string();
                ("derived_from_artifact", k)
            }
            DerivedFromTarget::Span { id } => {
                let k = id.strip_prefix("span:").unwrap_or(id).to_string();
                ("derived_from_span", k)
            }
            DerivedFromTarget::Memory { id } => {
                let k = id.bare_key().to_string();
                ("derived_from_edge", k)
            }
        };
        // RELATE cannot use type::record() inline (parse); bind via LET.
        let sql = format!(
            r#"
            LET $m = type::record('memory', $mem);
            LET $t = type::record($ttable, $tkey);
            RELATE $m -> {edge} -> $t;
            "#
        );
        let ttable = match &req.target {
            DerivedFromTarget::Artifact { .. } => "artifact",
            DerivedFromTarget::Span { .. } => "span",
            DerivedFromTarget::Memory { .. } => "memory",
        };
        let mut response = self
            .db
            .query(sql)
            .bind(("mem", mem_key))
            .bind(("tkey", tkey))
            .bind(("ttable", ttable.to_string()))
            .await
            .map_err(|e| Error::store(e.to_string()))?;
        ensure_ok(&mut response)?;
        Ok(())
    }

    async fn history(
        &self,
        id: &MemoryId,
        scope: &str,
    ) -> Result<Vec<nomiso_core::evidence::HistoryEntry>> {
        use nomiso_core::evidence::HistoryEntry;
        let scope = nomiso_core::scope::ScopePath::parse(scope)?;
        let scope = scope.as_str();
        let mut chain = Vec::new();
        let mut cur = id.clone();
        // Walk supersedes pointers backward then reverse
        for _ in 0..64 {
            let Some(row) = self.fetch_memory_raw(&cur).await? else {
                break;
            };
            if row.scope != scope {
                return Err(Error::ScopeDenied("history scope".into()));
            }
            let rec = row_to_record(row)?;
            let prior = rec.supersedes.clone();
            chain.push(HistoryEntry { record: rec });
            match prior {
                Some(p) => cur = p,
                None => break,
            }
        }
        chain.reverse();
        // Also walk forward from original id via superseded_by
        let mut forward = Vec::new();
        let mut cur = id.clone();
        for _ in 0..64 {
            let Some(row) = self.fetch_memory_raw(&cur).await? else {
                break;
            };
            let rec = row_to_record(row)?;
            let next = rec.superseded_by.clone();
            if rec.id.as_str() != id.as_str() {
                forward.push(HistoryEntry { record: rec });
            }
            match next {
                Some(n) => cur = n,
                None => break,
            }
        }
        chain.extend(forward);
        Ok(chain)
    }

    async fn annotate(&self, req: nomiso_core::evidence::AnnotateRequest) -> Result<WriteResult> {
        let mut req = req;
        req.scope = nomiso_core::scope::ScopePath::parse(&req.scope)?
            .as_str()
            .to_string();
        let row = self
            .fetch_memory_raw(&req.id)
            .await?
            .ok_or_else(|| Error::NotFound(req.id.to_string()))?;
        if row.scope != req.scope {
            return Err(Error::ScopeDenied("annotate scope".into()));
        }
        if let Some(ev) = req.expected_version {
            check_version(ev, row.version)?;
        }
        let rec0 = row_to_record(row)?;
        let merged_attrs = match (rec0.content.attrs.clone(), req.attrs.clone()) {
            (existing, None) => existing,
            (None, Some(a)) => Some(a),
            (Some(Value::Object(mut a)), Some(Value::Object(b))) => {
                for (k, v) in b {
                    a.insert(k, v);
                }
                Some(Value::Object(a))
            }
            (_, Some(a)) => Some(a),
        };
        if let Some(ref attrs) = merged_attrs {
            let n = attrs.to_string().len();
            if n > self.config.limits.max_attrs_bytes {
                return Err(Error::PayloadTooLarge(format!(
                    "content.attrs is {n} bytes (max {})",
                    self.config.limits.max_attrs_bytes
                )));
            }
        }
        let key = req.id.bare_key().to_string();
        let bump = req.bump_version;
        let mut response = self
            .db
            .query(
                r#"
                BEGIN TRANSACTION;
                LET $updated = (
                    UPDATE type::record('memory', $key) SET
                        confidence = IF $confidence = NONE THEN confidence ELSE $confidence END,
                        provenance.source = IF $psource = NONE THEN provenance.source ELSE $psource END,
                        provenance.kind = IF $pkind = NONE THEN provenance.kind ELSE $pkind END,
                        content.attrs = IF $attrs = NONE THEN content.attrs ELSE $attrs END,
                        version = IF $bump THEN version + 1 ELSE version END,
                        sys_updated = time::now()
                    WHERE version = $expected OR $expected = NONE
                    RETURN AFTER
                );
                IF array::len($updated) = 0 {
                    THROW 'nomiso_conflict';
                };
                LET $uv = $updated[0].version;
                CREATE type::record('belief_event', $eid) SET
                    scope = $scope,
                    memory_id = $mid,
                    kind = 'annotate',
                    at_sys = time::now(),
                    payload = { version: $uv, bump: $bump };
                RETURN $updated;
                COMMIT TRANSACTION;
                "#,
            )
            .bind(("key", key))
            .bind(("eid", Uuid::now_v7().to_string()))
            .bind(("mid", req.id.to_string()))
            .bind(("scope", req.scope.clone()))
            .bind(("confidence", req.confidence))
            .bind(("psource", req.provenance_source))
            .bind(("pkind", req.provenance_kind))
            .bind(("attrs", merged_attrs))
            .bind(("expected", req.expected_version.map(|v| v as i64)))
            .bind(("bump", bump))
            .await
            .map_err(|e| Error::store(e.to_string()))?;
        match ensure_ok(&mut response) {
            Ok(()) => {}
            Err(e) if is_conflict_store_err(&e) => {
                return Err(self
                    .conflict_from_memory(&req.id, req.expected_version.unwrap_or(rec0.version))
                    .await);
            }
            Err(e) => return Err(e),
        }
        // Find the updated memory row among the transaction's statement results.
        let mut rec_opt = None;
        for i in 0..8 {
            let Ok(rows) = take_json_rows(&mut response, i) else {
                continue;
            };
            for v in rows {
                let objs = match v {
                    Value::Array(a) => a,
                    other => vec![other],
                };
                for c in objs {
                    if let Ok(row) = value_to_memory_row(c) {
                        if let Ok(r) = row_to_record(row) {
                            rec_opt = Some(r);
                        }
                    }
                }
            }
        }
        let rec =
            rec_opt.ok_or_else(|| Error::store("annotate transaction returned no updated row"))?;
        Ok(WriteResult {
            replayed: false,
            id: rec.id.clone(),
            version: rec.version,
            record: Some(rec),
        })
    }

    async fn put_task_state(
        &self,
        req: nomiso_core::task_state::PutTaskStateRequest,
    ) -> Result<nomiso_core::task_state::TaskStateRecord> {
        let mut req = req;
        req.scope = ScopePath::parse(&req.scope)?.as_str().to_string();
        let slot = req.slot.trim().to_string();
        if slot.is_empty() {
            return Err(Error::invalid("slot required"));
        }
        if slot.len() > 128 {
            return Err(Error::invalid("slot exceeds 128 bytes"));
        }
        if req.create_only && req.expected_version.is_some() {
            return Err(Error::invalid(
                "create_only cannot be combined with expected_version",
            ));
        }
        if req.expected_version == Some(0) {
            return Err(Error::invalid("expected_version must be >= 1"));
        }
        if !req.body.is_object() {
            return Err(Error::invalid("task_state body must be a JSON object"));
        }
        let body_bytes = req.body.to_string().len();
        if body_bytes > self.config.limits.max_content_bytes {
            return Err(Error::PayloadTooLarge(format!(
                "task_state body is {body_bytes} bytes (max {})",
                self.config.limits.max_content_bytes
            )));
        }
        for attempt in 0..3 {
            // Atomic CAS: no pre-SELECT. Empty RETURN + expected set => Conflict.
            let mut response = match self
                .db
                .query(
                    r#"
                    UPDATE task_state SET
                        body = $body,
                        version = version + 1,
                        sys_updated = time::now()
                    WHERE scope = $scope AND slot = $slot
                      AND ($expected = NONE OR version = $expected)
                      AND $create_only = false
                    RETURN AFTER;
                    "#,
                )
                .bind(("scope", req.scope.clone()))
                .bind(("slot", slot.clone()))
                .bind(("body", req.body.clone()))
                .bind(("expected", req.expected_version.map(|v| v as i64)))
                .bind(("create_only", req.create_only))
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    let err = Error::store(e.to_string());
                    if is_kv_write_conflict(&err) && attempt < 2 {
                        continue;
                    }
                    if is_kv_write_conflict(&err) {
                        return Err(self
                            .task_state_conflict(&req.scope, &slot, req.expected_version)
                            .await);
                    }
                    return Err(err);
                }
            };
            if let Err(e) = ensure_ok(&mut response) {
                if is_kv_write_conflict(&e) && attempt < 2 {
                    continue;
                }
                if is_kv_write_conflict(&e) {
                    return Err(self
                        .task_state_conflict(&req.scope, &slot, req.expected_version)
                        .await);
                }
                return Err(e);
            }
            if let Some(row) = take_json_rows(&mut response, 0)?.into_iter().next() {
                return decode_task_state(row);
            }
            if req.expected_version.is_some() {
                return Err(self
                    .task_state_conflict(&req.scope, &slot, req.expected_version)
                    .await);
            }
            let key = Uuid::now_v7().to_string();
            let mut response = match self
                .db
                .query(
                    r#"
                CREATE type::record('task_state', $key) SET
                    scope = $scope,
                    slot = $slot,
                    body = $body,
                    version = 1,
                    sys_created = time::now(),
                    sys_updated = time::now()
                RETURN AFTER;
                "#,
                )
                .bind(("key", key))
                .bind(("scope", req.scope.clone()))
                .bind(("slot", slot.clone()))
                .bind(("body", req.body.clone()))
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    let err = Error::store(e.to_string());
                    if req.create_only && is_state_create_conflict(&err) {
                        return Err(self.task_state_conflict(&req.scope, &slot, Some(0)).await);
                    }
                    return Err(err);
                }
            };
            if let Err(e) = ensure_ok(&mut response) {
                if req.create_only && is_state_create_conflict(&e) {
                    return Err(self.task_state_conflict(&req.scope, &slot, Some(0)).await);
                }
                if is_state_create_conflict(&e) && attempt < 2 {
                    continue;
                }
                if is_kv_write_conflict(&e) {
                    return Err(self
                        .task_state_conflict(&req.scope, &slot, req.expected_version)
                        .await);
                }
                return Err(e);
            }
            let row = take_json_rows(&mut response, 0)?
                .into_iter()
                .next()
                .ok_or_else(|| Error::store("put_task_state create empty"))?;
            return decode_task_state(row);
        }
        Err(self
            .task_state_conflict(&req.scope, &slot, req.expected_version)
            .await)
    }

    async fn get_task_state(
        &self,
        req: nomiso_core::task_state::GetTaskStateRequest,
    ) -> Result<Option<nomiso_core::task_state::TaskStateRecord>> {
        let scope = ScopePath::parse(&req.scope)?;
        let mut response = self
            .db
            .query("SELECT * FROM task_state WHERE scope = $scope AND slot = $slot LIMIT 1;")
            .bind(("scope", scope.as_str().to_string()))
            .bind(("slot", req.slot.trim().to_string()))
            .await
            .map_err(|e| Error::store(e.to_string()))?;
        ensure_ok(&mut response)?;
        let rows = take_json_rows(&mut response, 0)?;
        if let Some(v) = rows.into_iter().next() {
            Ok(Some(decode_task_state(v)?))
        } else {
            Ok(None)
        }
    }

    async fn list_belief_events(
        &self,
        memory_id: &MemoryId,
        scope: &str,
    ) -> Result<Vec<nomiso_core::belief_event::BeliefEvent>> {
        use nomiso_core::belief_event::{BeliefEvent, BeliefEventKind};
        let scope = ScopePath::parse(scope)?;
        let scope = scope.as_str();
        let mid = memory_id.to_string();
        let mut response = self
            .db
            .query(
                r#"
                SELECT * FROM belief_event
                WHERE scope = $scope AND (memory_id = $mid OR memory_id = $mid2)
                ORDER BY at_sys ASC;
                "#,
            )
            .bind(("scope", scope.to_string()))
            .bind(("mid", mid.clone()))
            .bind(("mid2", format!("memory:{}", memory_id.bare_key())))
            .await
            .map_err(|e| Error::store(e.to_string()))?;
        ensure_ok(&mut response)?;
        let rows = take_json_rows(&mut response, 0)?;
        let mut out = Vec::with_capacity(rows.len());
        for v in rows {
            let id = v
                .get("id")
                .map(|x| x.to_string().trim_matches('"').to_string())
                .unwrap_or_default();
            let kind_s = v.get("kind").and_then(|k| k.as_str()).unwrap_or("");
            let Some(kind) = BeliefEventKind::parse(kind_s) else {
                // Skip corrupt/unknown kinds rather than mislabel as assert.
                continue;
            };
            let at_sys = match v.get("at_sys") {
                Some(t) => crate::mapping::parse_timestamp(t)?,
                None => now(),
            };
            let mem = v
                .get("memory_id")
                .map(|x| {
                    let s = x.to_string().trim_matches('"').to_string();
                    MemoryId::new(s)
                })
                .unwrap_or_else(|| memory_id.clone());
            out.push(BeliefEvent {
                id,
                scope: v
                    .get("scope")
                    .and_then(|s| s.as_str())
                    .unwrap_or(scope)
                    .to_string(),
                memory_id: mem,
                kind,
                at_sys,
                payload: v.get("payload").cloned(),
            });
        }
        Ok(out)
    }

    async fn put_entity(
        &self,
        req: nomiso_core::relationship::PutEntityRequest,
    ) -> Result<nomiso_core::relationship::EntityRecord> {
        let scope = ScopePath::parse(&req.scope)?.as_str().to_string();
        let kind = req.kind.trim();
        let name = req.name.trim();
        if kind.is_empty() || kind.len() > 128 {
            return Err(Error::invalid("entity kind must be 1..=128 chars"));
        }
        if name.is_empty() || name.len() > 256 {
            return Err(Error::invalid("entity name must be 1..=256 chars"));
        }
        if req.aliases.len() > 64 {
            return Err(Error::invalid("entity aliases limited to 64"));
        }
        let key = Uuid::now_v7().to_string();
        let mut response = self
            .db
            .query(
                r#"
                CREATE type::record('entity', $key) SET
                    scope = $scope,
                    kind = $kind,
                    name = $name,
                    aliases = $aliases,
                    attrs = $attrs,
                    version = 1,
                    created_at = time::now(),
                    updated_at = time::now()
                RETURN AFTER;
                "#,
            )
            .bind(("key", key.clone()))
            .bind(("scope", scope))
            .bind(("kind", kind.to_string()))
            .bind(("name", name.to_string()))
            .bind(("aliases", req.aliases.clone()))
            .bind(("attrs", req.attrs.clone()))
            .await
            .map_err(|e| Error::store(format!("put_entity: {e}")))?;
        ensure_ok(&mut response)?;
        let rows = take_json_rows(&mut response, 0)?;
        rows.into_iter()
            .next()
            .and_then(|v| decode_entity(&v).ok())
            .ok_or_else(|| Error::store("put_entity returned no row"))
    }

    async fn get_entity(
        &self,
        id: &str,
        scope: &str,
    ) -> Result<nomiso_core::relationship::EntityRecord> {
        let scope = ScopePath::parse(scope)?.as_str().to_string();
        let key = entity_bare_key(id);
        let mut response = self
            .db
            .query("SELECT * FROM type::record('entity', $key);")
            .bind(("key", key))
            .await
            .map_err(|e| Error::store(format!("get_entity: {e}")))?;
        ensure_ok(&mut response)?;
        let rows = take_json_rows(&mut response, 0)?;
        let rec = rows
            .into_iter()
            .next()
            .map(|v| decode_entity(&v))
            .transpose()?
            .ok_or_else(|| Error::NotFound(format!("entity:{id}")))?;
        if rec.scope != scope {
            return Err(Error::ScopeDenied("entity scope".into()));
        }
        Ok(rec)
    }

    async fn update_entity(
        &self,
        req: nomiso_core::relationship::UpdateEntityRequest,
    ) -> Result<nomiso_core::relationship::EntityRecord> {
        let scope = ScopePath::parse(&req.scope)?.as_str().to_string();
        let existing = self.get_entity(&req.id, &scope).await?;
        let key = entity_bare_key(&req.id);
        let mut response = self
            .db
            .query(
                r#"
                BEGIN TRANSACTION;
                LET $updated = (
                    UPDATE type::record('entity', $key) SET
                        name = IF $name = NONE THEN name ELSE $name END,
                        aliases = IF $aliases = NONE THEN aliases ELSE $aliases END,
                        attrs = IF $attrs = NONE THEN attrs ELSE $attrs END,
                        version = version + 1,
                        updated_at = time::now()
                    WHERE version = $expected_version
                    RETURN AFTER
                );
                IF array::len($updated) = 0 { THROW 'nomiso_conflict'; };
                RETURN $updated;
                COMMIT TRANSACTION;
                "#,
            )
            .bind(("key", key))
            .bind(("expected_version", req.expected_version as i64))
            .bind(("name", req.name.clone()))
            .bind(("aliases", req.aliases.clone()))
            .bind(("attrs", req.attrs.clone()))
            .await
            .map_err(|e| Error::store(format!("update_entity: {e}")))?;
        if let Err(e) = ensure_ok(&mut response) {
            if is_conflict_store_err(&e) {
                return Err(Error::Conflict {
                    expected: req.expected_version,
                    found: existing.version,
                });
            }
            return Err(e);
        }
        for i in 0..8 {
            let Ok(rows) = take_json_rows(&mut response, i) else {
                continue;
            };
            for v in rows {
                let objs = match v {
                    Value::Array(a) => a,
                    other => vec![other],
                };
                for c in objs {
                    if c.get("kind").is_some() && c.get("scope").is_some() {
                        if let Ok(rec) = decode_entity(&c) {
                            if rec.version == existing.version + 1 {
                                return Ok(rec);
                            }
                        }
                    }
                }
            }
        }
        Err(Error::store("update_entity returned no row"))
    }

    async fn put_relationship(
        &self,
        req: nomiso_core::relationship::PutRelationshipRequest,
    ) -> Result<nomiso_core::relationship::RelationshipWrite> {
        use nomiso_core::relationship::{
            EndpointRef, RelationshipWrite, RELATION_REGISTRY_VERSION,
        };
        let scope = ScopePath::parse(&req.scope)?.as_str().to_string();
        // REL-001: registry validation before any store interaction.
        req.predicate
            .check_endpoints(req.subject.kind, req.object.kind)?;
        let subject_key = req.subject.bare_key()?;
        let object_key = req.object.bare_key()?;
        for (kind, rev, label) in [
            (req.subject.kind, req.subject_rev, "subject_rev"),
            (req.object.kind, req.object_rev, "object_rev"),
        ] {
            if let Some(r) = rev {
                if r == 0 {
                    return Err(Error::invalid(format!("{label} must be >= 1")));
                }
                if !kind.versioned() {
                    return Err(Error::invalid(format!(
                        "{label} pins a revision on unversioned kind {}",
                        kind.as_str()
                    )));
                }
            }
        }
        // Endpoint existence and same owning scope.
        let sv = self
            .check_endpoint(req.subject.kind, &subject_key, &scope)
            .await?;
        let ov = self
            .check_endpoint(req.object.kind, &object_key, &scope)
            .await?;
        if let (Some(pin), Some(cur)) = (req.subject_rev, sv) {
            if pin > cur {
                return Err(Error::invalid(
                    "subject_rev pins a revision beyond the endpoint's current version",
                ));
            }
        }
        if let (Some(pin), Some(cur)) = (req.object_rev, ov) {
            if pin > cur {
                return Err(Error::invalid(
                    "object_rev pins a revision beyond the endpoint's current version",
                ));
            }
        }
        // Evidence references must also resolve inside the scope.
        let mut evidence = Vec::with_capacity(req.evidence.len());
        for ev in &req.evidence {
            let ekey = EndpointRef::new(ev.kind, ev.id.clone()).bare_key()?;
            let cur = self.check_endpoint(ev.kind, &ekey, &scope).await?;
            if let Some(pin) = ev.revision {
                if pin == 0 {
                    return Err(Error::invalid("evidence revision must be >= 1"));
                }
                if !ev.kind.versioned() {
                    return Err(Error::invalid(
                        "evidence revision pinned on an unversioned kind",
                    ));
                }
                if let Some(c) = cur {
                    if pin > c {
                        return Err(Error::invalid(
                            "evidence revision beyond the endpoint's current version",
                        ));
                    }
                }
            }
            evidence.push(json!({
                "kind": ev.kind.as_str(),
                "id": ekey,
                "revision": ev.revision,
            }));
        }
        let live_key = format!(
            "A|{scope}|{}|{}:{subject_key}|{}|{}:{object_key}|{}",
            req.predicate.as_str(),
            req.subject.kind.as_str(),
            req.subject_rev
                .map(|r| r.to_string())
                .unwrap_or_else(|| "-".into()),
            req.object.kind.as_str(),
            req.object_rev
                .map(|r| r.to_string())
                .unwrap_or_else(|| "-".into()),
        );
        // Dedupe pre-read: an identical active edge is a replay; an identical
        // live key with different payload conflicts (REL-002).
        if let Some(existing) = self.relationship_by_live_key(&live_key).await? {
            if req.dedupe && relationship_request_matches(&req, &existing) {
                return Ok(RelationshipWrite {
                    record: existing,
                    replayed: true,
                });
            }
            return Err(Error::IdempotencyConflict);
        }
        let key = Uuid::now_v7().to_string();
        let eid = Uuid::now_v7().to_string();
        let valid_from_ts = req.valid_from.unwrap_or_else(now);
        if let Some(vu) = req.valid_until {
            if vu <= valid_from_ts {
                return Err(Error::invalid(
                    "relationship valid_until must be after valid_from",
                ));
            }
        }
        let mut response = self
            .db
            .query(
                r#"
                BEGIN TRANSACTION;
                LET $created = (
                    CREATE type::record('relationship', $key) SET
                        scope = $scope,
                        predicate = $predicate,
                        predicate_version = $pred_ver,
                        subject_kind = $subject_kind,
                        subject_id = $subject_id,
                        object_kind = $object_kind,
                        object_id = $object_id,
                        subject_rev = $subject_rev,
                        object_rev = $object_rev,
                        epistemic = $epistemic,
                        evidence = $evidence,
                        state = 'active',
                        state_reason = NONE,
                        valid_from = type::datetime($valid_from),
                        valid_until = IF $valid_until = NONE THEN NONE ELSE type::datetime($valid_until) END,
                        producer = $producer,
                        version = 1,
                        live_key = $live_key,
                        created_at = time::now(),
                        updated_at = time::now()
                    RETURN AFTER
                );
                CREATE type::record('relationship_event', $eid) SET
                    relationship_id = $rid,
                    scope = $scope,
                    kind = 'assert',
                    at_sys = time::now(),
                    payload = { version: 1 };
                RETURN $created;
                COMMIT TRANSACTION;
                "#,
            )
            .bind(("key", key.clone()))
            .bind(("eid", eid))
            .bind(("rid", format!("relationship:{key}")))
            .bind(("scope", scope.clone()))
            .bind(("predicate", req.predicate.as_str().to_string()))
            .bind(("pred_ver", RELATION_REGISTRY_VERSION as i64))
            .bind(("subject_kind", req.subject.kind.as_str().to_string()))
            .bind(("subject_id", subject_key.clone()))
            .bind(("object_kind", req.object.kind.as_str().to_string()))
            .bind(("object_id", object_key.clone()))
            .bind(("subject_rev", req.subject_rev.map(|r| r as i64)))
            .bind(("object_rev", req.object_rev.map(|r| r as i64)))
            .bind(("epistemic", req.epistemic.as_str().to_string()))
            .bind(("evidence", evidence))
            .bind(("valid_from", ts_param(valid_from_ts)))
            .bind(("valid_until", req.valid_until.map(ts_param)))
            .bind(("producer", req.producer.as_ref().map(|p| json!(p))))
            .bind(("live_key", live_key.clone()))
            .await
            .map_err(|e| Error::store(format!("put_relationship: {e}")))?;
        if let Err(e) = ensure_ok(&mut response) {
            // A concurrent identical create claims the live key first; the
            // unique index makes that the single winner.
            if is_state_create_conflict(&e) {
                tracing::debug!(error = %e, "put_relationship live-key conflict");
                if let Some(existing) = self.relationship_by_live_key(&live_key).await? {
                    if req.dedupe && relationship_request_matches(&req, &existing) {
                        return Ok(RelationshipWrite {
                            record: existing,
                            replayed: true,
                        });
                    }
                }
                return Err(Error::IdempotencyConflict);
            }
            return Err(e);
        }
        for i in 0..8 {
            let Ok(rows) = take_json_rows(&mut response, i) else {
                continue;
            };
            for v in rows {
                let objs = match v {
                    Value::Array(a) => a,
                    other => vec![other],
                };
                for c in objs {
                    if c.get("live_key").is_some() {
                        if let Ok(rec) = decode_relationship(&c) {
                            return Ok(RelationshipWrite {
                                record: rec,
                                replayed: false,
                            });
                        }
                    }
                }
            }
        }
        Err(Error::store("put_relationship returned no created row"))
    }

    async fn get_relationship(
        &self,
        id: &str,
        scope: &str,
    ) -> Result<nomiso_core::relationship::RelationshipRecord> {
        let scope = ScopePath::parse(scope)?.as_str().to_string();
        let key = rel_bare_key(id);
        let mut response = self
            .db
            .query("SELECT * FROM type::record('relationship', $key);")
            .bind(("key", key))
            .await
            .map_err(|e| Error::store(format!("get_relationship: {e}")))?;
        ensure_ok(&mut response)?;
        let rows = take_json_rows(&mut response, 0)?;
        let rec = rows
            .into_iter()
            .next()
            .map(|v| decode_relationship(&v))
            .transpose()?
            .ok_or_else(|| Error::NotFound(format!("relationship:{id}")))?;
        if rec.scope != scope {
            return Err(Error::ScopeDenied("relationship scope".into()));
        }
        Ok(rec)
    }

    async fn update_relationship(
        &self,
        req: nomiso_core::relationship::UpdateRelationshipRequest,
    ) -> Result<nomiso_core::relationship::RelationshipRecord> {
        use nomiso_core::relationship::RelationshipState;
        let scope = ScopePath::parse(&req.scope)?.as_str().to_string();
        let existing = self.get_relationship(&req.id, &scope).await?;
        if let Some(st) = req.state {
            match st {
                RelationshipState::Purged => {
                    return Err(Error::invalid(
                        "purged state is endpoint-erasure-driven only",
                    ));
                }
                RelationshipState::Active => {
                    return Err(Error::invalid(
                        "closed edges cannot reopen; assert a new relationship",
                    ));
                }
                RelationshipState::Closed | RelationshipState::Stale => {
                    if req
                        .state_reason
                        .as_deref()
                        .map(str::trim)
                        .is_none_or(|s| s.is_empty())
                    {
                        return Err(Error::invalid(
                            "closing or staling a relationship requires state_reason",
                        ));
                    }
                }
            }
        }
        if let Some(vu) = req.valid_until {
            if vu <= existing.valid_from {
                return Err(Error::invalid(
                    "relationship valid_until must be after valid_from",
                ));
            }
        }
        let key = rel_bare_key(&existing.id);
        let eid = Uuid::now_v7().to_string();
        let mut response = self
            .db
            .query(
                r#"
                BEGIN TRANSACTION;
                LET $prior_snap = (SELECT version, state, epistemic, evidence, valid_until
                    FROM type::record('relationship', $key))[0];
                LET $updated = (
                    UPDATE type::record('relationship', $key) SET
                        epistemic = IF $epistemic = NONE THEN epistemic ELSE $epistemic END,
                        state = IF $state = NONE THEN state ELSE $state END,
                        state_reason = IF $state = NONE THEN state_reason ELSE $state_reason END,
                        valid_until = IF $valid_until = NONE THEN valid_until ELSE type::datetime($valid_until) END,
                        evidence = IF $evidence = NONE THEN evidence ELSE $evidence END,
                        live_key = IF $state = NONE THEN live_key ELSE 'X|' + <string> id END,
                        version = version + 1,
                        updated_at = time::now()
                    WHERE version = $expected_version AND state != 'purged'
                    RETURN AFTER
                );
                IF array::len($updated) = 0 { THROW 'nomiso_conflict'; };
                CREATE type::record('relationship_event', $eid) SET
                    relationship_id = $rid,
                    scope = $scope,
                    kind = 'update',
                    at_sys = time::now(),
                    payload = { prior: $prior_snap, version: $updated[0].version };
                RETURN $updated;
                COMMIT TRANSACTION;
                "#,
            )
            .bind(("key", key))
            .bind(("eid", eid))
            .bind(("rid", existing.id.clone()))
            .bind(("scope", scope.clone()))
            .bind(("expected_version", req.expected_version as i64))
            .bind(("epistemic", req.epistemic.map(|e| e.as_str().to_string())))
            .bind(("state", req.state.map(|s| s.as_str().to_string())))
            .bind(("state_reason", req.state_reason.clone()))
            .bind(("valid_until", req.valid_until.map(ts_param)))
            .bind((
                "evidence",
                req.evidence.as_ref().map(|evs| {
                    evs.iter()
                        .map(|e| {
                            json!({
                                "kind": e.kind.as_str(),
                                "id": nomiso_core::relationship::EndpointRef::new(e.kind, e.id.clone())
                                    .bare_key()
                                    .unwrap_or_else(|_| e.id.clone()),
                                "revision": e.revision,
                            })
                        })
                        .collect::<Vec<_>>()
                }),
            ))
            .await
            .map_err(|e| Error::store(format!("update_relationship: {e}")))?;
        if let Err(e) = ensure_ok(&mut response) {
            if is_conflict_store_err(&e) {
                return Err(Error::Conflict {
                    expected: req.expected_version,
                    found: existing.version,
                });
            }
            return Err(e);
        }
        for i in 0..8 {
            let Ok(rows) = take_json_rows(&mut response, i) else {
                continue;
            };
            for v in rows {
                let objs = match v {
                    Value::Array(a) => a,
                    other => vec![other],
                };
                for c in objs {
                    if c.get("live_key").is_some() {
                        if let Ok(rec) = decode_relationship(&c) {
                            if rec.version == existing.version + 1 {
                                return Ok(rec);
                            }
                        }
                    }
                }
            }
        }
        Err(Error::store("update_relationship returned no updated row"))
    }

    async fn list_relationships(
        &self,
        req: nomiso_core::relationship::ListRelationshipsRequest,
    ) -> Result<Vec<nomiso_core::relationship::RelationshipRecord>> {
        let scope = ScopePath::parse(&req.scope)?.as_str().to_string();
        let limit = req.limit.unwrap_or(128).clamp(1, 512) as i64;
        let mut conds = vec!["scope = $scope".to_string()];
        if req.endpoint.is_some() {
            conds.push(
                "((subject_kind = $ek AND subject_id = $eid) OR (object_kind = $ek AND object_id = $eid))"
                    .to_string(),
            );
        }
        if req.predicate.is_some() {
            conds.push("predicate = $pred".into());
        }
        if req.state.is_some() {
            conds.push("state = $state".into());
        }
        let mut q = String::from("SELECT * FROM relationship WHERE ");
        q.push_str(&conds.join(" AND "));
        q.push_str(" ORDER BY created_at DESC, id DESC LIMIT $limit;");
        let mut query = self
            .db
            .query(q)
            .bind(("scope", scope))
            .bind(("limit", limit));
        if let Some(ep) = &req.endpoint {
            query = query
                .bind(("ek", ep.kind.as_str().to_string()))
                .bind(("eid", ep.bare_key()?));
        }
        if let Some(p) = req.predicate {
            query = query.bind(("pred", p.as_str().to_string()));
        }
        if let Some(s) = req.state {
            query = query.bind(("state", s.as_str().to_string()));
        }
        let mut response = query
            .await
            .map_err(|e| Error::store(format!("list_relationships: {e}")))?;
        ensure_ok(&mut response)?;
        let rows = take_json_rows(&mut response, 0)?;
        let mut out = Vec::with_capacity(rows.len());
        for v in rows {
            out.push(decode_relationship(&v)?);
        }
        Ok(out)
    }

    async fn traverse(
        &self,
        req: nomiso_core::relationship::TraverseRequest,
    ) -> Result<nomiso_core::relationship::TraverseResult> {
        use nomiso_core::relationship::{
            EndpointRef, RelationPredicate, RelationshipState, TraverseDirection, TraverseNode,
            TraverseResult, CAP_DEADLINE_MS, CAP_MAX_DEPTH, CAP_MAX_EDGES, CAP_MAX_VISITED,
            DEFAULT_DEADLINE_MS, DEFAULT_MAX_DEPTH, DEFAULT_MAX_EDGES, DEFAULT_MAX_VISITED,
        };
        use std::collections::{HashSet, VecDeque};
        use std::time::Instant;

        let scope = ScopePath::parse(&req.scope)?.as_str().to_string();
        if req.seeds.is_empty() {
            return Err(Error::invalid("traverse requires at least one seed"));
        }
        let preds: Vec<RelationPredicate> = match &req.predicates {
            Some(v) if !v.is_empty() => {
                if v.iter().any(|p| p.spec().foundation_only) {
                    return Err(Error::invalid(
                        "foundation-maintained predicates are not traversable",
                    ));
                }
                v.clone()
            }
            _ => {
                let all = [
                    RelationPredicate::Supports,
                    RelationPredicate::DerivedFrom,
                    RelationPredicate::Contradicts,
                    RelationPredicate::Mentions,
                    RelationPredicate::DependsOn,
                    RelationPredicate::AppliesTo,
                    RelationPredicate::ObservedIn,
                    RelationPredicate::Attempted,
                    RelationPredicate::ResolvedBy,
                ];
                all.to_vec()
            }
        };
        let pred_names: Vec<String> = preds.iter().map(|p| p.as_str().to_string()).collect();
        let symmetric: HashSet<&'static str> = preds
            .iter()
            .filter(|p| p.spec().symmetric)
            .map(|p| p.as_str())
            .collect();
        let states: Vec<String> = req
            .states
            .clone()
            .unwrap_or_else(|| vec![RelationshipState::Active])
            .iter()
            .map(|s| s.as_str().to_string())
            .collect();
        let max_depth = req
            .budget
            .max_depth
            .unwrap_or(DEFAULT_MAX_DEPTH)
            .min(CAP_MAX_DEPTH);
        let max_visited = req
            .budget
            .max_visited
            .unwrap_or(DEFAULT_MAX_VISITED)
            .clamp(1, CAP_MAX_VISITED);
        let max_edges = req
            .budget
            .max_edges
            .unwrap_or(DEFAULT_MAX_EDGES)
            .clamp(1, CAP_MAX_EDGES);
        let deadline_ms = req
            .budget
            .deadline_ms
            .unwrap_or(DEFAULT_DEADLINE_MS)
            .clamp(1, CAP_DEADLINE_MS);
        let started = Instant::now();
        let deadline = started + std::time::Duration::from_millis(deadline_ms);

        // Seeds must exist and share the owning scope (REL-001).
        struct Frame {
            kind: nomiso_core::relationship::EndpointKind,
            key: String,
            depth: u32,
            path: Vec<String>,
        }
        let mut visited: HashSet<(u8, String)> = HashSet::new();
        let mut queue: VecDeque<Frame> = VecDeque::new();
        for seed in &req.seeds {
            let key = seed.bare_key()?;
            self.check_endpoint(seed.kind, &key, &scope).await?;
            let tag = kind_tag(seed.kind);
            if visited.insert((tag, key.clone())) {
                queue.push_back(Frame {
                    kind: seed.kind,
                    key,
                    depth: 0,
                    path: Vec::new(),
                });
            }
        }

        let mut nodes: Vec<TraverseNode> = Vec::new();
        let mut edges: Vec<nomiso_core::relationship::RelationshipRecord> = Vec::new();
        let mut edge_seen: HashSet<String> = HashSet::new();
        let mut truncated: Vec<String> = Vec::new();
        let mut edges_scanned: u32 = 0;
        let mut depth_reached: u32 = 0;
        let mark = |s: &'static str, truncated: &mut Vec<String>| {
            if !truncated.iter().any(|t| t == s) {
                truncated.push(s.to_string());
            }
        };

        let sym_names: Vec<String> = symmetric.iter().map(|s| s.to_string()).collect();
        // Direction-aware "did this node have eligible edges" clause, shared by
        // the depth-boundary probe and conceptually by the per-node scan.
        let follow_where = match req.direction {
            TraverseDirection::Out => {
                "((predicate IN $preds AND subject_kind = $k AND subject_id = $id) \
                 OR (predicate IN $sym_preds AND object_kind = $k AND object_id = $id))"
            }
            TraverseDirection::In => {
                "((predicate IN $preds AND object_kind = $k AND object_id = $id) \
                 OR (predicate IN $sym_preds AND subject_kind = $k AND subject_id = $id))"
            }
            TraverseDirection::Both => {
                "((predicate IN $preds AND (\
                    (subject_kind = $k AND subject_id = $id) \
                    OR (object_kind = $k AND object_id = $id))))"
            }
        };
        'bfs: while let Some(frame) = queue.pop_front() {
            depth_reached = depth_reached.max(frame.depth);
            if frame.depth >= max_depth {
                // Depth bound only counts as truncation when the boundary
                // node actually has followable edges we refused to take.
                let probe_sql = format!(
                    "SELECT id FROM relationship \
                     WHERE scope = $scope AND state IN $states AND {follow_where} \
                     LIMIT 1;"
                );
                let mut probe = self
                    .db
                    .query(probe_sql)
                    .bind(("scope", scope.clone()))
                    .bind(("states", states.clone()))
                    .bind(("preds", pred_names.clone()))
                    .bind(("sym_preds", sym_names.clone()))
                    .bind(("k", frame.kind.as_str().to_string()))
                    .bind(("id", frame.key.clone()))
                    .await
                    .map_err(|e| Error::store(format!("traverse depth probe: {e}")))?;
                ensure_ok(&mut probe)?;
                if !take_json_rows(&mut probe, 0)?.is_empty() {
                    mark("depth", &mut truncated);
                }
                continue;
            }
            if Instant::now() > deadline {
                mark("deadline", &mut truncated);
                break;
            }
            let remaining = max_edges.saturating_sub(edges_scanned);
            if remaining == 0 {
                mark("edges", &mut truncated);
                break;
            }
            let scan_sql = format!(
                "SELECT * FROM relationship \
                 WHERE scope = $scope AND state IN $states AND {follow_where} \
                 ORDER BY id ASC LIMIT $edge_limit;"
            );
            let mut response = self
                .db
                .query(scan_sql)
                .bind(("scope", scope.clone()))
                .bind(("states", states.clone()))
                .bind(("preds", pred_names.clone()))
                .bind(("sym_preds", sym_names.clone()))
                .bind(("k", frame.kind.as_str().to_string()))
                .bind(("id", frame.key.clone()))
                // Fetch one past the remaining budget: a surplus row is the
                // proof that edges were cut, not merely exhausted.
                .bind(("edge_limit", remaining as i64 + 1))
                .await
                .map_err(|e| Error::store(format!("traverse: {e}")))?;
            ensure_ok(&mut response)?;
            let mut rows = take_json_rows(&mut response, 0)?;
            if rows.len() as u32 > remaining {
                rows.truncate(remaining as usize);
                mark("edges", &mut truncated);
            }
            for row in rows {
                edges_scanned += 1;
                let rec = decode_relationship(&row)?;
                let edge_id = rec.id.clone();
                if !edge_seen.insert(edge_id.clone()) {
                    continue;
                }
                // Direction: `out` follows subject->object; symmetric edges
                // also flow object->subject regardless of direction.
                let is_subj =
                    rec.subject.kind == frame.kind && rec.subject.bare_key()? == frame.key;
                let is_obj = rec.object.kind == frame.kind && rec.object.bare_key()? == frame.key;
                let sym = symmetric.contains(rec.predicate.as_str());
                let nexts: Vec<EndpointRef> = match req.direction {
                    TraverseDirection::Out => {
                        let mut v = Vec::new();
                        if is_subj {
                            v.push(rec.object.clone());
                        }
                        if sym && is_obj {
                            v.push(rec.subject.clone());
                        }
                        v
                    }
                    TraverseDirection::In => {
                        let mut v = Vec::new();
                        if is_obj {
                            v.push(rec.subject.clone());
                        }
                        if sym && is_subj {
                            v.push(rec.object.clone());
                        }
                        v
                    }
                    TraverseDirection::Both => {
                        let mut v = Vec::new();
                        if is_subj {
                            v.push(rec.object.clone());
                        }
                        if is_obj {
                            v.push(rec.subject.clone());
                        }
                        v
                    }
                };
                edges.push(rec);
                for next in nexts {
                    let nkey = next.bare_key()?;
                    let tag = kind_tag(next.kind);
                    if visited.len() as u32 >= max_visited {
                        mark("visited", &mut truncated);
                        break 'bfs;
                    }
                    if visited.insert((tag, nkey.clone())) {
                        let mut path = frame.path.clone();
                        path.push(edge_id.clone());
                        nodes.push(TraverseNode {
                            endpoint: next.clone(),
                            depth: frame.depth + 1,
                            path: path.clone(),
                        });
                        queue.push_back(Frame {
                            kind: next.kind,
                            key: nkey,
                            depth: frame.depth + 1,
                            path,
                        });
                    }
                }
            }
            if edges_scanned >= max_edges {
                mark("edges", &mut truncated);
                break;
            }
        }
        // Unexplored queue means truncation even if no budget tripped inside.
        if !queue.is_empty() && truncated.is_empty() {
            truncated.push("visited".to_string());
        }
        Ok(TraverseResult {
            nodes,
            edges,
            truncated,
            depth_reached,
            visited: visited.len() as u32,
            edges_scanned,
            elapsed_ms: started.elapsed().as_millis() as u64,
        })
    }

    // --- Embedding identity and generations (MIG-004/005) ---

    async fn embedding_state(&self) -> Result<EmbeddingState> {
        let generations = self.list_generations().await?;
        let active = generations
            .iter()
            .find(|g| g.status == GenerationStatus::Active)
            .cloned();
        Ok(EmbeddingState {
            active,
            generations,
        })
    }

    async fn attest_embedding_identity(
        &self,
        identity: EmbeddingIdentity,
    ) -> Result<EmbeddingGeneration> {
        let Some(active) = self.active_generation().await? else {
            return Err(Error::invalid(
                "no active embedding generation to attest; declare one first",
            ));
        };
        if !active.identity.is_unknown() {
            return Err(Error::invalid(format!(
                "active generation {} already carries declared identity {}/{}; declare a new generation to change it",
                active.generation, active.identity.family, active.identity.model
            )));
        }
        if identity.dimension != active.identity.dimension {
            return Err(Error::invalid(format!(
                "attested dimension {} does not match generation dimension {}",
                identity.dimension, active.identity.dimension
            )));
        }
        let mut response = self
            .db
            .query(
                r#"
                UPDATE embedding_generation SET
                    family = $family,
                    model = $model,
                    normalization = $norm,
                    encoding = $encoding,
                    limitation = $limitation
                WHERE generation = $gen
                  AND family = 'unknown' AND model = 'unknown'
                RETURN AFTER;
                "#,
            )
            .bind(("family", identity.family.clone()))
            .bind(("model", identity.model.clone()))
            .bind((
                "norm",
                match identity.normalization {
                    EmbeddingNormalization::L2 => "l2",
                    EmbeddingNormalization::None => "none",
                    EmbeddingNormalization::Unknown => "unknown",
                },
            ))
            .bind(("encoding", identity.encoding.clone()))
            .bind(("limitation", identity.limitation.clone()))
            .bind(("gen", active.generation as i64))
            .await
            .map_err(|e| Error::store(format!("attest embedding identity: {e}")))?;
        ensure_ok(&mut response)?;
        self.active_generation()
            .await?
            .ok_or_else(|| Error::internal("active generation vanished during attest"))
    }

    async fn declare_embedding_generation(
        &self,
        req: DeclareGenerationRequest,
    ) -> Result<EmbeddingGeneration> {
        // A different dimension needs a different index — that is a schema
        // change, not a staged reindex on this one.
        if req.identity.dimension as usize != self.config.embedding_dim {
            return Err(Error::invalid(format!(
                "declared dimension {} does not match index dimension {}; dimension changes require a schema migration",
                req.identity.dimension, self.config.embedding_dim
            )));
        }
        let mut response = self
            .db
            .query(
                r#"
                BEGIN TRANSACTION;
                LET $building = (SELECT count() AS c FROM embedding_generation
                                 WHERE status = 'building' GROUP ALL);
                IF (($building[0].c) ?? 0) > 0 {
                    THROW 'nomiso_conflict';
                };
                LET $next = ((SELECT generation FROM embedding_generation
                              ORDER BY generation DESC LIMIT 1)[0].generation) ?? 0;
                LET $active = ((SELECT generation FROM embedding_generation
                                WHERE status = 'active' LIMIT 1)[0].generation);
                LET $frontier = ((SELECT count() AS c FROM memory
                                  WHERE embedding IS NOT NONE GROUP ALL)[0].c) ?? 0;
                CREATE embedding_generation SET
                    generation = $next + 1,
                    family = $family,
                    model = $model,
                    dimension = $dim,
                    normalization = $norm,
                    encoding = $encoding,
                    limitation = $limitation,
                    status = 'building',
                    frontier_captured_at = time::now(),
                    frontier_generation = $active,
                    expected_count = $frontier,
                    embedded_count = 0,
                    note = $note,
                    created_at = time::now(),
                    activated_at = NONE
                RETURN AFTER;
                COMMIT TRANSACTION;
                "#,
            )
            .bind(("family", req.identity.family.clone()))
            .bind(("model", req.identity.model.clone()))
            .bind(("dim", req.identity.dimension as i64))
            .bind((
                "norm",
                match req.identity.normalization {
                    EmbeddingNormalization::L2 => "l2",
                    EmbeddingNormalization::None => "none",
                    EmbeddingNormalization::Unknown => "unknown",
                },
            ))
            .bind(("encoding", req.identity.encoding.clone()))
            .bind(("limitation", req.identity.limitation.clone()))
            .bind(("note", req.note.clone()))
            .await
            .map_err(|e| Error::store(format!("declare generation: {e}")))?;
        match ensure_ok(&mut response) {
            Ok(()) => {}
            Err(e) if is_conflict_store_err(&e) => {
                return Err(Error::invalid(
                    "another embedding generation is already building",
                ));
            }
            Err(e) => return Err(e),
        }
        // Re-read the created row (highest generation = the one just made).
        let gens = self.list_generations().await?;
        gens.into_iter()
            .max_by_key(|g| g.generation)
            .ok_or_else(|| Error::internal("declared generation missing after commit"))
    }

    async fn stage_embeddings(&self, generation: u64, items: Vec<StagedEmbedding>) -> Result<u64> {
        let gens = self.list_generations().await?;
        let gen = gens
            .iter()
            .find(|g| g.generation == generation)
            .ok_or_else(|| Error::NotFound(format!("embedding generation {generation}")))?;
        if gen.status != GenerationStatus::Building {
            return Err(Error::invalid(format!(
                "embedding generation {generation} is {:?}; only a building generation accepts staged vectors",
                gen.status
            )));
        }
        for item in &items {
            if item.vector.len() != gen.identity.dimension as usize {
                return Err(Error::DimensionMismatch {
                    expected: gen.identity.dimension as usize,
                    got: item.vector.len(),
                });
            }
        }
        let payload: Vec<Value> = items
            .iter()
            .map(|i| {
                json!({
                    "rid": format!("{}x{}", generation, i.memory.bare_key()),
                    "mem": i.memory.bare_key(),
                    "vec": i.vector,
                })
            })
            .collect();
        let mut response = self
            .db
            .query(
                r#"
                BEGIN TRANSACTION;
                LET $present = (SELECT VALUE <string> record::id(id) FROM memory
                                WHERE <string> record::id(id) IN $mem_ids);
                LET $missing = array::complement($mem_ids, $present);
                IF array::len($missing) > 0 {
                    THROW 'nomiso_not_found';
                };
                FOR $item IN $items {
                    UPSERT type::record('embedding_vector', $item.rid) SET
                        memory = type::record('memory', $item.mem),
                        generation = $gen,
                        vector = $item.vec,
                        created_at = time::now();
                };
                LET $c = ((SELECT count() AS c FROM embedding_vector
                           WHERE generation = $gen GROUP ALL)[0].c) ?? 0;
                UPDATE embedding_generation SET embedded_count = $c
                WHERE generation = $gen;
                COMMIT TRANSACTION;
                "#,
            )
            .bind(("gen", generation as i64))
            .bind((
                "mem_ids",
                items
                    .iter()
                    .map(|i| i.memory.bare_key().to_string())
                    .collect::<Vec<_>>(),
            ))
            .bind(("items", payload))
            .await
            .map_err(|e| Error::store(format!("stage embeddings: {e}")))?;
        match ensure_ok(&mut response) {
            Ok(()) => {}
            Err(e) if err_text_lower(&e).contains("nomiso_not_found") => {
                return Err(Error::NotFound(
                    "staged vector references a memory that does not exist".into(),
                ));
            }
            Err(e) => return Err(e),
        }
        let gens = self.list_generations().await?;
        Ok(gens
            .iter()
            .find(|g| g.generation == generation)
            .map(|g| g.embedded_count)
            .unwrap_or(0))
    }

    async fn activate_embedding_generation(&self, generation: u64) -> Result<EmbeddingGeneration> {
        let gens = self.list_generations().await?;
        let gen = gens
            .iter()
            .find(|g| g.generation == generation)
            .ok_or_else(|| Error::NotFound(format!("embedding generation {generation}")))?;
        if !matches!(
            gen.status,
            GenerationStatus::Building | GenerationStatus::Retired
        ) {
            return Err(Error::invalid(format!(
                "embedding generation {generation} is {:?}; only building or retired generations can activate",
                gen.status
            )));
        }
        // MIG-005: validate coverage before touching the live index — an
        // incomplete generation is never silently treated as complete.
        let need = self.embedded_memory_count().await?;
        if gen.embedded_count < need {
            return Err(Error::invalid(format!(
                "embedding generation {generation} incomplete: {} staged of {} embedded memories",
                gen.embedded_count, need
            )));
        }
        let mut response = self
            .db
            .query(
                r#"
                BEGIN TRANSACTION;
                // Re-verify coverage inside the txn: staged vectors must cover
                // every currently-embedded row (extras for erased rows skip).
                LET $need = (SELECT VALUE <string> record::id(id) FROM memory
                             WHERE embedding IS NOT NONE);
                LET $have = (SELECT VALUE <string> record::id(memory) FROM embedding_vector
                             WHERE generation = $gen);
                LET $missing = array::complement($need, $have);
                IF array::len($missing) > 0 {
                    THROW 'nomiso_incomplete_generation';
                };
                FOR $v IN (SELECT * FROM embedding_vector
                           WHERE generation = $gen
                             AND memory IN (SELECT VALUE id FROM memory)) {
                    UPDATE $v.memory SET
                        embedding = $v.vector,
                        embedding_generation = $gen;
                };
                UPDATE embedding_generation SET status = 'retired'
                WHERE status = 'active';
                UPDATE embedding_generation SET
                    status = 'active',
                    activated_at = time::now()
                WHERE generation = $gen;
                COMMIT TRANSACTION;
                "#,
            )
            .bind(("gen", generation as i64))
            .await
            .map_err(|e| Error::store(format!("activate generation: {e}")))?;
        match ensure_ok(&mut response) {
            Ok(()) => {}
            Err(e) if err_text_lower(&e).contains("nomiso_incomplete_generation") => {
                return Err(Error::invalid(format!(
                    "embedding generation {generation} incomplete: staged vectors do not cover all embedded memories"
                )));
            }
            Err(e) => return Err(e),
        }
        let gens = self.list_generations().await?;
        gens.into_iter()
            .find(|g| g.generation == generation)
            .ok_or_else(|| Error::internal("activated generation missing after commit"))
    }

    // ---- Durable job journal (JOB-001..007) ----

    async fn enqueue_job(&self, req: EnqueueJobRequest) -> Result<EnqueueJobResult> {
        validate_enqueue(&req)?;
        let scope = ScopePath::parse(&req.scope)?.as_str().to_string();
        let dedup_key = job_dedup_key(&req, &scope)?;
        let inputs_json = job_inputs_value(&req.inputs)?;
        let expires_at: Option<String> = req.budget.deadline_ms.map(|ms| ts_param(now_plus_ms(ms)));

        // Input existence + scope + pinned-revision preflight, in the same
        // transaction as the durable insert (JOB-001/004).
        let mut stmt = String::from("BEGIN TRANSACTION;\n");
        let mut binds: Vec<(String, String)> = Vec::new();
        for (i, input) in req.inputs.iter().enumerate() {
            let key = format!("iid{i}");
            stmt.push_str(&job_input_check(
                &key,
                &format!("chk{i}"),
                input,
                "nomiso_bad_input",
            ));
            binds.push((key, input_bare_key(input)?));
        }
        stmt.push_str(
            "CREATE nomiso_job SET
                scope = $scope, kind = $kind, state = 'pending',
                dedup_key = $dedup, dedup_slot = $dedup,
                inputs = $inputs, composition = $comp, payload = $payload,
                budget = $budget, attempts = 0, fence = 0,
                owner = NONE, lease_until = NONE, not_before = NONE,
                expires_at = IF $expires = NONE THEN NONE ELSE type::datetime($expires) END,
                checkpoint = NONE, result = NONE,
                error = NONE, replaced_by = NONE, terminal_reason = NONE,
                history = [],
                created_at = time::now(), updated_at = time::now(),
                completed_at = NONE;
            COMMIT TRANSACTION;",
        );

        let mut q = self
            .db
            .query(stmt)
            .bind(("scope", scope.clone()))
            .bind(("kind", req.kind.trim().to_string()))
            .bind(("dedup", dedup_key.clone()))
            .bind(("inputs", inputs_json))
            .bind(("comp", req.composition.clone()))
            .bind(("payload", req.payload.clone()))
            .bind((
                "budget",
                serde_json::to_value(&req.budget)
                    .map_err(|e| Error::invalid(format!("job budget: {e}")))?,
            ))
            .bind(("expires", expires_at));
        for (k, v) in binds {
            q = q.bind((k, v));
        }
        let mut response = q
            .await
            .map_err(|e| Error::store(format!("enqueue job: {e}")))?;
        match ensure_ok(&mut response) {
            Ok(()) => {}
            Err(e) if err_text_lower(&e).contains("nomiso_bad_input") => {
                return Err(Error::invalid(
                    "job input not found in the job's scope (or pinned revision mismatch)",
                ));
            }
            // Live dedup-slot conflict → replay the existing intent (JOB-001).
            // If the slot holder isn't visible, surface the original create
            // error — it is the real signal (e.g. a non-unique-key conflict).
            Err(e) if is_state_create_conflict(&e) => {
                return match self.dedup_replay(&scope, &dedup_key).await {
                    Ok(r) => Ok(r),
                    Err(_) => Err(e),
                };
            }
            Err(e) => return Err(e),
        }
        let rows: Vec<Value> = response
            .take(response.num_statements() - 2)
            .map_err(|e| Error::store(format!("decode enqueued job: {e}")))?;
        let row = rows
            .into_iter()
            .next()
            .ok_or_else(|| Error::internal("enqueue returned no job row"))?;
        Ok(EnqueueJobResult {
            job: decode_job(&row)?,
            deduplicated: false,
        })
    }

    async fn get_job(&self, scope: &str, id: &str) -> Result<JobRecord> {
        let scope = ScopePath::parse(scope)?.as_str().to_string();
        let key = job_bare_key(id)?;
        let mut response = self
            .db
            .query(
                "SELECT * FROM nomiso_job
                 WHERE <string> record::id(id) = $id AND scope = $scope LIMIT 1;",
            )
            .bind(("id", key.clone()))
            .bind(("scope", scope))
            .await
            .map_err(|e| Error::store(format!("get job: {e}")))?;
        ensure_ok(&mut response)?;
        let rows: Vec<Value> = response
            .take(0)
            .map_err(|e| Error::store(format!("decode job: {e}")))?;
        rows.first()
            .map(decode_job)
            .transpose()?
            .ok_or_else(|| Error::NotFound(format!("job {key}")))
    }

    async fn list_jobs(&self, req: ListJobsRequest) -> Result<Vec<JobSummary>> {
        let scope = ScopePath::parse(&req.scope)?.as_str().to_string();
        let limit = req.limit.unwrap_or(100).clamp(1, 500) as i64;
        let mut sql = String::from("SELECT * FROM nomiso_job WHERE scope = $scope");
        if req.state.is_some() {
            sql.push_str(" AND state = $state");
        }
        if req.kind.is_some() {
            sql.push_str(" AND kind = $kind");
        }
        sql.push_str(" ORDER BY created_at DESC LIMIT $limit;");
        let mut q = self
            .db
            .query(sql)
            .bind(("scope", scope))
            .bind(("limit", limit));
        if let Some(s) = req.state {
            q = q.bind(("state", s.as_str().to_string()));
        }
        if let Some(k) = &req.kind {
            q = q.bind(("kind", k.clone()));
        }
        let mut response = q
            .await
            .map_err(|e| Error::store(format!("list jobs: {e}")))?;
        ensure_ok(&mut response)?;
        let rows: Vec<Value> = response
            .take(0)
            .map_err(|e| Error::store(format!("decode jobs: {e}")))?;
        rows.iter()
            .map(|r| decode_job(r).map(|j| JobSummary::from(&j)))
            .collect()
    }

    async fn claim_job(&self, req: ClaimJobRequest) -> Result<Option<JobLease>> {
        let worker = req.worker.trim().to_string();
        if worker.is_empty() || req.scopes.is_empty() || req.kinds.is_empty() {
            return Err(Error::invalid(
                "claim requires a worker identity, scope grant, and kind allowlist",
            ));
        }
        let mut scopes = Vec::new();
        for s in &req.scopes {
            scopes.push(ScopePath::parse(s)?.as_str().to_string());
        }
        // Candidates: pending past not_before, or leased past expiry — inside
        // the worker's restricted grant only.
        let mut response = self
            .db
            .query(
                "SELECT * FROM nomiso_job
                 WHERE scope IN $scopes AND kind IN $kinds
                   AND (
                     (state = 'pending' AND (not_before IS NONE OR not_before <= time::now()))
                     OR (state = 'leased' AND lease_until <= time::now())
                   )
                 ORDER BY created_at ASC LIMIT 8;",
            )
            .bind(("scopes", scopes))
            .bind(("kinds", req.kinds.clone()))
            .await
            .map_err(|e| Error::store(format!("claim scan: {e}")))?;
        ensure_ok(&mut response)?;
        let rows: Vec<Value> = response
            .take(0)
            .map_err(|e| Error::store(format!("decode claim scan: {e}")))?;

        for row in rows {
            let job = decode_job(&row)?;
            // Past-deadline pending jobs are terminal-failed, not re-run
            // (JOB-005: retries cannot exceed the original grant).
            if job.state == JobState::Pending && job.expires_at.is_some_and(|e| e <= now()) {
                self.mark_deadline_expired(&job).await?;
                continue;
            }
            if job.attempts >= job.budget.max_attempts {
                continue;
            }
            let lease_until = now_plus_ms(job.budget.lease_ms);
            let mut response = self
                .db
                .query(
                    "UPDATE type::record('nomiso_job', $id) SET
                        state = 'leased',
                        fence = fence + 1,
                        owner = $worker,
                        lease_until = type::datetime($lease_until),
                        not_before = NONE,
                        attempts = attempts + 1,
                        updated_at = time::now(),
                        history = array::append(history, $attempt)
                     WHERE (
                        (state = 'pending' AND (not_before IS NONE OR not_before <= time::now()))
                        OR (state = 'leased' AND lease_until <= time::now())
                     ) AND fence = $old_fence;",
                )
                .bind(("id", job_bare_key(&job.id)?))
                .bind(("worker", worker.clone()))
                .bind(("lease_until", ts_param(lease_until)))
                .bind(("old_fence", job.fence as i64))
                .bind((
                    "attempt",
                    json!({
                        "attempt": job.attempts + 1,
                        "worker": worker,
                        "fence": job.fence + 1,
                        "started_at": ts_param(now()),
                        "ended_at": null,
                        "error": null,
                    }),
                ))
                .await
                .map_err(|e| Error::store(format!("claim job: {e}")))?;
            ensure_ok(&mut response)?;
            let updated: Vec<Value> = response
                .take(0)
                .map_err(|e| Error::store(format!("decode claimed job: {e}")))?;
            if let Some(row) = updated.into_iter().next() {
                let job = decode_job(&row)?;
                return Ok(Some(JobLease {
                    fence: job.fence,
                    lease_until,
                    job,
                }));
            }
            // Lost the CAS to another claimant — try the next candidate.
        }
        Ok(None)
    }

    async fn renew_job_lease(&self, id: &str, fence: u64, worker: &str) -> Result<JobLease> {
        let job = self.job_by_key(id).await?;
        let lease_until = now_plus_ms(job.budget.lease_ms);
        let mut response = self
            .db
            .query(
                "UPDATE type::record('nomiso_job', $id) SET
                    lease_until = type::datetime($lease_until),
                    updated_at = time::now()
                 WHERE state = 'leased' AND fence = $fence AND owner = $worker;",
            )
            .bind(("id", job_bare_key(id)?))
            .bind(("lease_until", ts_param(lease_until)))
            .bind(("fence", fence as i64))
            .bind(("worker", worker.to_string()))
            .await
            .map_err(|e| Error::store(format!("renew lease: {e}")))?;
        ensure_ok(&mut response)?;
        let updated: Vec<Value> = response
            .take(0)
            .map_err(|e| Error::store(format!("decode renewed job: {e}")))?;
        match updated.into_iter().next() {
            Some(row) => {
                let job = decode_job(&row)?;
                Ok(JobLease {
                    fence: job.fence,
                    lease_until,
                    job,
                })
            }
            None => Err(Error::LeaseLost(format!(
                "job {} is not leased to worker {worker} under fence {fence}",
                job.id
            ))),
        }
    }

    async fn checkpoint_job(
        &self,
        id: &str,
        fence: u64,
        worker: &str,
        checkpoint: Value,
    ) -> Result<JobRecord> {
        validate_job_body("checkpoint", &checkpoint)?;
        if !checkpoint.is_object() {
            return Err(Error::invalid("job checkpoint must be an object"));
        }
        let job = self.job_by_key(id).await?;
        let mut response = self
            .db
            .query(
                "UPDATE type::record('nomiso_job', $id) SET
                    checkpoint = $checkpoint,
                    updated_at = time::now()
                 WHERE state = 'leased' AND fence = $fence AND owner = $worker;",
            )
            .bind(("id", job_bare_key(id)?))
            .bind(("checkpoint", checkpoint))
            .bind(("fence", fence as i64))
            .bind(("worker", worker.to_string()))
            .await
            .map_err(|e| Error::store(format!("checkpoint job: {e}")))?;
        ensure_ok(&mut response)?;
        let updated: Vec<Value> = response
            .take(0)
            .map_err(|e| Error::store(format!("decode checkpointed job: {e}")))?;
        match updated.into_iter().next() {
            Some(row) => decode_job(&row),
            None => Err(Error::LeaseLost(format!(
                "job {} is not leased to worker {worker} under fence {fence}",
                job.id
            ))),
        }
    }

    async fn complete_job(
        &self,
        id: &str,
        fence: u64,
        worker: &str,
        result: Value,
    ) -> Result<JobRecord> {
        validate_job_body("result", &result)?;
        if !result.is_object() {
            return Err(Error::invalid("job result must be an object"));
        }
        let job = self.job_by_key(id).await?;
        let tomb = format!("X|{}", job_bare_key(&job.id)?);

        // Fence + owner + state and every pinned input revision are
        // revalidated inside the completion transaction (JOB-003/004).
        let mut stmt = String::from("BEGIN TRANSACTION;\n");
        let mut binds: Vec<(String, String)> = Vec::new();
        for (i, input) in job.inputs.iter().enumerate() {
            let key = format!("iid{i}");
            stmt.push_str(&job_input_check(
                &key,
                &format!("chk{i}"),
                input,
                "nomiso_stale_input",
            ));
            binds.push((key, input_bare_key(input)?));
        }
        stmt.push_str(
            "UPDATE type::record('nomiso_job', $id) SET
                state = 'succeeded', result = $result,
                completed_at = time::now(), updated_at = time::now(),
                dedup_slot = $tomb, history = $history
             WHERE state = 'leased' AND fence = $fence AND owner = $worker;
            COMMIT TRANSACTION;",
        );
        let mut q = self
            .db
            .query(stmt)
            .bind(("id", job_bare_key(id)?))
            .bind(("result", result))
            .bind(("tomb", tomb))
            .bind(("history", close_history(&job)))
            .bind(("fence", fence as i64))
            .bind(("worker", worker.to_string()))
            .bind(("scope", job.scope.clone()));
        for (k, v) in binds {
            q = q.bind((k, v));
        }
        let mut response = q
            .await
            .map_err(|e| Error::store(format!("complete job: {e}")))?;
        match ensure_ok(&mut response) {
            Ok(()) => {}
            Err(e) if err_text_lower(&e).contains("nomiso_stale_input") => {
                return Err(Error::StaleInput(format!(
                    "job {} inputs no longer satisfy pinned revisions; replan the work",
                    job.id
                )));
            }
            Err(e) => return Err(e),
        }
        let updated: Vec<Value> = response
            .take(response.num_statements() - 2)
            .map_err(|e| Error::store(format!("decode completed job: {e}")))?;
        match updated.into_iter().next() {
            Some(row) => decode_job(&row),
            None => Err(Error::LeaseLost(format!(
                "job {} is not leased to worker {worker} under fence {fence}",
                job.id
            ))),
        }
    }

    async fn fail_job(
        &self,
        id: &str,
        fence: u64,
        worker: &str,
        error: JobError,
    ) -> Result<JobRecord> {
        let job = self.job_by_key(id).await?;
        let deadline_hit = job.expires_at.is_some_and(|e| e <= now());
        let retry = error.retryable && job.attempts < job.budget.max_attempts && !deadline_hit;
        let (state, not_before, tomb) = if retry {
            (
                "pending",
                Some(ts_param(now_plus_ms(job.budget.backoff_ms(job.attempts)))),
                job.dedup_key.clone(),
            )
        } else {
            ("failed", None, format!("X|{}", job_bare_key(&job.id)?))
        };
        let terminal_reason = if retry {
            None
        } else if deadline_hit {
            Some("deadline exceeded".to_string())
        } else if job.attempts >= job.budget.max_attempts {
            Some("attempt budget exhausted".to_string())
        } else {
            Some("non-retryable failure".to_string())
        };
        let err_json =
            serde_json::to_value(&error).map_err(|e| Error::invalid(format!("job error: {e}")))?;
        let mut response = self
            .db
            .query(
                "UPDATE type::record('nomiso_job', $id) SET
                    state = $state,
                    error = $error,
                    not_before = IF $not_before = NONE THEN NONE ELSE type::datetime($not_before) END,
                    lease_until = NONE, owner = NONE,
                    dedup_slot = $slot,
                    terminal_reason = $reason,
                    completed_at = IF $completed = NONE THEN NONE ELSE type::datetime($completed) END,
                    history = $history,
                    updated_at = time::now()
                 WHERE state = 'leased' AND fence = $fence AND owner = $worker;",
            )
            .bind(("id", job_bare_key(id)?))
            .bind(("state", state))
            .bind(("error", err_json))
            .bind(("not_before", not_before))
            .bind(("slot", tomb))
            .bind(("reason", terminal_reason))
            .bind((
                "completed",
                if retry { None } else { Some(ts_param(now())) },
            ))
            .bind(("history", close_history_with(&job, Some(&error.message))))
            .bind(("fence", fence as i64))
            .bind(("worker", worker.to_string()))
            .await
            .map_err(|e| Error::store(format!("fail job: {e}")))?;
        ensure_ok(&mut response)?;
        let updated: Vec<Value> = response
            .take(0)
            .map_err(|e| Error::store(format!("decode failed job: {e}")))?;
        match updated.into_iter().next() {
            Some(row) => decode_job(&row),
            None => Err(Error::LeaseLost(format!(
                "job {} is not leased to worker {worker} under fence {fence}",
                job.id
            ))),
        }
    }

    async fn cancel_job(&self, scope: &str, id: &str, reason: &str) -> Result<JobRecord> {
        let scope = ScopePath::parse(scope)?.as_str().to_string();
        let job = self.job_by_key(id).await?;
        if job.scope != scope {
            return Err(Error::ScopeDenied(format!("job {} scope", job.id)));
        }
        if job.state.terminal() {
            return Err(Error::invalid(format!(
                "job {} is already {:?}",
                job.id, job.state
            )));
        }
        let mut response = self
            .db
            .query(
                "UPDATE type::record('nomiso_job', $id) SET
                    state = 'cancelled',
                    terminal_reason = $reason,
                    dedup_slot = $tomb,
                    lease_until = NONE, owner = NONE,
                    completed_at = time::now(),
                    history = $history,
                    updated_at = time::now()
                 WHERE state IN ['pending', 'leased'] AND scope = $scope;",
            )
            .bind(("id", job_bare_key(id)?))
            .bind(("reason", reason.to_string()))
            .bind(("tomb", format!("X|{}", job_bare_key(&job.id)?)))
            .bind(("history", close_history(&job)))
            .bind(("scope", scope))
            .await
            .map_err(|e| Error::store(format!("cancel job: {e}")))?;
        ensure_ok(&mut response)?;
        let updated: Vec<Value> = response
            .take(0)
            .map_err(|e| Error::store(format!("decode cancelled job: {e}")))?;
        match updated.into_iter().next() {
            Some(row) => decode_job(&row),
            // Lost the state race — return the now-terminal record.
            None => self.job_by_key(id).await,
        }
    }

    async fn supersede_job(
        &self,
        scope: &str,
        id: &str,
        replacement_id: &str,
        reason: &str,
    ) -> Result<JobRecord> {
        let scope = ScopePath::parse(scope)?.as_str().to_string();
        let job = self.job_by_key(id).await?;
        if job.scope != scope {
            return Err(Error::ScopeDenied(format!("job {} scope", job.id)));
        }
        if job.state.terminal() {
            return Err(Error::invalid(format!(
                "job {} is already {:?}",
                job.id, job.state
            )));
        }
        let replacement = self.job_by_key(replacement_id).await?;
        if replacement.scope != scope {
            return Err(Error::ScopeDenied(
                "replacement job must share the job's scope".into(),
            ));
        }
        let mut response = self
            .db
            .query(
                "UPDATE type::record('nomiso_job', $id) SET
                    state = 'superseded',
                    replaced_by = $replacement,
                    terminal_reason = $reason,
                    dedup_slot = $tomb,
                    lease_until = NONE, owner = NONE,
                    completed_at = time::now(),
                    history = $history,
                    updated_at = time::now()
                 WHERE state IN ['pending', 'leased'] AND scope = $scope;",
            )
            .bind(("id", job_bare_key(id)?))
            .bind(("replacement", replacement.id.clone()))
            .bind(("reason", reason.to_string()))
            .bind(("tomb", format!("X|{}", job_bare_key(&job.id)?)))
            .bind(("history", close_history(&job)))
            .bind(("scope", scope))
            .await
            .map_err(|e| Error::store(format!("supersede job: {e}")))?;
        ensure_ok(&mut response)?;
        let updated: Vec<Value> = response
            .take(0)
            .map_err(|e| Error::store(format!("decode superseded job: {e}")))?;
        match updated.into_iter().next() {
            Some(row) => decode_job(&row),
            None => self.job_by_key(id).await,
        }
    }
}

/// Bare record key for an entity id in any accepted form.
fn entity_bare_key(id: &str) -> String {
    let s = id
        .trim()
        .trim_matches(|c| c == '`' || c == '"' || c == '⟨' || c == '⟩');
    let s = s.strip_prefix("entity:").unwrap_or(s);
    s.trim_matches(|c| c == '`' || c == '⟨' || c == '⟩')
        .to_string()
}

/// Bare record key for a relationship id in any accepted form.
fn rel_bare_key(id: &str) -> String {
    let s = id
        .trim()
        .trim_matches(|c| c == '`' || c == '"' || c == '⟨' || c == '⟩');
    let s = s.strip_prefix("relationship:").unwrap_or(s);
    s.trim_matches(|c| c == '`' || c == '⟨' || c == '⟩')
        .to_string()
}

/// Normalize a Surreal record-id value (`entity:`k``, `⟨…⟩`, …) to
/// `table:bare_key` for stable downstream use.
fn norm_record_id(raw: &str, table: &str) -> String {
    let s = raw
        .trim_matches(|c| c == '`' || c == '"' || c == '⟨' || c == '⟩')
        .to_string();
    let prefixed = format!("{table}:");
    let bare = s
        .strip_prefix(&prefixed)
        .unwrap_or(&s)
        .trim_matches(|c| c == '`' || c == '⟨' || c == '⟩');
    format!("{table}:{bare}")
}

/// Compact per-kind tag for visited-set keys.
fn kind_tag(kind: nomiso_core::relationship::EndpointKind) -> u8 {
    use nomiso_core::relationship::EndpointKind::*;
    match kind {
        Memory => 0,
        Entity => 1,
        Artifact => 2,
        Span => 3,
    }
}

/// Does a stored edge equal the put's logical identity (beyond the live key)?
/// Excludes server-assigned clocks and `valid_from` so identical intent replays.
fn relationship_request_matches(
    req: &nomiso_core::relationship::PutRelationshipRequest,
    rec: &nomiso_core::relationship::RelationshipRecord,
) -> bool {
    if rec.predicate != req.predicate || rec.epistemic != req.epistemic {
        return false;
    }
    if rec.valid_until != req.valid_until || rec.producer != req.producer {
        return false;
    }
    let mut a: Vec<(String, String, u64)> = rec
        .evidence
        .iter()
        .map(|e| {
            (
                e.kind.as_str().to_string(),
                nomiso_core::relationship::EndpointRef::new(e.kind, e.id.clone())
                    .bare_key()
                    .unwrap_or_else(|_| e.id.clone()),
                e.revision.unwrap_or(0),
            )
        })
        .collect();
    let mut b: Vec<(String, String, u64)> = req
        .evidence
        .iter()
        .map(|e| {
            (
                e.kind.as_str().to_string(),
                nomiso_core::relationship::EndpointRef::new(e.kind, e.id.clone())
                    .bare_key()
                    .unwrap_or_else(|_| e.id.clone()),
                e.revision.unwrap_or(0),
            )
        })
        .collect();
    a.sort();
    b.sort();
    a == b
}

fn decode_entity(v: &Value) -> Result<nomiso_core::relationship::EntityRecord> {
    use nomiso_core::relationship::EntityRecord;
    let id = v
        .get("id")
        .map(|x| norm_record_id(x.to_string().trim_matches('"'), "entity"))
        .unwrap_or_default();
    let created = match v.get("created_at") {
        Some(t) => crate::mapping::parse_timestamp(t)?,
        None => now(),
    };
    let updated = match v.get("updated_at") {
        Some(t) => crate::mapping::parse_timestamp(t)?,
        None => created,
    };
    Ok(EntityRecord {
        id,
        scope: v
            .get("scope")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        kind: v
            .get("kind")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        name: v
            .get("name")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        aliases: v
            .get("aliases")
            .and_then(|x| serde_json::from_value(x.clone()).ok())
            .unwrap_or_default(),
        attrs: v.get("attrs").cloned().filter(|a| !a.is_null()),
        version: v.get("version").and_then(|x| x.as_u64()).unwrap_or(1),
        created_at: created,
        updated_at: updated,
    })
}

fn decode_relationship(v: &Value) -> Result<nomiso_core::relationship::RelationshipRecord> {
    use nomiso_core::relationship::{
        EndpointKind, EndpointRef, EpistemicStatus, EvidenceRef, ProducerMeta, RelationPredicate,
        RelationshipRecord, RelationshipState,
    };
    let id = v
        .get("id")
        .map(|x| norm_record_id(x.to_string().trim_matches('"'), "relationship"))
        .unwrap_or_default();
    let pred_s = v.get("predicate").and_then(|x| x.as_str()).unwrap_or("");
    let predicate = RelationPredicate::parse(pred_s)
        .ok_or_else(|| Error::store(format!("relationship {id}: unknown predicate '{pred_s}'")))?;
    let ep_s = v.get("epistemic").and_then(|x| x.as_str()).unwrap_or("");
    let epistemic = EpistemicStatus::parse(ep_s)
        .ok_or_else(|| Error::store(format!("relationship {id}: unknown epistemic '{ep_s}'")))?;
    let st_s = v.get("state").and_then(|x| x.as_str()).unwrap_or("");
    let state = RelationshipState::parse(st_s)
        .ok_or_else(|| Error::store(format!("relationship {id}: unknown state '{st_s}'")))?;
    let endpoint = |kind_key: &str, id_key: &str| -> Result<EndpointRef> {
        let ks = v.get(kind_key).and_then(|x| x.as_str()).unwrap_or("");
        let kind = EndpointKind::parse(ks).ok_or_else(|| {
            Error::store(format!("relationship row: unknown endpoint kind '{ks}'"))
        })?;
        let rid = v
            .get(id_key)
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        Ok(EndpointRef { kind, id: rid })
    };
    let subject = endpoint("subject_kind", "subject_id")?;
    let object = endpoint("object_kind", "object_id")?;
    let evidence = match v.get("evidence") {
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for e in items {
                let ks = e.get("kind").and_then(|x| x.as_str()).unwrap_or("");
                let kind = EndpointKind::parse(ks).ok_or_else(|| {
                    Error::store(format!("relationship {id}: bad evidence kind '{ks}'"))
                })?;
                out.push(EvidenceRef {
                    kind,
                    id: e
                        .get("id")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                    revision: e.get("revision").and_then(|x| x.as_u64()),
                });
            }
            out
        }
        _ => Vec::new(),
    };
    let producer = v
        .get("producer")
        .filter(|p| !p.is_null())
        .map(|p| ProducerMeta {
            source: json_opt_string(p, "source"),
            kind: json_opt_string(p, "kind"),
            version: json_opt_string(p, "version"),
        });
    let valid_from = match v.get("valid_from") {
        Some(t) => crate::mapping::parse_timestamp(t)?,
        None => {
            return Err(Error::store(format!(
                "relationship {id}: missing valid_from"
            )))
        }
    };
    let valid_until = match v.get("valid_until") {
        Some(t) if !t.is_null() => Some(crate::mapping::parse_timestamp(t)?),
        _ => None,
    };
    let created = match v.get("created_at") {
        Some(t) => crate::mapping::parse_timestamp(t)?,
        None => now(),
    };
    let updated = match v.get("updated_at") {
        Some(t) => crate::mapping::parse_timestamp(t)?,
        None => created,
    };
    Ok(RelationshipRecord {
        id,
        scope: v
            .get("scope")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        predicate,
        predicate_version: v
            .get("predicate_version")
            .and_then(|x| x.as_u64())
            .unwrap_or(1) as u32,
        subject,
        object,
        subject_rev: v.get("subject_rev").and_then(|x| x.as_u64()),
        object_rev: v.get("object_rev").and_then(|x| x.as_u64()),
        epistemic,
        evidence,
        state,
        state_reason: json_opt_string(v, "state_reason"),
        valid_from,
        valid_until,
        producer,
        version: v.get("version").and_then(|x| x.as_u64()).unwrap_or(1),
        created_at: created,
        updated_at: updated,
    })
}

/// Bare record key for a job id in `job:<key>` or bare form.
fn job_bare_key(id: &str) -> Result<String> {
    let s = id
        .trim()
        .trim_matches(|c| c == '`' || c == '"' || c == '⟨' || c == '⟩');
    let s = s
        .strip_prefix("job:")
        .or_else(|| s.strip_prefix("nomiso_job:"))
        .unwrap_or(s)
        .trim_matches(|c| c == '`' || c == '⟨' || c == '⟩');
    if s.is_empty() {
        return Err(Error::invalid("job id is empty"));
    }
    Ok(s.to_string())
}

/// Bare record key for a job input's target.
fn input_bare_key(input: &JobInput) -> Result<String> {
    nomiso_core::relationship::EndpointRef::new(input.kind, input.id.clone()).bare_key()
}

/// Canonical deduplication identity: scope + kind + sorted pinned inputs +
/// canonical composition + caller hint (JOB-001).
fn job_dedup_key(req: &EnqueueJobRequest, scope: &str) -> Result<String> {
    fn sorted(value: Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut entries: Vec<_> = map.into_iter().collect();
                entries.sort_by(|a, b| a.0.cmp(&b.0));
                Value::Object(entries.into_iter().map(|(k, v)| (k, sorted(v))).collect())
            }
            Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
            value => value,
        }
    }
    let mut ins: Vec<String> = req
        .inputs
        .iter()
        .map(|i| {
            Ok(format!(
                "{}:{}:{}",
                i.kind.as_str(),
                input_bare_key(i)?,
                i.revision.unwrap_or(0)
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    ins.sort();
    let comp = serde_json::to_string(&sorted(req.composition.clone().unwrap_or(Value::Null)))
        .map_err(|e| Error::invalid(format!("job composition: {e}")))?;
    let material = format!(
        "nomiso.job-dedup.v0\n{scope}\n{}\n{}\n{comp}\n{}",
        req.kind.trim(),
        ins.join(","),
        req.dedup_hint.as_deref().unwrap_or("")
    );
    Ok(blake3::hash(material.as_bytes()).to_hex().to_string())
}

/// Serialize job inputs for storage.
fn job_inputs_value(inputs: &[JobInput]) -> Result<Value> {
    let mut out = Vec::with_capacity(inputs.len());
    for i in inputs {
        out.push(json!({
            "kind": i.kind.as_str(),
            "id": input_bare_key(i)?,
            "revision": i.revision,
        }));
    }
    Ok(Value::Array(out))
}

/// Current time plus a millisecond offset.
fn now_plus_ms(ms: u64) -> nomiso_core::types::Timestamp {
    use std::time::Duration;
    let t = std::time::SystemTime::now() + Duration::from_millis(ms);
    let nanos = t
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as i128;
    nomiso_core::types::Timestamp::from_nanosecond(nanos)
        .unwrap_or_else(|_| nomiso_core::types::Timestamp::now())
}

/// Bounded-history helper: mark the open attempt entry ended.
fn close_history_with(job: &JobRecord, error: Option<&str>) -> Value {
    let mut history = job.history.clone();
    if let Some(last) = history.last_mut().filter(|h| h.ended_at.is_none()) {
        last.ended_at = Some(now());
        if let Some(e) = error {
            last.error = Some(e.to_string());
        }
    }
    if history.len() > JOB_HISTORY_MAX {
        let drop = history.len() - JOB_HISTORY_MAX;
        history.drain(..drop);
    }
    serde_json::to_value(&history).unwrap_or(Value::Array(vec![]))
}

fn close_history(job: &JobRecord) -> Value {
    close_history_with(job, None)
}

/// Per-input staleness SQL: the row must exist in the job's scope at its
/// pinned revision (versioned kinds only). Emits a `LET $chk{i}` block.
fn job_input_check(bind_key: &str, var: &str, input: &JobInput, marker: &str) -> String {
    let table = input.kind.as_str();
    let mut s = format!(
        "LET ${var} = (SELECT * FROM {table}
             WHERE <string> record::id(id) = ${bind_key}
               AND scope = $scope LIMIT 1)[0];
        IF ${var} IS NONE {{ THROW '{marker}' }};\n"
    );
    if input.kind.versioned() {
        if let Some(rev) = input.revision {
            s.push_str(&format!(
                "IF ${var}.version != {rev} {{ THROW '{marker}' }};\n"
            ));
        }
    }
    s
}

/// Map in-transaction job markers to typed errors. `nomiso_bad_input`
/// becomes an invalid-op error; dedup/create conflicts pass through so the
/// caller can decide between retry and dedup replay.
fn job_txn_error(e: Error) -> Error {
    if err_text_lower(&e).contains("nomiso_bad_input") {
        Error::invalid("job input not found in the job's scope (or pinned revision mismatch)")
    } else {
        e
    }
}

/// Extra in-transaction material for a keyed put: the request identity
/// stored in the slot, the pre-generated record key, and appended statement
/// fragments (atomic job inserts — JOB-001).
struct KeyedTxn<'a> {
    request_identity: String,
    mem_key: String,
    extra_sql: &'a str,
    extra_binds: &'a [(String, Option<Value>)],
}

/// Outcome slot for one declared intent after the write transaction:
/// either deduplicated against a live identical job or inserted in-txn.
enum PendingJob {
    Deduped(Box<EnqueueJobResult>),
    Inserted(String),
}

/// Bind list shared by in-transaction job fragments and their enclosing
/// write transactions.
type TxnBinds = Vec<(String, Option<Value>)>;

/// In-transaction job insert: input checks plus the `CREATE nomiso_job`
/// statement, parameterized so several fragments can share one transaction
/// (JOB-001 commit-together). `scope` is supplied by the enclosing
/// transaction's `$scope` bind. Optional fields stay `Option<Value>` binds —
/// JSON `null` is `NULL`, not Surreal `NONE`, and `option<object>` schema
/// fields reject `NULL`.
fn job_txn_fragment(
    idx: usize,
    req: &EnqueueJobRequest,
    dedup_key: &str,
    expires_at: &Option<String>,
) -> Result<(String, TxnBinds)> {
    let mut sql = String::new();
    let mut binds: TxnBinds = Vec::new();
    for (i, input) in req.inputs.iter().enumerate() {
        let key = format!("iid{idx}_{i}");
        sql.push_str(&job_input_check(
            &key,
            &format!("chk{idx}_{i}"),
            input,
            "nomiso_bad_input",
        ));
        binds.push((key, Some(Value::String(input_bare_key(input)?))));
    }
    sql.push_str(&format!(
        "CREATE nomiso_job SET
            scope = $scope, kind = $jkind{idx}, state = 'pending',
            dedup_key = $jdedup{idx}, dedup_slot = $jdedup{idx},
            inputs = $jinputs{idx}, composition = $jcomp{idx}, payload = $jpayload{idx},
            budget = $jbudget{idx}, attempts = 0, fence = 0,
            owner = NONE, lease_until = NONE, not_before = NONE,
            expires_at = IF $jexpires{idx} = NONE THEN NONE ELSE type::datetime($jexpires{idx}) END,
            checkpoint = NONE, result = NONE,
            error = NONE, replaced_by = NONE, terminal_reason = NONE,
            history = [],
            created_at = time::now(), updated_at = time::now(),
            completed_at = NONE;\n"
    ));
    binds.push((
        format!("jkind{idx}"),
        Some(Value::String(req.kind.trim().to_string())),
    ));
    binds.push((
        format!("jdedup{idx}"),
        Some(Value::String(dedup_key.to_string())),
    ));
    binds.push((
        format!("jinputs{idx}"),
        Some(job_inputs_value(&req.inputs)?),
    ));
    binds.push((format!("jcomp{idx}"), req.composition.clone()));
    binds.push((format!("jpayload{idx}"), req.payload.clone()));
    binds.push((
        format!("jbudget{idx}"),
        Some(
            serde_json::to_value(&req.budget)
                .map_err(|e| Error::invalid(format!("job budget: {e}")))?,
        ),
    ));
    binds.push((
        format!("jexpires{idx}"),
        expires_at.as_ref().map(|s| Value::String(s.clone())),
    ));
    Ok((sql, binds))
}

/// Canonical identity material for a set of job intents: blake3 over the
/// declared intents (self-input marker, not the resolved record key), so a
/// keyed write+enqueue replay computes a stable identity across retries.
fn intents_identity(intents: &[JobIntent]) -> Result<String> {
    let body =
        serde_json::to_vec(intents).map_err(|e| Error::invalid(format!("job intents: {e}")))?;
    Ok(blake3::hash(&body).to_hex().to_string())
}

fn decode_job(v: &Value) -> Result<JobRecord> {
    let get = |k: &str| v.get(k).cloned().unwrap_or(Value::Null);
    let str_of = |k: &str| get(k).as_str().unwrap_or_default().to_string();
    let opt_str = |k: &str| get(k).as_str().map(|s| s.to_string());
    let opt_ts = |k: &str| -> Result<Option<nomiso_core::types::Timestamp>> {
        match get(k) {
            Value::Null => Ok(None),
            v => Ok(Some(crate::mapping::parse_timestamp(&v)?)),
        }
    };
    let state = JobState::parse(get("state").as_str().unwrap_or_default())
        .ok_or_else(|| Error::store(format!("job row: unknown state {}", get("state"))))?;
    let budget: nomiso_core::job::JobBudget = serde_json::from_value(get("budget"))
        .map_err(|e| Error::store(format!("job row budget: {e}")))?;
    let inputs: Vec<JobInput> = match get("inputs") {
        Value::Array(items) => items
            .iter()
            .map(|i| {
                let kind = nomiso_core::relationship::EndpointKind::parse(
                    i.get("kind").and_then(|x| x.as_str()).unwrap_or(""),
                )
                .ok_or_else(|| Error::store("job input: bad kind"))?;
                Ok(JobInput {
                    kind,
                    id: i
                        .get("id")
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    revision: i
                        .get("revision")
                        .and_then(|x| x.as_u64().or_else(|| x.as_i64().map(|n| n.max(0) as u64))),
                })
            })
            .collect::<Result<Vec<_>>>()?,
        _ => Vec::new(),
    };
    let error = match get("error") {
        Value::Null => None,
        e => Some(JobError {
            code: json_opt_string(&e, "code").unwrap_or_default(),
            message: json_opt_string(&e, "message").unwrap_or_default(),
            retryable: e
                .get("retryable")
                .and_then(|x| x.as_bool())
                .unwrap_or(false),
        }),
    };
    let history: Vec<JobAttempt> = match get("history") {
        Value::Array(items) => items
            .iter()
            .filter_map(|h| {
                Some(JobAttempt {
                    attempt: h.get("attempt")?.as_u64()? as u32,
                    worker: h.get("worker")?.as_str()?.to_string(),
                    fence: h.get("fence")?.as_u64()?,
                    started_at: crate::mapping::parse_timestamp(h.get("started_at")?).ok()?,
                    ended_at: match h.get("ended_at") {
                        Some(Value::Null) | None => None,
                        Some(t) => crate::mapping::parse_timestamp(t).ok(),
                    },
                    error: json_opt_string(h, "error"),
                })
            })
            .collect(),
        _ => Vec::new(),
    };
    Ok(JobRecord {
        id: v
            .get("id")
            .map(|x| norm_record_id(x.to_string().trim_matches('"'), "nomiso_job"))
            .unwrap_or_default(),
        scope: str_of("scope"),
        kind: str_of("kind"),
        state,
        dedup_key: str_of("dedup_key"),
        inputs,
        composition: match get("composition") {
            Value::Null => None,
            c => Some(c),
        },
        payload: match get("payload") {
            Value::Null => None,
            p => Some(p),
        },
        budget,
        attempts: get("attempts").as_u64().unwrap_or(0) as u32,
        fence: get("fence").as_u64().unwrap_or(0),
        owner: opt_str("owner"),
        lease_until: opt_ts("lease_until")?,
        not_before: opt_ts("not_before")?,
        expires_at: opt_ts("expires_at")?,
        checkpoint: match get("checkpoint") {
            Value::Null => None,
            c => Some(c),
        },
        result: match get("result") {
            Value::Null => None,
            r => Some(r),
        },
        error,
        replaced_by: opt_str("replaced_by"),
        terminal_reason: opt_str("terminal_reason"),
        history,
        created_at: opt_ts("created_at")?.unwrap_or_else(now),
        updated_at: opt_ts("updated_at")?.unwrap_or_else(now),
        completed_at: opt_ts("completed_at")?,
    })
}

fn decode_task_state(v: Value) -> Result<nomiso_core::task_state::TaskStateRecord> {
    use nomiso_core::task_state::TaskStateRecord;
    let id = v
        .get("id")
        .map(|x| x.to_string().trim_matches('"').to_string())
        .unwrap_or_default();
    let created = match v.get("sys_created") {
        Some(c) => crate::mapping::parse_timestamp(c)?,
        None => now(),
    };
    let updated = match v.get("sys_updated") {
        Some(c) => crate::mapping::parse_timestamp(c)?,
        None => created,
    };
    Ok(TaskStateRecord {
        id,
        scope: v
            .get("scope")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        slot: v
            .get("slot")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        body: v.get("body").cloned().unwrap_or(json!({})),
        version: v.get("version").and_then(|x| x.as_u64()).unwrap_or(1),
        sys_created: created,
        sys_updated: updated,
    })
}

/// Canonical table inventory for frontier/statistics reporting (OPS-004).
/// Kept in one place so snapshot manifests and restore checks enumerate the
/// same schema.
const FRONTIER_TABLES: &[&str] = &[
    "memory",
    "entity",
    "nomiso_meta",
    "mentions",
    "belongs_to",
    "supersedes_edge",
    "trace",
    "trace_event",
    "artifact",
    "span",
    "derived_from_edge",
    "derived_from_artifact",
    "derived_from_span",
    "idempotency_slot",
    "belief_event",
    "task_state",
    "schema_migration",
    "relationship",
    "relationship_event",
    "embedding_generation",
    "embedding_vector",
    "nomiso_job",
];

fn artifact_bare_key(id: &str) -> String {
    let s = id
        .trim()
        .trim_matches(|c| c == '`' || c == '"' || c == '⟨' || c == '⟩');
    let s = s.strip_prefix("artifact:").unwrap_or(s);
    s.trim_matches(|c| c == '`' || c == '⟨' || c == '⟩')
        .to_string()
}

fn decode_artifact(v: Value) -> Result<nomiso_core::evidence::ArtifactRecord> {
    use nomiso_core::evidence::ArtifactRecord;
    let id = match v.get("id") {
        Some(x) => {
            let s = crate::mapping::record_id_to_string(x)?;
            let bare = artifact_bare_key(&s);
            format!("artifact:{bare}")
        }
        None => return Err(Error::store("artifact row missing id")),
    };
    let created = match v.get("created_at") {
        Some(c) => crate::mapping::parse_timestamp(c)?,
        None => now(),
    };
    Ok(ArtifactRecord {
        id,
        scope: v
            .get("scope")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        blake3: v
            .get("blake3")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        location: v
            .get("location")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        media_type: v
            .get("media_type")
            .and_then(|x| x.as_str())
            .unwrap_or("application/octet-stream")
            .to_string(),
        source: v
            .get("source")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
        trust: v.get("trust").and_then(|x| x.as_f64()),
        created_at: created,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nomiso_core::ops::{ForgetRequest, PutRequest, ReadRequest, SearchQuery, SupersedeRequest};
    use nomiso_core::types::{Category, Content, Provenance, Timestamp};

    async fn setup(dim: usize) -> SurrealMemoryStore {
        let store = SurrealMemoryStore::connect(StoreConfig::memory_test(dim))
            .await
            .expect("connect");
        store.migrate().await.expect("migrate");
        store
    }

    fn put_req(scope: &str, text: &str) -> PutRequest {
        PutRequest {
            scope: scope.into(),
            category: Category::Semantic,
            content: Content::text(text),
            valid_from: None,
            valid_until: None,
            known_at: None,
            confidence: Some(0.9),
            provenance: Provenance {
                source: Some("test".into()),
                kind: Some("fixture".into()),
                span: None,
            },
            entity_links: vec!["entity:person:alice".into()],
            embedding: None,
            embedding_identity: None,
            idempotency_key: None,
            extractor_version: None,
            model_version: None,
            valid_rev_from: None,
            valid_rev_until: None,
        }
    }

    #[tokio::test]
    async fn put_read_roundtrip() {
        let store = setup(8).await;
        let wr = store
            .put(put_req("org/acme/user/alice", "Alice prefers TypeScript"))
            .await
            .expect("put");
        assert_eq!(wr.version, 1);
        let rows = store
            .read(ReadRequest {
                ids: vec![wr.id.clone()],
                scope: "org/acme/user/alice".into(),
                scope_match: ScopeMatch::Exact,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
            })
            .await
            .expect("read");
        assert_eq!(rows.len(), 1);
        assert!(rows[0].content.text.contains("TypeScript"));
    }

    #[tokio::test]
    async fn supersede_and_as_of() {
        let store = setup(8).await;
        let wr = store
            .put(put_req("org/acme", "prefers JavaScript"))
            .await
            .unwrap();
        let before = now();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let wr2 = store
            .supersede(SupersedeRequest {
                prior_id: wr.id.clone(),
                expected_version: 1,
                new: put_req("org/acme", "prefers TypeScript"),
                close_at: None,
            })
            .await
            .unwrap();
        assert_ne!(wr2.id.as_str(), wr.id.as_str());

        let hits = store
            .search(SearchQuery {
                query: "prefers TypeScript".into(),
                scope: "org/acme".into(),
                scope_match: ScopeMatch::Exact,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
                categories: None,
                limit: Some(8),
                embedding: None,
                graph_enrich: Some(false),
                graph_expand: None,
            })
            .await
            .unwrap();
        assert!(
            hits.iter().any(|h| h.preview.contains("TypeScript")),
            "expected new fact in search, got {hits:?}"
        );

        let old_hits = store
            .search(SearchQuery {
                query: "prefers JavaScript".into(),
                scope: "org/acme".into(),
                scope_match: ScopeMatch::Exact,
                as_of: Some(before),
                known_as_of: None,
                sys_as_of: None,
                categories: None,
                limit: Some(8),
                embedding: None,
                graph_enrich: Some(false),
                graph_expand: None,
            })
            .await
            .unwrap();
        assert!(
            old_hits.iter().any(|h| h.preview.contains("JavaScript")),
            "as_of should return prior fact: {old_hits:?}"
        );

        let err = store
            .supersede(SupersedeRequest {
                prior_id: wr.id.clone(),
                expected_version: 1,
                new: put_req("org/acme", "prefers Rust"),
                close_at: None,
            })
            .await;
        assert!(matches!(err, Err(Error::Conflict { .. })));
    }

    #[tokio::test]
    async fn scope_isolation() {
        let store = setup(8).await;
        store
            .put(put_req(
                "org/acme/project/a",
                "secret project a alpha-token-111",
            ))
            .await
            .unwrap();
        store
            .put(put_req(
                "org/acme/project/b",
                "secret project b beta-token-222",
            ))
            .await
            .unwrap();

        let hits_a = store
            .search(SearchQuery {
                query: "secret project".into(),
                scope: "org/acme/project/a".into(),
                scope_match: ScopeMatch::Exact,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
                categories: None,
                limit: Some(8),
                embedding: None,
                graph_enrich: Some(false),
                graph_expand: None,
            })
            .await
            .unwrap();
        assert!(hits_a.iter().all(|h| h.scope == "org/acme/project/a"));
        assert!(!hits_a.iter().any(|h| h.preview.contains("beta-token")));

        let hits_prefix = store
            .search(SearchQuery {
                query: "secret project".into(),
                scope: "org/acme".into(),
                scope_match: ScopeMatch::Prefix,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
                categories: None,
                limit: Some(8),
                embedding: None,
                graph_enrich: Some(false),
                graph_expand: None,
            })
            .await
            .unwrap();
        assert!(hits_prefix.len() >= 2);
    }

    #[tokio::test]
    async fn soft_forget_excludes_from_search() {
        let store = setup(8).await;
        let wr = store
            .put(put_req("org/acme", "unique-forget-token-xyz"))
            .await
            .unwrap();
        store
            .forget(ForgetRequest {
                id: wr.id.clone(),
                scope: "org/acme".into(),
                expected_version: Some(1),
                hard: false,
                at: None,
            })
            .await
            .unwrap();
        let hits = store
            .search(SearchQuery {
                query: "unique-forget-token-xyz".into(),
                scope: "org/acme".into(),
                scope_match: ScopeMatch::Exact,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
                categories: None,
                limit: Some(8),
                embedding: None,
                graph_enrich: Some(false),
                graph_expand: None,
            })
            .await
            .unwrap();
        assert!(
            hits.is_empty(),
            "forgotten fact should not appear: {hits:?}"
        );
    }

    #[tokio::test]
    async fn dimension_mismatch_on_put() {
        let store = setup(4).await;
        let mut req = put_req("org/acme", "has bad vector");
        req.embedding = Some(vec![0.1, 0.2]);
        let err = store.put(req).await;
        assert!(matches!(err, Err(Error::DimensionMismatch { .. })));
    }

    #[tokio::test]
    async fn read_wrong_scope_denied() {
        let store = setup(8).await;
        let wr = store
            .put(put_req("org/acme/project/a", "secret alpha"))
            .await
            .unwrap();
        let err = store
            .read(ReadRequest {
                ids: vec![wr.id],
                scope: "org/acme/project/b".into(),
                scope_match: ScopeMatch::Exact,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
            })
            .await;
        assert!(
            matches!(err, Err(Error::ScopeDenied(_))),
            "expected ScopeDenied, got {err:?}"
        );
    }

    #[tokio::test]
    async fn vector_search_path() {
        let store = setup(4).await;
        let mut req = put_req("org/acme", "vector memory about cats");
        req.embedding = Some(vec![1.0, 0.0, 0.0, 0.0]);
        store.put(req).await.unwrap();
        let mut other = put_req("org/acme", "vector memory about dogs");
        other.embedding = Some(vec![0.0, 1.0, 0.0, 0.0]);
        store.put(other).await.unwrap();

        let hits = store
            .search(SearchQuery {
                query: String::new(),
                scope: "org/acme".into(),
                scope_match: ScopeMatch::Exact,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
                categories: None,
                limit: Some(2),
                embedding: Some(vec![0.99, 0.01, 0.0, 0.0]),
                graph_enrich: Some(false),
                graph_expand: None,
            })
            .await
            .unwrap();
        assert!(!hits.is_empty());
        assert!(
            hits[0].preview.contains("cats"),
            "nearest should be cats: {hits:?}"
        );
        // Engine-derived vector distance should not be unlabeled fallback.
        assert_eq!(hits[0].score_kind, ScoreKind::Engine);
        assert!(hits[0].signals.vector);
    }

    /// N concurrent supersedes of one prior → exactly one winner, typed conflicts for losers.
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn supersede_race_one_winner() {
        let store = setup(8).await;
        let wr = store
            .put(put_req("org/race", "original concurrent fact"))
            .await
            .unwrap();
        let n = 8usize;
        let mut handles = Vec::with_capacity(n);
        for i in 0..n {
            let store = store.clone();
            let prior = wr.id.clone();
            handles.push(tokio::spawn(async move {
                store
                    .supersede(SupersedeRequest {
                        prior_id: prior,
                        expected_version: 1,
                        new: put_req("org/race", &format!("successor racer {i}")),
                        close_at: None,
                    })
                    .await
            }));
        }
        let mut oks = 0usize;
        let mut conflicts = 0usize;
        for h in handles {
            match h.await.expect("join") {
                Ok(_) => oks += 1,
                Err(Error::Conflict { .. }) => conflicts += 1,
                Err(e) => panic!("unexpected error (want Conflict or Ok): {e:?}"),
            }
        }
        assert_eq!(
            oks, 1,
            "exactly one winner; got oks={oks} conflicts={conflicts}"
        );
        assert_eq!(
            conflicts,
            n - 1,
            "all losers typed Conflict; got oks={oks} conflicts={conflicts}"
        );

        // Single live successor at current time (no multi-live forks).
        let hits = store
            .search(SearchQuery {
                query: "successor racer".into(),
                scope: "org/race".into(),
                scope_match: ScopeMatch::Exact,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
                categories: None,
                limit: Some(32),
                embedding: None,
                graph_enrich: Some(false),
                graph_expand: None,
            })
            .await
            .unwrap();
        let live: Vec<_> = hits
            .iter()
            .filter(|h| h.preview.contains("successor racer"))
            .collect();
        assert_eq!(live.len(), 1, "exactly one live successor, got {live:?}");
    }

    #[tokio::test]
    async fn soft_forget_version_guard_conflict() {
        let store = setup(8).await;
        let wr = store
            .put(put_req("org/acme", "forget-version-guard-token"))
            .await
            .unwrap();
        // Bump version via soft forget once.
        store
            .forget(ForgetRequest {
                id: wr.id.clone(),
                scope: "org/acme".into(),
                expected_version: Some(1),
                hard: false,
                at: None,
            })
            .await
            .unwrap();
        // Stale version must conflict.
        let err = store
            .forget(ForgetRequest {
                id: wr.id.clone(),
                scope: "org/acme".into(),
                expected_version: Some(1),
                hard: false,
                at: None,
            })
            .await;
        assert!(
            matches!(err, Err(Error::Conflict { .. })),
            "stale soft-forget version should Conflict, got {err:?}"
        );
    }

    #[tokio::test]
    async fn close_at_before_valid_from_rejected() {
        let store = setup(8).await;
        let wr = store
            .put(put_req("org/acme", "close-at-check"))
            .await
            .unwrap();
        let past = jiff::Timestamp::from_second(1).unwrap();
        let err = store
            .supersede(SupersedeRequest {
                prior_id: wr.id,
                expected_version: 1,
                new: put_req("org/acme", "should fail close_at"),
                close_at: Some(past),
            })
            .await;
        assert!(
            matches!(err, Err(Error::InvalidOp(_))),
            "close_at before prior valid_from should InvalidOp, got {err:?}"
        );
    }

    #[tokio::test]
    async fn engine_scores_labeled_on_bm25() {
        let store = setup(8).await;
        // Need N ≥ 3 so a rare token has df < N/2 (single-doc corpora floor IDF at 0).
        store
            .put(put_req("org/score", "unrelated filler about cats and dogs"))
            .await
            .unwrap();
        store
            .put(put_req("org/score", "another filler about weather systems"))
            .await
            .unwrap();
        store
            .put(put_req("org/score", "honest score labeling token zeta-99"))
            .await
            .unwrap();
        let hits = store
            .search(SearchQuery {
                query: "zeta-99".into(),
                scope: "org/score".into(),
                scope_match: ScopeMatch::Exact,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
                categories: None,
                limit: Some(5),
                embedding: None,
                graph_enrich: Some(false),
                graph_expand: None,
            })
            .await
            .unwrap();
        assert!(!hits.is_empty());
        let top = hits
            .iter()
            .find(|h| h.preview.contains("zeta-99"))
            .expect("rare token hit");
        assert_eq!(
            top.score_kind,
            ScoreKind::Engine,
            "distinctive BM25 (df < N/2) must be Engine, got {top:?}"
        );
        assert!(top.score > 0.05, "genuine BM25 expected, got {}", top.score);
        assert!(top.signals.bm25);
    }

    /// Discriminating BM25 df regimes (whole-table N):
    /// - df < N/2: Surreal returns genuine non-zero BM25 → ScoreKind::Engine
    /// - df ≥ N/2: engine score is 0.0 (IDF floor) → ScoreKind::RankFallback
    ///   (never unlabeled rank-decay presented as a real engine score)
    #[tokio::test]
    async fn bm25_df_regimes_score_kind_honesty() {
        let store = setup(8).await;
        let scope = "org/df";
        // N = 6 docs in the whole memory table for this store.
        // Shared term "commonword" appears in all 6 → df=6 ≥ N/2 → IDF floor → 0.0.
        // Rare term "rarezxq9" appears in 1/6 → df=1 < N/2 → genuine BM25.
        for i in 0..5 {
            store
                .put(put_req(
                    scope,
                    &format!("commonword filler document number {i} about nothing special"),
                ))
                .await
                .unwrap();
        }
        store
            .put(put_req(
                scope,
                "commonword document that also has rarezxq9 distinctive token",
            ))
            .await
            .unwrap();

        // --- low df: distinctive term ---
        let rare = store
            .search(SearchQuery {
                query: "rarezxq9".into(),
                scope: scope.into(),
                scope_match: ScopeMatch::Exact,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
                categories: None,
                limit: Some(8),
                embedding: None,
                graph_enrich: Some(false),
                graph_expand: None,
            })
            .await
            .unwrap();
        assert!(!rare.is_empty(), "rare term must retrieve at least one hit");
        for h in &rare {
            assert_eq!(
                h.score_kind,
                ScoreKind::Engine,
                "df < N/2 must expose Engine scores, got {h:?}"
            );
            assert!(
                h.score > 0.0 && h.score.is_finite(),
                "genuine BM25 for rare term should be > 0, got {}",
                h.score
            );
            // Rank-decay pseudo-scores are ~1/61 ≈ 0.016; real BM25 for rare is typically ≫ that.
            assert!(
                h.score > 0.05,
                "expected real BM25 (≫ rank-decay ~0.016), got {} kind={:?}",
                h.score,
                h.score_kind
            );
            assert!(h.signals.bm25);
        }
        assert!(
            rare.iter().any(|h| h.preview.contains("rarezxq9")),
            "rare hit content: {rare:?}"
        );

        // --- high df: term in ≥ N/2 of table ---
        let common = store
            .search(SearchQuery {
                query: "commonword".into(),
                scope: scope.into(),
                scope_match: ScopeMatch::Exact,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
                categories: None,
                limit: Some(8),
                embedding: None,
                graph_enrich: Some(false),
                graph_expand: None,
            })
            .await
            .unwrap();
        assert!(
            !common.is_empty(),
            "common term should still return rows (order may be arbitrary)"
        );
        // High-df: Surreal IDF floor ⇒ engine 0.0 ⇒ we label RankFallback.
        for h in &common {
            assert_ne!(
                h.score_kind,
                ScoreKind::Unknown,
                "must label score_kind: {h:?}"
            );
            // Critical honesty: never present pure rank-decay (~0.016) as Engine.
            if (0.01..0.03).contains(&h.score) {
                assert_eq!(
                    h.score_kind,
                    ScoreKind::RankFallback,
                    "rank-decay magnitude must be labeled RankFallback, not Engine: {h:?}"
                );
            }
            if h.score_kind == ScoreKind::Engine {
                assert!(
                    h.score > 0.05,
                    "Engine scores must be genuine non-zero BM25, not zero/floor: {h:?}"
                );
            }
        }
        let any_fallback = common
            .iter()
            .any(|h| h.score_kind == ScoreKind::RankFallback);
        assert!(
            any_fallback,
            "high-df (df≥N/2) regime must produce RankFallback after IDF floor; got {common:?}"
        );
    }

    /// Hybrid fusion path: text + vector produces Engine RRF (or channel) scores with kinds.
    #[tokio::test]
    async fn trace_append_and_get() {
        use nomiso_core::trace::{AppendTraceEvent, TraceEventKind};
        let store = setup(8).await;
        let tid = uuid::Uuid::now_v7().to_string();
        store
            .append_trace_event(AppendTraceEvent {
                trace_id: tid.clone(),
                scope: "org/tr".into(),
                kind: TraceEventKind::Search,
                session_id: Some("s1".into()),
                turn_id: None,
                payload: Some(serde_json::json!({"hits": []})),
                memory_ids: vec![],
            })
            .await
            .unwrap();
        store
            .append_trace_event(AppendTraceEvent {
                trace_id: tid.clone(),
                scope: "org/tr".into(),
                kind: TraceEventKind::Outcome,
                session_id: None,
                turn_id: None,
                payload: Some(serde_json::json!({"outcome": "unknown"})),
                memory_ids: vec![],
            })
            .await
            .unwrap();
        let bundle = store.get_trace(&tid, "org/tr").await.unwrap();
        assert_eq!(bundle.events.len(), 2);
        assert_eq!(bundle.events[0].kind, TraceEventKind::Search);
        assert_eq!(bundle.events[1].kind, TraceEventKind::Outcome);
    }

    #[tokio::test]
    async fn list_and_count_scope() {
        use nomiso_core::list::{CountRequest, ListRequest};
        let store = setup(8).await;
        store
            .put(put_req("org/list", "alpha list token"))
            .await
            .unwrap();
        store
            .put(put_req("org/list", "beta list token"))
            .await
            .unwrap();
        store
            .put(put_req("org/other", "gamma list token"))
            .await
            .unwrap();
        let page = store
            .list(ListRequest {
                scope: "org/list".into(),
                scope_match: ScopeMatch::Exact,
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
        assert_eq!(page.items.len(), 2);
        let n = store
            .count(CountRequest {
                scope: "org/list".into(),
                scope_match: ScopeMatch::Exact,
                categories: None,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
                text: None,
            })
            .await
            .unwrap();
        assert_eq!(n, 2);
        let texted = store
            .list(ListRequest {
                scope: "org/list".into(),
                scope_match: ScopeMatch::Exact,
                categories: None,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
                text: Some("alpha".into()),
                limit: Some(10),
                cursor: None,
            })
            .await
            .unwrap();
        assert!(
            texted
                .items
                .iter()
                .any(|i| i.record.content.text.contains("alpha")),
            "{texted:?}"
        );
    }

    #[tokio::test]
    async fn count_exceeds_search_limit() {
        use nomiso_core::list::CountRequest;
        let store = setup(8).await;
        for i in 0..40 {
            store
                .put(put_req(
                    "org/cnt",
                    &format!("count fact number {i} unique-token-{i}"),
                ))
                .await
                .unwrap();
        }
        let n = store
            .count(CountRequest {
                scope: "org/cnt".into(),
                scope_match: ScopeMatch::Exact,
                categories: None,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
                text: None,
            })
            .await
            .unwrap();
        assert!(n >= 40, "count must not clamp to max_search_limit; got {n}");
    }

    async fn put_dated(
        store: &SurrealMemoryStore,
        scope: &str,
        text: &str,
        known_at: Timestamp,
        embedding: Option<Vec<f32>>,
    ) -> WriteResult {
        let mut r = put_req(scope, text);
        r.known_at = Some(known_at);
        r.valid_from = Some("2000-01-01T00:00:00Z".parse().unwrap());
        r.embedding = embedding;
        store.put(r).await.expect("put_dated")
    }

    #[tokio::test]
    async fn search_known_as_of_predicates_push_down() {
        let store = setup(8).await;
        let t2020: Timestamp = "2020-01-01T00:00:00Z".parse().unwrap();
        let t2025: Timestamp = "2025-01-01T00:00:00Z".parse().unwrap();
        let t2030: Timestamp = "2030-01-01T00:00:00Z".parse().unwrap();
        let vec = vec![1.0f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let old = put_dated(
            &store,
            "org/temp",
            "shared-token early known fact",
            t2020,
            Some(vec.clone()),
        )
        .await;
        let old_sub = put_dated(
            &store,
            "org/temp/sub",
            "shared-token early known subscope fact",
            t2020,
            Some(vec.clone()),
        )
        .await;
        for i in 0..40 {
            put_dated(
                &store,
                "org/temp",
                &format!("shared-token later known fact {i}"),
                t2030,
                Some(vec.clone()),
            )
            .await;
        }
        for i in 0..5 {
            put_dated(
                &store,
                "org/temp/sub",
                &format!("shared-token later known sub fact {i}"),
                t2030,
                Some(vec.clone()),
            )
            .await;
        }
        let query =
            |scope_match: ScopeMatch, embedding: Option<Vec<f32>>, limit: u32| SearchQuery {
                query: "shared-token".into(),
                scope: "org/temp".into(),
                scope_match,
                as_of: Some(t2025),
                known_as_of: Some(t2025),
                sys_as_of: None,
                categories: None,
                limit: Some(limit),
                embedding,
                graph_enrich: Some(false),
                graph_expand: None,
            };
        let hits = store
            .search(query(ScopeMatch::Exact, None, 1))
            .await
            .unwrap();
        assert_eq!(
            hits.len(),
            1,
            "known_as_of must filter before limit/fusion: {hits:?}"
        );
        assert_eq!(hits[0].id, old.id);
        let hits = store
            .search(query(ScopeMatch::Exact, Some(vec.clone()), 1))
            .await
            .unwrap();
        assert_eq!(hits.len(), 1, "hybrid path: {hits:?}");
        assert_eq!(hits[0].id, old.id);
        let mut vec_only = query(ScopeMatch::Exact, Some(vec.clone()), 1);
        vec_only.query = String::new();
        let hits = store.search(vec_only).await.unwrap();
        assert_eq!(hits.len(), 1, "vector-only path: {hits:?}");
        assert_eq!(hits[0].id, old.id);
        let hits = store
            .search(query(ScopeMatch::Prefix, None, 8))
            .await
            .unwrap();
        let ids: std::collections::HashSet<_> =
            hits.iter().map(|h| h.id.as_str().to_string()).collect();
        assert_eq!(hits.len(), 2, "prefix scope sees both early rows: {hits:?}");
        assert!(ids.contains(old.id.as_str()) && ids.contains(old_sub.id.as_str()));
    }

    #[tokio::test]
    async fn search_sys_as_of_predicates_push_down() {
        let store = setup(8).await;
        let future: Timestamp = "2100-01-01T00:00:00Z".parse().unwrap();
        let vec = vec![1.0f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let old = put_dated(
            &store,
            "org/tsys",
            "shared-token first written fact",
            now(),
            Some(vec.clone()),
        )
        .await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let boundary = now();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        for i in 0..40 {
            put_dated(
                &store,
                "org/tsys",
                &format!("shared-token later written fact {i}"),
                now(),
                Some(vec.clone()),
            )
            .await;
        }
        let query = |text: &str, embedding: Option<Vec<f32>>| SearchQuery {
            query: text.into(),
            scope: "org/tsys".into(),
            scope_match: ScopeMatch::Exact,
            as_of: Some(future),
            known_as_of: None,
            sys_as_of: Some(boundary),
            categories: None,
            limit: Some(1),
            embedding,
            graph_enrich: Some(false),
            graph_expand: None,
        };
        for (text, emb) in [
            ("shared-token", None),
            ("shared-token", Some(vec.clone())),
            ("", Some(vec.clone())),
        ] {
            let hits = store.search(query(text, emb)).await.unwrap();
            assert_eq!(hits.len(), 1, "sys_as_of path text={text:?}: {hits:?}");
            assert_eq!(hits[0].id, old.id, "sys_as_of path text={text:?}");
        }
    }

    #[tokio::test]
    async fn scope_whitespace_aliases_normalize() {
        use nomiso_core::list::{CountRequest, ListRequest};
        use nomiso_core::task_state::{GetTaskStateRequest, PutTaskStateRequest};
        use nomiso_core::trace::{AppendTraceEvent, TraceEventKind};
        let store = setup(8).await;
        let wr = store
            .put(put_req("  org/ws  ", "whitespace scope alias token"))
            .await
            .unwrap();
        let hits = store
            .search(SearchQuery {
                query: "whitespace alias".into(),
                scope: " org/ws ".into(),
                scope_match: ScopeMatch::Exact,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
                categories: None,
                limit: Some(8),
                embedding: None,
                graph_enrich: Some(false),
                graph_expand: None,
            })
            .await
            .unwrap();
        assert!(
            hits.iter().any(|h| h.id == wr.id),
            "padded search scope must match normalized store scope: {hits:?}"
        );
        let listed = store
            .list(ListRequest {
                scope: " org/ws ".into(),
                scope_match: ScopeMatch::Exact,
                categories: None,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
                text: None,
                limit: Some(8),
                cursor: None,
            })
            .await
            .unwrap();
        assert_eq!(listed.items.len(), 1, "{listed:?}");
        let n = store
            .count(CountRequest {
                scope: " org/ws ".into(),
                scope_match: ScopeMatch::Exact,
                categories: None,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
                text: None,
            })
            .await
            .unwrap();
        assert_eq!(n, 1);
        store
            .put_task_state(PutTaskStateRequest {
                scope: " org/ws ".into(),
                slot: " coding-wm ".into(),
                body: json!({"goal": "ws"}),
                expected_version: None,
                create_only: false,
            })
            .await
            .unwrap();
        let got = store
            .get_task_state(GetTaskStateRequest {
                scope: "org/ws".into(),
                slot: "coding-wm".into(),
            })
            .await
            .unwrap()
            .expect("normalized state slot");
        assert_eq!(got.version, 1);
        store
            .append_trace_event(AppendTraceEvent {
                trace_id: "tid-ws".into(),
                scope: " org/ws ".into(),
                kind: TraceEventKind::Search,
                session_id: None,
                turn_id: None,
                payload: None,
                memory_ids: vec![],
            })
            .await
            .unwrap();
        let bundle = store.get_trace("tid-ws", "org/ws").await.unwrap();
        assert_eq!(bundle.events.len(), 1, "{bundle:?}");
        let bundle2 = store.get_trace("tid-ws", " org/ws ").await.unwrap();
        assert_eq!(bundle2.events.len(), 1, "{bundle2:?}");
    }

    #[tokio::test]
    async fn soft_forget_before_valid_from_rejected() {
        let store = setup(8).await;
        let future = "2099-01-01T00:00:00Z".parse().unwrap();
        let mut req = put_req("org/sf", "soft forget interval guard");
        req.valid_from = Some(future);
        let wr = store.put(req).await.unwrap();
        let past = "2000-01-01T00:00:00Z".parse().unwrap();
        let err = store
            .forget(ForgetRequest {
                id: wr.id.clone(),
                scope: "org/sf".into(),
                expected_version: None,
                hard: false,
                at: Some(past),
            })
            .await;
        assert!(
            matches!(err, Err(Error::InvalidOp(_))),
            "soft-forget before valid_from must fail: {err:?}"
        );
    }

    #[tokio::test]
    async fn put_past_valid_until_without_valid_from_rejected() {
        let store = setup(8).await;
        let mut req = put_req("org/sf", "omitted from with past until");
        req.valid_until = Some("2000-01-01T00:00:00Z".parse().unwrap());
        let err = store.put(req).await;
        assert!(
            matches!(err, Err(Error::InvalidOp(_))),
            "omitted valid_from + past valid_until must fail: {err:?}"
        );
    }

    #[tokio::test]
    async fn put_historical_interval_both_bounds_ok() {
        let store = setup(8).await;
        let from: jiff::Timestamp = "2000-01-01T00:00:00Z".parse().unwrap();
        let until: jiff::Timestamp = "2001-01-01T00:00:00Z".parse().unwrap();
        let mut req = put_req("org/sf", "historical ingest both bounds");
        req.valid_from = Some(from);
        req.valid_until = Some(until);
        let wr = store
            .put(req)
            .await
            .expect("historical ingest with both bounds");
        let rec = wr.record.expect("record");
        assert_eq!(rec.valid_from, from);
        assert_eq!(rec.valid_until, Some(until));
    }

    #[tokio::test]
    async fn table_counts_frontier() {
        use nomiso_core::evidence::PutArtifactRequest;
        let store = setup(8).await;
        let empty = store.table_counts().await.unwrap();
        assert_eq!(empty.get("memory"), Some(&0));
        assert_eq!(empty.get("artifact"), Some(&0));
        assert_eq!(empty.get("nomiso_job"), Some(&0));
        store.put(put_req("org/f", "frontier row")).await.unwrap();
        store
            .put_artifact(PutArtifactRequest {
                scope: "org/f".into(),
                blake3: "d".repeat(64),
                location: "file:///x".into(),
                media_type: "text/plain".into(),
                source: None,
                trust: None,
            })
            .await
            .unwrap();
        let counts = store.table_counts().await.unwrap();
        assert_eq!(counts.get("memory"), Some(&1));
        assert_eq!(counts.get("artifact"), Some(&1));
        assert!(counts.contains_key("relationship"));
        assert!(counts.contains_key("schema_migration"));
    }

    #[tokio::test]
    async fn artifact_list_and_guarded_rebind() {
        use nomiso_core::evidence::PutArtifactRequest;
        let store = setup(8).await;
        let art = store
            .put_artifact(PutArtifactRequest {
                scope: "org/reloc".into(),
                blake3: "b".repeat(64),
                location: "file:///old/root/ab/cd/blob1".into(),
                media_type: "text/plain".into(),
                source: None,
                trust: None,
            })
            .await
            .unwrap();
        let other = store
            .put_artifact(PutArtifactRequest {
                scope: "org/other".into(),
                blake3: "c".repeat(64),
                location: "file:///old/root/ef/01/blob2".into(),
                media_type: "text/plain".into(),
                source: None,
                trust: None,
            })
            .await
            .unwrap();
        // list_artifacts: exact scope filters; None lists everything.
        assert_eq!(
            store.list_artifacts(Some("org/reloc")).await.unwrap().len(),
            1
        );
        assert!(store
            .list_artifacts(None)
            .await
            .unwrap()
            .iter()
            .any(|a| a.id == other.id));
        // Guarded rebind: expected-location match rewrites; stale expected fails.
        assert!(
            !store
                .rebind_artifact_location(&art.id, "file:///WRONG", "file:///new/root/ab/cd/blob1")
                .await
                .unwrap(),
            "guard must fail on stale expected location"
        );
        assert!(store
            .rebind_artifact_location(
                &art.id,
                "file:///old/root/ab/cd/blob1",
                "file:///new/root/ab/cd/blob1"
            )
            .await
            .unwrap());
        let after = store.list_artifacts(Some("org/reloc")).await.unwrap();
        assert_eq!(after[0].location, "file:///new/root/ab/cd/blob1");
        // Re-bind again with the now-current location (idempotent retry shape).
        assert!(store
            .rebind_artifact_location(
                &art.id,
                "file:///new/root/ab/cd/blob1",
                "file:///new/root/ab/cd/blob1"
            )
            .await
            .unwrap());
        // Empty new location rejected.
        assert!(store
            .rebind_artifact_location(&art.id, "file:///new/root/ab/cd/blob1", "")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn artifact_span_and_history_annotate() {
        use nomiso_core::evidence::{
            AnnotateRequest, DerivedFromTarget, LinkDerivedFromRequest, PutArtifactRequest,
            PutSpanRequest, SpanUnit,
        };
        let store = setup(8).await;
        let art = store
            .put_artifact(PutArtifactRequest {
                scope: "org/ev".into(),
                blake3: "a".repeat(64),
                location: "file:///tmp/x".into(),
                media_type: "text/plain".into(),
                source: Some("test".into()),
                trust: Some(0.9),
            })
            .await
            .unwrap();
        assert!(art.id.contains("artifact"));
        let span = store
            .put_span(PutSpanRequest {
                scope: "org/ev".into(),
                artifact_id: art.id.clone(),
                start: 0,
                end: 10,
                unit: SpanUnit::Byte,
            })
            .await
            .unwrap();
        let wr = store
            .put(put_req("org/ev", "derived fact about code"))
            .await
            .unwrap();
        store
            .link_derived_from(LinkDerivedFromRequest {
                memory_id: wr.id.clone(),
                scope: "org/ev".into(),
                target: DerivedFromTarget::Artifact { id: art.id },
            })
            .await
            .unwrap();
        store
            .link_derived_from(LinkDerivedFromRequest {
                memory_id: wr.id.clone(),
                scope: "org/ev".into(),
                target: DerivedFromTarget::Span { id: span.id },
            })
            .await
            .unwrap();
        let hist = store.history(&wr.id, "org/ev").await.unwrap();
        assert!(!hist.is_empty());
        let ann = store
            .annotate(AnnotateRequest {
                id: wr.id.clone(),
                scope: "org/ev".into(),
                expected_version: Some(1),
                confidence: Some(0.5),
                provenance_source: Some("annot".into()),
                provenance_kind: None,
                attrs: None,
                bump_version: true,
            })
            .await
            .unwrap();
        assert_eq!(ann.version, 2);
    }

    #[tokio::test]
    async fn put_idempotency_replay() {
        let store = setup(8).await;
        let mut req = put_req("org/idemp", "idempotent body");
        req.idempotency_key = Some("k1".into());
        let a = store.put(req.clone()).await.unwrap();
        let b = store.put(req).await.unwrap();
        assert_eq!(a.id.as_str(), b.id.as_str());
    }

    #[tokio::test]
    async fn one_shot_close_and_belief_events() {
        let store = setup(8).await;
        let wr = store
            .put(put_req("org/audit", "fact for one-shot close"))
            .await
            .unwrap();
        assert!(wr.record.as_ref().unwrap().sys_created.is_some());
        store
            .forget(ForgetRequest {
                id: wr.id.clone(),
                scope: "org/audit".into(),
                expected_version: Some(1),
                hard: false,
                at: None,
            })
            .await
            .unwrap();
        let err = store
            .forget(ForgetRequest {
                id: wr.id.clone(),
                scope: "org/audit".into(),
                expected_version: None,
                hard: false,
                at: None,
            })
            .await;
        assert!(
            matches!(err, Err(Error::InvalidOp(_))),
            "second soft close must fail: {err:?}"
        );
        use nomiso_core::belief_event::BeliefEventKind;
        let ev = store.list_belief_events(&wr.id, "org/audit").await.unwrap();
        assert!(
            ev.iter().any(|e| e.kind == BeliefEventKind::Assert),
            "{ev:?}"
        );
        assert!(
            ev.iter().any(|e| e.kind == BeliefEventKind::SoftForget),
            "{ev:?}"
        );
    }

    #[tokio::test]
    async fn journal_no_erase_event_when_hard_forget_conflicts() {
        use nomiso_core::belief_event::BeliefEventKind;
        let store = setup(8).await;
        let wr = store
            .put(put_req("org/journal", "fact under journal"))
            .await
            .unwrap();
        // Version guard fails: the delete commits nothing, so no erase event
        // may be journaled for the still-live memory.
        let err = store
            .forget(ForgetRequest {
                id: wr.id.clone(),
                scope: "org/journal".into(),
                expected_version: Some(99),
                hard: true,
                at: None,
            })
            .await;
        assert!(matches!(err, Err(Error::Conflict { .. })), "{err:?}");
        let ev = store
            .list_belief_events(&wr.id, "org/journal")
            .await
            .unwrap();
        assert!(
            !ev.iter().any(|e| e.kind == BeliefEventKind::HardErase),
            "failed hard delete must not journal an erase: {ev:?}"
        );
        assert!(ev.iter().any(|e| e.kind == BeliefEventKind::Assert));
        // The memory is still live and erasable with the correct version.
        store
            .forget(ForgetRequest {
                id: wr.id.clone(),
                scope: "org/journal".into(),
                expected_version: Some(1),
                hard: true,
                at: None,
            })
            .await
            .unwrap();
        let ev = store
            .list_belief_events(&wr.id, "org/journal")
            .await
            .unwrap();
        let erase = ev
            .iter()
            .find(|e| e.kind == BeliefEventKind::HardErase)
            .expect("committed erase journals an event");
        assert_eq!(erase.payload.as_ref().unwrap()["version"], 1);
    }

    #[tokio::test]
    async fn journal_no_forget_event_when_soft_forget_conflicts() {
        use nomiso_core::belief_event::BeliefEventKind;
        let store = setup(8).await;
        let wr = store
            .put(put_req("org/journal2", "fact under journal"))
            .await
            .unwrap();
        let err = store
            .forget(ForgetRequest {
                id: wr.id.clone(),
                scope: "org/journal2".into(),
                expected_version: Some(42),
                hard: false,
                at: None,
            })
            .await;
        assert!(matches!(err, Err(Error::Conflict { .. })), "{err:?}");
        let ev = store
            .list_belief_events(&wr.id, "org/journal2")
            .await
            .unwrap();
        assert!(
            !ev.iter().any(|e| e.kind == BeliefEventKind::SoftForget),
            "failed soft close must not journal a forget: {ev:?}"
        );
    }

    #[tokio::test]
    async fn journal_supersede_commits_close_and_assert() {
        use nomiso_core::belief_event::BeliefEventKind;
        let store = setup(8).await;
        let prior = store.put(put_req("org/jsup", "prior fact")).await.unwrap();
        let succ = store
            .supersede(SupersedeRequest {
                prior_id: prior.id.clone(),
                expected_version: 1,
                new: put_req("org/jsup", "successor fact"),
                close_at: None,
            })
            .await
            .unwrap();
        let prior_ev = store
            .list_belief_events(&prior.id, "org/jsup")
            .await
            .unwrap();
        let close = prior_ev
            .iter()
            .find(|e| e.kind == BeliefEventKind::Close)
            .expect("supersede must journal the prior close");
        assert_eq!(
            close.payload.as_ref().unwrap()["successor"],
            succ.id.to_string()
        );
        assert_eq!(close.payload.as_ref().unwrap()["closed_version"], 2);
        let succ_ev = store
            .list_belief_events(&succ.id, "org/jsup")
            .await
            .unwrap();
        let assert = succ_ev
            .iter()
            .find(|e| e.kind == BeliefEventKind::Assert)
            .expect("supersede must journal the successor assert");
        assert_eq!(
            assert.payload.as_ref().unwrap()["prior"],
            prior.id.to_string()
        );
    }

    #[tokio::test]
    async fn journal_failed_supersede_journals_nothing() {
        use nomiso_core::belief_event::BeliefEventKind;
        let store = setup(8).await;
        let prior = store.put(put_req("org/jfail", "prior fact")).await.unwrap();
        let err = store
            .supersede(SupersedeRequest {
                prior_id: prior.id.clone(),
                expected_version: 77,
                new: put_req("org/jfail", "never committed"),
                close_at: None,
            })
            .await;
        assert!(matches!(err, Err(Error::Conflict { .. })), "{err:?}");
        let ev = store
            .list_belief_events(&prior.id, "org/jfail")
            .await
            .unwrap();
        assert!(
            ev.iter().all(|e| e.kind == BeliefEventKind::Assert),
            "failed supersede must not journal close: {ev:?}"
        );
    }

    #[tokio::test]
    async fn journal_keyed_replay_adds_no_event() {
        use nomiso_core::belief_event::BeliefEventKind;
        let store = setup(8).await;
        let mut req = put_req("org/jkey", "keyed fact");
        req.idempotency_key = Some("journal-key".into());
        let first = store.put(req.clone()).await.unwrap();
        let replay = store.put(req).await.unwrap();
        assert!(replay.replayed);
        let ev = store
            .list_belief_events(&first.id, "org/jkey")
            .await
            .unwrap();
        let asserts = ev
            .iter()
            .filter(|e| e.kind == BeliefEventKind::Assert)
            .count();
        assert_eq!(asserts, 1, "replay is not a new effect: {ev:?}");
        let assert_ev = ev
            .iter()
            .find(|e| e.kind == BeliefEventKind::Assert)
            .unwrap();
        assert_eq!(assert_ev.payload.as_ref().unwrap()["key"], "journal-key");
    }

    #[tokio::test]
    async fn journal_annotate_records_committed_version() {
        use nomiso_core::belief_event::BeliefEventKind;
        let store = setup(8).await;
        let wr = store
            .put(put_req("org/jann", "annotatable fact"))
            .await
            .unwrap();
        let rec = store
            .annotate(nomiso_core::evidence::AnnotateRequest {
                id: wr.id.clone(),
                scope: "org/jann".into(),
                expected_version: Some(1),
                confidence: Some(0.9),
                provenance_source: None,
                provenance_kind: None,
                attrs: None,
                bump_version: true,
            })
            .await
            .unwrap();
        assert_eq!(rec.version, 2);
        let ev = store.list_belief_events(&wr.id, "org/jann").await.unwrap();
        let ann = ev
            .iter()
            .find(|e| e.kind == BeliefEventKind::Annotate)
            .expect("annotate commits a journal event");
        assert_eq!(ann.payload.as_ref().unwrap()["version"], 2);
    }

    #[tokio::test]
    async fn migrate_is_idempotent_and_ledgered() {
        let store = setup(8).await;
        // Second migrate must be a no-op over an already-ledgered store.
        store.migrate().await.unwrap();
        let mut response = store
            .db
            .query("SELECT * FROM schema_migration;")
            .await
            .unwrap();
        ensure_ok(&mut response).unwrap();
        let rows: Vec<Value> = response.take(0).unwrap();
        assert!(
            rows.len() >= 9,
            "every migration has a ledger row: {rows:?}"
        );
        for row in &rows {
            assert_eq!(row["status"], "applied", "{row:?}");
            assert_eq!(row["checksum"].as_str().unwrap().len(), 64);
            assert!(row["applied_at"].is_string(), "{row:?}");
        }
        let schema = store.meta_value("schema").await.unwrap().unwrap();
        assert_eq!(schema.as_str().unwrap(), SCHEMA_VERSION);
    }

    #[tokio::test]
    async fn migrate_rejects_newer_store_marker() {
        let store = setup(8).await;
        let mut r = store
            .db
            .query("UPSERT nomiso_meta:schema SET name = 'schema_version', value = '99.0.0';")
            .await
            .unwrap();
        ensure_ok(&mut r).unwrap();
        let err = store.migrate().await.unwrap_err();
        assert!(
            matches!(err, Error::IncompatibleStore(_)),
            "newer store must fail closed: {err:?}"
        );
        assert_eq!(err.code(), "incompatible_store");
    }

    #[tokio::test]
    async fn migrate_rejects_unrecognized_store_marker() {
        let store = setup(8).await;
        let mut r = store
            .db
            .query(
                "UPSERT nomiso_meta:schema SET name = 'schema_version', value = 'not-a-version';",
            )
            .await
            .unwrap();
        ensure_ok(&mut r).unwrap();
        assert!(matches!(
            store.migrate().await.unwrap_err(),
            Error::IncompatibleStore(_)
        ));
    }

    #[tokio::test]
    async fn migrate_rejects_drifted_migration_checksum() {
        let store = setup(8).await;
        let mut r = store
            .db
            .query("UPDATE schema_migration:001_core SET checksum = 'tampered';")
            .await
            .unwrap();
        ensure_ok(&mut r).unwrap();
        let err = store.migrate().await.unwrap_err();
        assert!(
            matches!(err, Error::IncompatibleStore(_)),
            "drifted applied migration must fail closed: {err:?}"
        );
    }

    #[tokio::test]
    async fn migrate_rechecks_dim_before_markers() {
        let store = setup(8).await;
        // Reopen the same logical store identity with a different dim marker.
        let mut r = store
            .db
            .query("UPSERT nomiso_meta:embedding_dim SET name = 'embedding_dim', value = 16;")
            .await
            .unwrap();
        ensure_ok(&mut r).unwrap();
        let err = store.migrate().await.unwrap_err();
        assert!(matches!(err, Error::InvalidOp(_)), "{err:?}");
        // The schema marker must not have advanced past the failed check.
        let schema = store.meta_value("schema").await.unwrap().unwrap();
        assert_eq!(schema.as_str().unwrap(), SCHEMA_VERSION);
    }

    #[tokio::test]
    async fn migrate_concurrent_migrators_converge() {
        let store = setup(8).await;
        let mut r = store.db.query("DELETE schema_migration;").await.unwrap();
        ensure_ok(&mut r).unwrap();
        let mut tasks = Vec::new();
        for _ in 0..4 {
            let store = store.clone();
            tasks.push(tokio::spawn(async move { store.migrate().await }));
        }
        for t in tasks {
            t.await.unwrap().unwrap();
        }
        let mut response = store
            .db
            .query("SELECT * FROM schema_migration;")
            .await
            .unwrap();
        ensure_ok(&mut response).unwrap();
        let rows: Vec<Value> = response.take(0).unwrap();
        assert!(rows.iter().all(|r| r["status"] == "applied"), "{rows:?}");
    }

    #[tokio::test]
    async fn task_state_roundtrip() {
        use nomiso_core::task_state::{GetTaskStateRequest, PutTaskStateRequest};
        let store = setup(8).await;
        let put = store
            .put_task_state(PutTaskStateRequest {
                scope: "org/wm".into(),
                slot: "coding-wm".into(),
                body: json!({"goal": "ship"}),
                expected_version: None,
                create_only: false,
            })
            .await
            .unwrap();
        assert_eq!(put.version, 1);
        let got = store
            .get_task_state(GetTaskStateRequest {
                scope: "org/wm".into(),
                slot: "coding-wm".into(),
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.body["goal"], "ship");
        // Optimistic version conflict
        let conflict = store
            .put_task_state(PutTaskStateRequest {
                scope: "org/wm".into(),
                slot: "coding-wm".into(),
                body: json!({"goal": "wrong"}),
                expected_version: Some(99),
                create_only: false,
            })
            .await;
        assert!(
            matches!(conflict, Err(Error::Conflict { .. })),
            "{conflict:?}"
        );
    }

    #[tokio::test]
    async fn known_as_of_and_sys_as_of_lenses() {
        let store = setup(8).await;
        let past = "2000-01-01T00:00:00Z".parse().unwrap();
        let req = put_req("org/lens", "fact learned late about lenses");
        // known_at defaults to now on create; past known_as_of must exclude it.
        let wr = store.put(req).await.unwrap();
        let rec = wr.record.as_ref().unwrap();
        assert!(rec.sys_created.is_some());

        let none_past = store
            .search(SearchQuery {
                query: "lenses".into(),
                scope: "org/lens".into(),
                scope_match: ScopeMatch::Exact,
                as_of: None,
                known_as_of: Some(past),
                sys_as_of: None,
                categories: None,
                limit: Some(8),
                embedding: None,
                graph_enrich: Some(false),
                graph_expand: None,
            })
            .await
            .unwrap();
        assert!(
            none_past.is_empty(),
            "known_as_of in the past must hide facts learned later: {none_past:?}"
        );

        let now_hits = store
            .search(SearchQuery {
                query: "lenses".into(),
                scope: "org/lens".into(),
                scope_match: ScopeMatch::Exact,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
                categories: None,
                limit: Some(8),
                embedding: None,
                graph_enrich: Some(false),
                graph_expand: None,
            })
            .await
            .unwrap();
        assert!(!now_hits.is_empty());

        // Soft-close then sys_as_of after close should exclude (closed as of U).
        store
            .forget(ForgetRequest {
                id: wr.id.clone(),
                scope: "org/lens".into(),
                expected_version: None,
                hard: false,
                at: None,
            })
            .await
            .unwrap();
        let future = "2099-01-01T00:00:00Z".parse().unwrap();
        let after_close = store
            .read(ReadRequest {
                ids: vec![wr.id.clone()],
                scope: "org/lens".into(),
                scope_match: ScopeMatch::Exact,
                as_of: None,
                known_as_of: None,
                sys_as_of: Some(future),
            })
            .await
            .unwrap();
        assert!(
            after_close.is_empty(),
            "sys_as_of after close must hide closed row: {after_close:?}"
        );
    }

    #[tokio::test]
    async fn supersede_marks_derived_dependents_stale() {
        use nomiso_core::evidence::{DerivedFromTarget, LinkDerivedFromRequest};
        let store = setup(8).await;
        let base = store
            .put(put_req("org/stale", "base fact for derivation"))
            .await
            .unwrap();
        let dep = store
            .put(put_req("org/stale", "summary derived from base"))
            .await
            .unwrap();
        store
            .link_derived_from(LinkDerivedFromRequest {
                memory_id: dep.id.clone(),
                scope: "org/stale".into(),
                target: DerivedFromTarget::Memory {
                    id: base.id.clone(),
                },
            })
            .await
            .unwrap();
        store
            .supersede(SupersedeRequest {
                prior_id: base.id.clone(),
                expected_version: 1,
                new: put_req("org/stale", "base fact revised"),
                close_at: None,
            })
            .await
            .unwrap();
        let rows = store
            .read(ReadRequest {
                ids: vec![dep.id.clone()],
                scope: "org/stale".into(),
                scope_match: ScopeMatch::Exact,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
            })
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].stale,
            Some(true),
            "dependent should be marked stale after source supersede: {:?}",
            rows[0]
        );
    }

    #[tokio::test]
    async fn hybrid_fusion_score_kind_engine() {
        let store = setup(4).await;
        let mut req = put_req("org/hyb", "hybrid fusion prefers TypeScript tooling");
        req.embedding = Some(vec![1.0, 0.0, 0.0, 0.0]);
        store.put(req).await.unwrap();
        let mut other = put_req("org/hyb", "unrelated quantum barbecue noise");
        other.embedding = Some(vec![0.0, 1.0, 0.0, 0.0]);
        store.put(other).await.unwrap();

        let hits = store
            .search(SearchQuery {
                query: "TypeScript tooling".into(),
                scope: "org/hyb".into(),
                scope_match: ScopeMatch::Exact,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
                categories: None,
                limit: Some(5),
                embedding: Some(vec![0.95, 0.05, 0.0, 0.0]),
                graph_enrich: Some(false),
                graph_expand: None,
            })
            .await
            .unwrap();
        assert!(!hits.is_empty(), "hybrid must return hits");
        for h in &hits {
            assert_ne!(
                h.score_kind,
                ScoreKind::Unknown,
                "hybrid hit must label score_kind: {h:?}"
            );
            // Prefer engine scores on fusion path; if fallback, must be labeled.
            if matches!(h.score_kind, ScoreKind::Engine) {
                assert!(h.score.is_finite());
            }
        }
        assert!(
            hits.iter().any(|h| h.preview.contains("TypeScript")),
            "TypeScript doc should rank: {hits:?}"
        );
    }

    #[tokio::test]
    async fn annotate_telemetry_no_version_bump() {
        use nomiso_core::evidence::AnnotateRequest;
        let store = setup(8).await;
        let wr = store
            .put(put_req("org/tel", "telemetry target fact"))
            .await
            .unwrap();
        let ann = store
            .annotate(AnnotateRequest {
                id: wr.id.clone(),
                scope: "org/tel".into(),
                expected_version: None,
                confidence: None,
                provenance_source: None,
                provenance_kind: None,
                attrs: Some(json!({"scan_count": 1})),
                bump_version: false,
            })
            .await
            .unwrap();
        assert_eq!(ann.version, 1, "telemetry annotate must not bump version");
    }

    /// Multi-process smoke helpers (invoked by scripts/smoke-durable-mp.sh).
    #[cfg(feature = "embedded-rocks")]
    #[tokio::test]
    #[ignore = "scripts/smoke-durable-mp.sh sets NOMISO_DURABLE_MP_PHASE and passes --ignored"]
    async fn durable_mp_write() {
        let phase = std::env::var("NOMISO_DURABLE_MP_PHASE").unwrap_or_default();
        assert_eq!(
            phase, "write",
            "set NOMISO_DURABLE_MP_PHASE=write (see scripts/smoke-durable-mp.sh)"
        );
        let path = std::env::var("NOMISO_DURABLE_MP_PATH").expect("NOMISO_DURABLE_MP_PATH");
        let store = SurrealMemoryStore::connect(StoreConfig::rocksdb_path(&path, 8))
            .await
            .unwrap();
        store.migrate().await.unwrap();
        store
            .put(put_req(
                "org/mp",
                "multi-process durable token MP_UNIQUE_42 survives across processes",
            ))
            .await
            .unwrap();
        // Leave process; drop closes rocks lock for the next process.
    }

    #[cfg(feature = "embedded-rocks")]
    #[tokio::test]
    #[ignore = "scripts/smoke-durable-mp.sh sets NOMISO_DURABLE_MP_PHASE and passes --ignored"]
    async fn durable_mp_read() {
        let phase = std::env::var("NOMISO_DURABLE_MP_PHASE").unwrap_or_default();
        assert_eq!(
            phase, "read",
            "set NOMISO_DURABLE_MP_PHASE=read (see scripts/smoke-durable-mp.sh)"
        );
        let path = std::env::var("NOMISO_DURABLE_MP_PATH").expect("NOMISO_DURABLE_MP_PATH");
        let store = SurrealMemoryStore::connect(StoreConfig::rocksdb_path(&path, 8))
            .await
            .unwrap();
        store.migrate().await.unwrap();
        let hits = store
            .search(SearchQuery {
                query: "MP_UNIQUE_42".into(),
                scope: "org/mp".into(),
                scope_match: ScopeMatch::Exact,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
                categories: None,
                limit: Some(8),
                embedding: None,
                graph_enrich: Some(false),
                graph_expand: None,
            })
            .await
            .unwrap();
        assert!(
            hits.iter().any(|h| h.preview.contains("MP_UNIQUE_42")),
            "process B must see process A write: {hits:?}"
        );
    }

    /// Embedded RocksDB engine path (single connection).
    ///
    /// Surreal/RocksDB holds a process-level lock; same-process reconnect is not
    /// supported. Multi-session durability is multi-process (CLI/smoke on shared path).
    #[cfg(feature = "embedded-rocks")]
    #[tokio::test]
    async fn durable_rocksdb_put_read_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nomiso-rocks");
        assert!(!StoreConfig::rocksdb_path(&path, 8).is_ephemeral());
        let store = SurrealMemoryStore::connect(StoreConfig::rocksdb_path(&path, 8))
            .await
            .unwrap();
        store.migrate().await.unwrap();
        let wr = store
            .put(put_req("org/dur", "durable rocks fact on embedded engine"))
            .await
            .unwrap();
        let rows = store
            .read(ReadRequest {
                ids: vec![wr.id.clone()],
                scope: "org/dur".into(),
                scope_match: ScopeMatch::Exact,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
            })
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].content.text.contains("durable rocks"));
        // Path must exist on disk (not pure memory).
        assert!(path.exists(), "rocksdb path should be created: {path:?}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn task_state_race_one_winner() {
        use nomiso_core::task_state::PutTaskStateRequest;
        let store = setup(8).await;
        store
            .put_task_state(PutTaskStateRequest {
                scope: "org/ts".into(),
                slot: "coding-wm".into(),
                body: json!({"n": 0}),
                expected_version: None,
                create_only: false,
            })
            .await
            .unwrap();
        let n = 8usize;
        let mut handles = Vec::with_capacity(n);
        for i in 0..n {
            let store = store.clone();
            handles.push(tokio::spawn(async move {
                store
                    .put_task_state(PutTaskStateRequest {
                        scope: "org/ts".into(),
                        slot: "coding-wm".into(),
                        body: json!({"n": i}),
                        expected_version: Some(1),
                        create_only: false,
                    })
                    .await
            }));
        }
        let mut oks = 0usize;
        let mut conflicts = 0usize;
        for h in handles {
            match h.await.expect("join") {
                Ok(_) => oks += 1,
                Err(Error::Conflict { .. }) => conflicts += 1,
                Err(e) => panic!("unexpected error (want Conflict or Ok): {e:?}"),
            }
        }
        assert_eq!(
            oks, 1,
            "exactly one CAS winner; oks={oks} conflicts={conflicts}"
        );
        assert_eq!(conflicts, n - 1);
        let got = store
            .get_task_state(nomiso_core::task_state::GetTaskStateRequest {
                scope: "org/ts".into(),
                slot: "coding-wm".into(),
            })
            .await
            .unwrap()
            .expect("slot");
        assert_eq!(got.version, 2, "one increment from v1: {got:?}");
        let mut count_resp = store
            .db
            .query("SELECT * FROM task_state WHERE scope = $scope AND slot = $slot;")
            .bind(("scope", "org/ts"))
            .bind(("slot", "coding-wm"))
            .await
            .unwrap();
        let count_rows = take_json_rows(&mut count_resp, 0).unwrap();
        assert_eq!(
            count_rows.len(),
            1,
            "UNIQUE scope+slot must hold: {count_rows:?}"
        );
    }

    #[tokio::test]
    async fn list_paginates_all_ids_once_with_cursor_context() {
        use nomiso_core::list::{ListCursor, ListRequest};
        let store = setup(8).await;
        for i in 0..100 {
            let mut r = put_req("org/page", &format!("unrelated filler doc {i}"));
            r.valid_from = Some(Timestamp::from_second(1_699_000_000 + i as i64).unwrap());
            store.put(r).await.unwrap();
        }
        for i in 0..40 {
            let mut r = put_req(
                "org/page",
                &format!(
                    "{} padding {}",
                    "sharedtoken ".repeat((1 + i % 5) as usize),
                    "neutral ".repeat(30)
                ),
            );
            r.valid_from = Some(Timestamp::from_second(1_700_000_000 + i as i64 * 7).unwrap());
            store.put(r).await.unwrap();
        }
        let req = |cursor: Option<ListCursor>| ListRequest {
            scope: "org/page".into(),
            scope_match: ScopeMatch::Exact,
            categories: None,
            as_of: None,
            known_as_of: None,
            sys_as_of: None,
            text: Some("sharedtoken".into()),
            limit: Some(7),
            cursor,
        };
        let mut seen = std::collections::HashSet::new();
        let mut cursor = None;
        let mut pages = 0usize;
        loop {
            let page = store.list(req(cursor)).await.unwrap();
            pages += 1;
            for it in &page.items {
                assert!(
                    seen.insert(it.record.id.as_str().to_string()),
                    "duplicate id across pages: {:?}",
                    it.record.id
                );
            }
            match page.next_cursor {
                Some(c) => {
                    assert!(c.query.is_some(), "cursor must carry query context");
                    cursor = Some(c);
                }
                None => break,
            }
            if pages > 20 {
                panic!("pagination did not terminate");
            }
        }
        assert_eq!(seen.len(), 40, "every id exactly once, no skips");
        assert_eq!(pages, 6, "ceil(40/7) pages; got {pages}");
        let text_count = store
            .count(nomiso_core::list::CountRequest {
                scope: "org/page".into(),
                scope_match: ScopeMatch::Exact,
                categories: None,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
                text: Some("sharedtoken".into()),
            })
            .await
            .unwrap();
        assert_eq!(
            text_count, 40,
            "text-filtered count must page the same 40 regardless of lexical rank"
        );

        // Cursor context mismatches are rejected.
        let first = store.list(req(None)).await.unwrap();
        let cur = first.next_cursor.clone().expect("page-1 cursor");
        let mut wrong_scope = req(Some(cur.clone()));
        wrong_scope.scope = "org/other".into();
        assert!(store.list(wrong_scope).await.is_err(), "scope mismatch");
        let mut wrong_text = req(Some(cur.clone()));
        wrong_text.text = Some("other".into());
        assert!(store.list(wrong_text).await.is_err(), "text mismatch");
        let mut wrong_as_of = req(Some(cur.clone()));
        wrong_as_of.as_of = Some("1999-01-01T00:00:00Z".parse().unwrap());
        assert!(store.list(wrong_as_of).await.is_err(), "as_of mismatch");
        let mut wrong_known = req(Some(cur.clone()));
        wrong_known.known_as_of = Some(now());
        assert!(
            store.list(wrong_known).await.is_err(),
            "known_as_of mismatch"
        );
        let mut wrong_sys = req(Some(cur.clone()));
        wrong_sys.sys_as_of = Some(now());
        assert!(store.list(wrong_sys).await.is_err(), "sys_as_of mismatch");
        let mut wrong_cats = req(Some(cur.clone()));
        wrong_cats.categories = Some(vec![Category::Episodic]);
        assert!(store.list(wrong_cats).await.is_err(), "categories mismatch");
        // Legacy two-field cursor (no query context) is explicitly rejected.
        let legacy = ListCursor {
            valid_from: cur.valid_from,
            id: cur.id.clone(),
            query: None,
        };
        assert!(
            store.list(req(Some(legacy))).await.is_err(),
            "legacy cursor without context must be rejected"
        );
        // Whitespace-padded scope normalizes to the same context.
        let mut padded = req(Some(cur.clone()));
        padded.scope = "  org/page  ".into();
        assert!(
            store.list(padded).await.is_ok(),
            "normalized scope must accept the cursor"
        );
        // Page-size changes are allowed: limit is not part of the context.
        let mut resized = req(Some(cur));
        resized.limit = Some(5);
        let p2 = store.list(resized).await.unwrap();
        assert_eq!(p2.items.len(), 5);
    }

    #[tokio::test]
    async fn count_page_limit_is_explicit() {
        use nomiso_core::list::CountRequest;
        let store = setup(8).await;
        for i in 0..40 {
            store
                .put(put_req("org/cntp", &format!("cntp item {i}")))
                .await
                .unwrap();
        }
        let req = CountRequest {
            scope: "org/cntp".into(),
            scope_match: ScopeMatch::Exact,
            categories: None,
            as_of: None,
            known_as_of: None,
            sys_as_of: None,
            text: None,
        };
        let err = store
            .count_with_page_limit(req.clone(), 1)
            .await
            .expect_err("40 rows cannot fit one page of 32");
        assert!(
            matches!(err, Error::PayloadTooLarge(_)),
            "expected PayloadTooLarge, got {err:?}"
        );
        let n = store.count_with_page_limit(req.clone(), 2).await.unwrap();
        assert_eq!(n, 40);
        let n = store.count(req).await.unwrap();
        assert_eq!(n, 40);
    }

    #[tokio::test]
    async fn task_state_create_only() {
        use nomiso_core::task_state::{GetTaskStateRequest, PutTaskStateRequest};
        let store = setup(8).await;
        let first = store
            .put_task_state(PutTaskStateRequest {
                scope: "org/co".into(),
                slot: "s1".into(),
                body: json!({"v": 1}),
                expected_version: None,
                create_only: true,
            })
            .await
            .unwrap();
        assert_eq!(first.version, 1);
        let err = store
            .put_task_state(PutTaskStateRequest {
                scope: "org/co".into(),
                slot: "s1".into(),
                body: json!({"v": 2}),
                expected_version: None,
                create_only: true,
            })
            .await
            .expect_err("second create_only must conflict");
        match err {
            Error::Conflict {
                expected: 0,
                found: 1,
            } => {}
            other => panic!("expected Conflict{{expected:0, found:1}}, got {other:?}"),
        }
        let got = store
            .get_task_state(GetTaskStateRequest {
                scope: "org/co".into(),
                slot: "s1".into(),
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.body["v"], 1, "conflicting create must not overwrite");
        assert_eq!(got.version, 1, "version must not advance");
        let bad = |slot: &str, body: Value, expected_version: Option<u64>, create_only: bool| {
            PutTaskStateRequest {
                scope: "org/co".into(),
                slot: slot.into(),
                body,
                expected_version,
                create_only,
            }
        };
        assert!(
            store
                .put_task_state(bad("s1", json!({}), Some(1), true))
                .await
                .is_err(),
            "create_only + expected_version rejected"
        );
        assert!(
            store
                .put_task_state(bad("s2", json!({}), Some(0), false))
                .await
                .is_err(),
            "expected_version 0 rejected"
        );
        assert!(
            store
                .put_task_state(bad("s3", json!([1, 2]), None, true))
                .await
                .is_err(),
            "non-object body rejected"
        );
        assert!(
            store
                .put_task_state(bad(&"x".repeat(200), json!({}), None, false))
                .await
                .is_err(),
            "oversized slot rejected"
        );
        // Legacy upsert (create_only=false, no version) still replaces.
        let up = store
            .put_task_state(bad("s1", json!({"v": 9}), None, false))
            .await
            .unwrap();
        assert_eq!(up.version, 2);
        assert_eq!(up.body["v"], 9);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn task_state_create_only_race_one_winner() {
        use nomiso_core::task_state::PutTaskStateRequest;
        let store = setup(8).await;
        let n = 8usize;
        let mut handles = Vec::with_capacity(n);
        for i in 0..n {
            let store = store.clone();
            handles.push(tokio::spawn(async move {
                store
                    .put_task_state(PutTaskStateRequest {
                        scope: "org/cor".into(),
                        slot: "coding-wm".into(),
                        body: json!({"n": i}),
                        expected_version: None,
                        create_only: true,
                    })
                    .await
            }));
        }
        let mut oks = 0usize;
        let mut conflicts = 0usize;
        for h in handles {
            match h.await.expect("join") {
                Ok(_) => oks += 1,
                Err(Error::Conflict { expected: 0, .. }) => conflicts += 1,
                Err(e) => panic!("unexpected error (want Conflict expected=0 or Ok): {e:?}"),
            }
        }
        assert_eq!(
            oks, 1,
            "exactly one create_only winner; oks={oks} conflicts={conflicts}"
        );
        assert_eq!(conflicts, n - 1);
    }

    #[tokio::test]
    async fn idempotency_legacy_receipt_is_not_guessed() {
        let store = setup(8).await;
        let mut req = put_req("org/legacy", "retained legacy fact");
        req.idempotency_key = Some("old-key".into());
        let first = store.put(req.clone()).await.unwrap();
        let mut response = store.db.query("UPDATE idempotency_slot SET request_identity = NONE, receipt = NONE WHERE scope = $scope;")
            .bind(("scope", "org/legacy")).await.unwrap();
        ensure_ok(&mut response).unwrap();
        assert!(matches!(
            store.put(req).await,
            Err(Error::IdempotencyUnavailable)
        ));
        assert!(store.fetch_memory_raw(&first.id).await.unwrap().is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn idempotency_race_one_row() {
        let store = setup(8).await;
        let n = 8usize;
        let mut handles = Vec::with_capacity(n);
        for _ in 0..n {
            let store = store.clone();
            handles.push(tokio::spawn(async move {
                let mut req = put_req("org/idem", "same idempotent body");
                req.idempotency_key = Some("same-key".into());
                store.put(req).await
            }));
        }
        let mut ids = std::collections::HashSet::new();
        for h in handles {
            let wr = h.await.expect("join").expect("put");
            ids.insert(wr.id.to_string());
        }
        assert_eq!(ids.len(), 1, "one memory for one idempotency key: {ids:?}");
        let listed = store
            .list(nomiso_core::list::ListRequest {
                scope: "org/idem".into(),
                scope_match: ScopeMatch::Exact,
                categories: None,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
                text: None,
                limit: Some(32),
                cursor: None,
            })
            .await
            .unwrap();
        assert_eq!(listed.items.len(), 1, "list must show one row: {listed:?}");
    }

    #[tokio::test]
    async fn blake3_dedup_is_scoped() {
        use nomiso_core::evidence::PutArtifactRequest;
        let store = setup(8).await;
        let h = "ab".repeat(32);
        let a = store
            .put_artifact(PutArtifactRequest {
                scope: "org/team-a".into(),
                blake3: h.clone(),
                location: "file:///secret/team-a/report.pdf".into(),
                media_type: "application/pdf".into(),
                source: Some("team-a-source".into()),
                trust: Some(0.9),
            })
            .await
            .unwrap();
        let b = store
            .put_artifact(PutArtifactRequest {
                scope: "org/team-b".into(),
                blake3: h,
                location: "file:///team-b/copy.pdf".into(),
                media_type: "application/pdf".into(),
                source: Some("team-b".into()),
                trust: Some(0.1),
            })
            .await
            .unwrap();
        assert_ne!(a.id, b.id);
        assert_eq!(b.scope, "org/team-b");
        assert!(
            !b.location.contains("team-a"),
            "must not disclose other scope location: {b:?}"
        );
        assert_ne!(b.source.as_deref(), Some("team-a-source"));
    }

    #[tokio::test]
    async fn put_span_resolves_artifact_id() {
        use nomiso_core::evidence::{PutArtifactRequest, PutSpanRequest, SpanUnit};
        let store = setup(8).await;
        let art = store
            .put_artifact(PutArtifactRequest {
                scope: "org/e1".into(),
                blake3: "cd".repeat(32),
                location: "file:///e1".into(),
                media_type: "text/plain".into(),
                source: None,
                trust: None,
            })
            .await
            .unwrap();
        let span = store
            .put_span(PutSpanRequest {
                scope: "org/e1".into(),
                artifact_id: art.id.clone(),
                start: 0,
                end: 4,
                unit: SpanUnit::Byte,
            })
            .await
            .unwrap();
        assert!(span.artifact_id.contains("artifact"));
        let bare = artifact_bare_key(&span.artifact_id);
        let mut q = store
            .db
            .query("SELECT * FROM type::record('artifact', $art);")
            .bind(("art", bare))
            .await
            .unwrap();
        ensure_ok(&mut q).unwrap();
        let rows = take_json_rows(&mut q, 0).unwrap();
        assert_eq!(
            rows.len(),
            1,
            "span.artifact_id must address a live artifact"
        );
    }

    #[tokio::test]
    async fn mark_stale_does_not_cross_scope() {
        use nomiso_core::evidence::{DerivedFromTarget, LinkDerivedFromRequest};
        let store = setup(8).await;
        let prior = store.put(put_req("org/b", "prior in B")).await.unwrap();
        let dep = store
            .put(put_req("org/a", "dep in A derived from B"))
            .await
            .unwrap();
        store
            .link_derived_from(LinkDerivedFromRequest {
                memory_id: dep.id.clone(),
                scope: "org/a".into(),
                target: DerivedFromTarget::Memory {
                    id: prior.id.clone(),
                },
            })
            .await
            .unwrap();
        store
            .supersede(SupersedeRequest {
                prior_id: prior.id.clone(),
                expected_version: 1,
                new: put_req("org/b", "prior in B revised"),
                close_at: None,
            })
            .await
            .unwrap();
        let rows = store
            .read(ReadRequest {
                ids: vec![dep.id.clone()],
                scope: "org/a".into(),
                scope_match: ScopeMatch::Exact,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
            })
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_ne!(
            rows[0].stale,
            Some(true),
            "cross-scope stale flip is a write isolation bug: {:?}",
            rows[0]
        );
    }

    #[tokio::test]
    async fn list_count_honor_known_as_of() {
        let store = setup(8).await;
        let now_ts = now();
        let future = jiff::Timestamp::from_second(now_ts.as_second() + 86_400).unwrap();
        for i in 0..10 {
            let mut r = put_req("org/lens2", &format!("present {i}"));
            r.known_at = Some(now_ts);
            store.put(r).await.unwrap();
        }
        for i in 0..40 {
            let mut r = put_req("org/lens2", &format!("future {i}"));
            r.known_at = Some(future);
            r.valid_from = Some(future);
            store.put(r).await.unwrap();
        }
        let mid = jiff::Timestamp::from_second(now_ts.as_second() + 60).unwrap();
        let listed = store
            .list(nomiso_core::list::ListRequest {
                scope: "org/lens2".into(),
                scope_match: ScopeMatch::Exact,
                categories: None,
                as_of: Some(future),
                known_as_of: Some(mid),
                sys_as_of: None,
                text: None,
                limit: Some(32),
                cursor: None,
            })
            .await
            .unwrap();
        let n = store
            .count(nomiso_core::list::CountRequest {
                scope: "org/lens2".into(),
                scope_match: ScopeMatch::Exact,
                categories: None,
                as_of: Some(future),
                known_as_of: Some(mid),
                sys_as_of: None,
                text: None,
            })
            .await
            .unwrap();
        assert_eq!(
            listed.items.len(),
            10,
            "list under known_as_of must return 10 not 0: {}",
            listed.items.len()
        );
        assert_eq!(n, 10, "count under known_as_of must be 10, got {n}");
    }

    #[tokio::test]
    async fn annotate_merges_attrs() {
        use nomiso_core::evidence::AnnotateRequest;
        let store = setup(8).await;
        let mut req = put_req("org/ann", "attrs merge");
        req.content.attrs = Some(json!({"keep_me": true, "n": 1}));
        let wr = store.put(req).await.unwrap();
        store
            .annotate(AnnotateRequest {
                id: wr.id.clone(),
                scope: "org/ann".into(),
                expected_version: None,
                confidence: None,
                provenance_source: None,
                provenance_kind: None,
                attrs: Some(json!({"scan_count": 1})),
                bump_version: false,
            })
            .await
            .unwrap();
        let rows = store
            .read(ReadRequest {
                ids: vec![wr.id.clone()],
                scope: "org/ann".into(),
                scope_match: ScopeMatch::Exact,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
            })
            .await
            .unwrap();
        let attrs = rows[0].content.attrs.as_ref().expect("attrs");
        assert_eq!(attrs.get("keep_me"), Some(&json!(true)));
        assert_eq!(attrs.get("scan_count"), Some(&json!(1)));
        let evs = store.list_belief_events(&wr.id, "org/ann").await.unwrap();
        assert!(
            evs.iter().any(|e| e.kind.as_str() == "annotate"),
            "bump=false must still journal: {evs:?}"
        );
    }

    #[tokio::test]
    async fn trace_foreign_scope_rejected() {
        use nomiso_core::trace::{AppendTraceEvent, TraceEventKind};
        let store = setup(8).await;
        let tid = uuid::Uuid::now_v7().to_string();
        store
            .append_trace_event(AppendTraceEvent {
                trace_id: tid.clone(),
                scope: "org/owner".into(),
                kind: TraceEventKind::Search,
                session_id: None,
                turn_id: None,
                payload: None,
                memory_ids: vec![],
            })
            .await
            .unwrap();
        let err = store
            .append_trace_event(AppendTraceEvent {
                trace_id: tid.clone(),
                scope: "org/attacker".into(),
                kind: TraceEventKind::Search,
                session_id: None,
                turn_id: None,
                payload: None,
                memory_ids: vec![],
            })
            .await;
        assert!(
            matches!(err, Err(Error::ScopeDenied(_))),
            "foreign-scope append must ScopeDenied, got {err:?}"
        );
        let bundle = store.get_trace(&tid, "org/owner").await.unwrap();
        assert_eq!(bundle.events.len(), 1);
    }

    #[tokio::test]
    async fn list_traces_scope_inventory() {
        use nomiso_core::trace::{AppendTraceEvent, ListTracesRequest, TraceEventKind};
        let store = setup(8).await;
        let a = uuid::Uuid::now_v7().to_string();
        let b = uuid::Uuid::now_v7().to_string();
        store
            .append_trace_event(AppendTraceEvent {
                trace_id: a.clone(),
                scope: "org/lt".into(),
                kind: TraceEventKind::Search,
                session_id: None,
                turn_id: None,
                payload: None,
                memory_ids: vec![],
            })
            .await
            .unwrap();
        store
            .append_trace_event(AppendTraceEvent {
                trace_id: b.clone(),
                scope: "org/lt".into(),
                kind: TraceEventKind::Write,
                session_id: None,
                turn_id: None,
                payload: None,
                memory_ids: vec![],
            })
            .await
            .unwrap();
        store
            .append_trace_event(AppendTraceEvent {
                trace_id: uuid::Uuid::now_v7().to_string(),
                scope: "org/other".into(),
                kind: TraceEventKind::Search,
                session_id: None,
                turn_id: None,
                payload: None,
                memory_ids: vec![],
            })
            .await
            .unwrap();
        let rows = store
            .list_traces(ListTracesRequest {
                scope: "org/lt".into(),
                limit: Some(16),
                ..Default::default()
            })
            .await
            .unwrap();
        let ids: std::collections::HashSet<_> = rows.iter().map(|r| r.trace_id.as_str()).collect();
        assert!(
            ids.contains(a.as_str()) && ids.contains(b.as_str()),
            "{rows:?}"
        );
        assert_eq!(rows.len(), 2, "{rows:?}");
    }

    #[tokio::test]
    async fn list_traces_filters_session_and_turn() {
        use nomiso_core::trace::{AppendTraceEvent, ListTracesRequest, TraceEventKind};
        let store = setup(8).await;
        let a = uuid::Uuid::now_v7().to_string();
        let b = uuid::Uuid::now_v7().to_string();
        store
            .append_trace_event(AppendTraceEvent {
                trace_id: a.clone(),
                scope: "org/ltf".into(),
                kind: TraceEventKind::Search,
                session_id: Some("sess-a".into()),
                turn_id: Some("turn-1".into()),
                payload: None,
                memory_ids: vec![],
            })
            .await
            .unwrap();
        store
            .append_trace_event(AppendTraceEvent {
                trace_id: b.clone(),
                scope: "org/ltf".into(),
                kind: TraceEventKind::Search,
                session_id: Some("sess-b".into()),
                turn_id: Some("turn-2".into()),
                payload: None,
                memory_ids: vec![],
            })
            .await
            .unwrap();
        let by_sess = store
            .list_traces(ListTracesRequest {
                scope: "org/ltf".into(),
                session_id: Some("sess-a".into()),
                limit: Some(16),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(by_sess.len(), 1, "{by_sess:?}");
        assert_eq!(by_sess[0].trace_id, a);
        let by_turn = store
            .list_traces(ListTracesRequest {
                scope: "org/ltf".into(),
                turn_id: Some("turn-2".into()),
                limit: Some(16),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(by_turn.len(), 1, "{by_turn:?}");
        assert_eq!(by_turn[0].trace_id, b);
    }

    #[tokio::test]
    async fn list_traces_for_memory_joins_inject_and_outcome() {
        use nomiso_core::trace::{
            AppendTraceEvent, TraceEventKind, TraceOutcome, TracesByMemoryRequest,
        };
        let store = setup(8).await;
        let mid = MemoryId::new(uuid::Uuid::now_v7().to_string());
        let other = MemoryId::new(uuid::Uuid::now_v7().to_string());
        let hit = uuid::Uuid::now_v7().to_string();
        let miss = uuid::Uuid::now_v7().to_string();
        store
            .append_trace_event(AppendTraceEvent {
                trace_id: hit.clone(),
                scope: "org/tbm".into(),
                kind: TraceEventKind::Inject,
                session_id: None,
                turn_id: None,
                payload: Some(serde_json::json!({ "memory_ids": [mid.to_string()] })),
                memory_ids: vec![],
            })
            .await
            .unwrap();
        store
            .append_trace_event(AppendTraceEvent {
                trace_id: hit.clone(),
                scope: "org/tbm".into(),
                kind: TraceEventKind::Outcome,
                session_id: None,
                turn_id: None,
                payload: Some(serde_json::json!({ "outcome": "helped", "note": "used" })),
                memory_ids: vec![],
            })
            .await
            .unwrap();
        store
            .append_trace_event(AppendTraceEvent {
                trace_id: miss.clone(),
                scope: "org/tbm".into(),
                kind: TraceEventKind::Inject,
                session_id: None,
                turn_id: None,
                payload: Some(serde_json::json!({ "memory_ids": [other.to_string()] })),
                memory_ids: vec![],
            })
            .await
            .unwrap();
        store
            .append_trace_event(AppendTraceEvent {
                trace_id: miss.clone(),
                scope: "org/tbm".into(),
                kind: TraceEventKind::Outcome,
                session_id: None,
                turn_id: None,
                payload: Some(serde_json::json!({ "outcome": "helped" })),
                memory_ids: vec![],
            })
            .await
            .unwrap();

        let rows = store
            .list_traces_for_memory(TracesByMemoryRequest {
                scope: "org/tbm".into(),
                memory_id: mid.clone(),
                limit: Some(16),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].trace_id, hit);
        assert!(
            rows[0].matched_kinds.contains(&TraceEventKind::Inject),
            "{rows:?}"
        );
        assert_eq!(rows[0].outcome, Some(TraceOutcome::Helped));
        assert_eq!(rows[0].outcome_note.as_deref(), Some("used"));

        let prefixed = MemoryId::new(format!("memory:{}", mid.bare_key()));
        let by_pref = store
            .list_traces_for_memory(TracesByMemoryRequest {
                scope: "org/tbm".into(),
                memory_id: prefixed,
                limit: Some(16),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(by_pref.len(), 1, "{by_pref:?}");
        assert_eq!(by_pref[0].trace_id, hit);
        assert_eq!(by_pref[0].outcome, Some(TraceOutcome::Helped));

        let foreign = store
            .list_traces_for_memory(TracesByMemoryRequest {
                scope: "org/other".into(),
                memory_id: mid,
                limit: Some(16),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(
            foreign.is_empty(),
            "foreign scope must not leak: {foreign:?}"
        );
    }

    #[tokio::test]
    async fn list_traces_for_memory_sees_search_hit_card() {
        use nomiso_core::trace::{AppendTraceEvent, TraceEventKind, TracesByMemoryRequest};
        let store = setup(8).await;
        let mid = MemoryId::new(uuid::Uuid::now_v7().to_string());
        let tid = uuid::Uuid::now_v7().to_string();
        store
            .append_trace_event(AppendTraceEvent {
                trace_id: tid.clone(),
                scope: "org/tbm-hit".into(),
                kind: TraceEventKind::Search,
                session_id: None,
                turn_id: None,
                payload: Some(serde_json::json!({
                    "hits": [{
                        "id": mid.to_string(),
                        "rank": 0,
                        "score": 1.0,
                        "score_kind": "engine",
                        "preview": "hit card"
                    }]
                })),
                memory_ids: vec![],
            })
            .await
            .unwrap();
        let rows = store
            .list_traces_for_memory(TracesByMemoryRequest {
                scope: "org/tbm-hit".into(),
                memory_id: mid,
                limit: Some(8),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].trace_id, tid);
        assert!(
            rows[0].matched_kinds.contains(&TraceEventKind::Search),
            "{rows:?}"
        );
    }

    // ---- Phase 4: typed relationships ----

    use nomiso_core::relationship::{
        EndpointKind as EK, EndpointRef, EpistemicStatus, ListRelationshipsRequest,
        PutEntityRequest, PutRelationshipRequest, RelationPredicate, RelationshipState,
        TraverseBudget, TraverseDirection, TraverseRequest, UpdateEntityRequest,
        UpdateRelationshipRequest,
    };

    fn ent_req(scope: &str, name: &str) -> PutEntityRequest {
        PutEntityRequest {
            scope: scope.into(),
            kind: "person".into(),
            name: name.into(),
            aliases: vec![],
            attrs: None,
        }
    }

    fn rel_req(
        scope: &str,
        predicate: RelationPredicate,
        subject: EndpointRef,
        object: EndpointRef,
    ) -> PutRelationshipRequest {
        PutRelationshipRequest {
            scope: scope.into(),
            predicate,
            subject,
            object,
            subject_rev: None,
            object_rev: None,
            epistemic: EpistemicStatus::Reported,
            evidence: vec![],
            valid_from: None,
            valid_until: None,
            producer: None,
            dedupe: true,
        }
    }

    #[tokio::test]
    async fn entity_put_get_update_cas() {
        let store = setup(8).await;
        let e = store
            .put_entity(ent_req("org/ent", "Alice"))
            .await
            .expect("put_entity");
        assert_eq!(e.version, 1);
        let got = store.get_entity(&e.id, "org/ent").await.unwrap();
        assert_eq!(got.name, "Alice");
        // Cross-scope read is denied, not merely hidden.
        assert!(matches!(
            store.get_entity(&e.id, "org/other").await,
            Err(Error::ScopeDenied(_))
        ));
        let upd = store
            .update_entity(UpdateEntityRequest {
                id: e.id.clone(),
                scope: "org/ent".into(),
                expected_version: 1,
                name: Some("Alice B".into()),
                aliases: None,
                attrs: None,
            })
            .await
            .unwrap();
        assert_eq!(upd.version, 2);
        assert_eq!(upd.name, "Alice B");
        // Stale version conflicts.
        assert!(matches!(
            store
                .update_entity(UpdateEntityRequest {
                    id: e.id.clone(),
                    scope: "org/ent".into(),
                    expected_version: 1,
                    name: Some("nope".into()),
                    aliases: None,
                    attrs: None,
                })
                .await,
            Err(Error::Conflict { .. })
        ));
    }

    #[tokio::test]
    async fn rel_registry_rejects_illtyped_and_foundation_edges() {
        let store = setup(8).await;
        let a = store.put(put_req("org/r", "a")).await.unwrap();
        let b = store.put(put_req("org/r", "b")).await.unwrap();
        // `mentions` requires object kind entity, not memory.
        assert!(matches!(
            store
                .put_relationship(rel_req(
                    "org/r",
                    RelationPredicate::Mentions,
                    EndpointRef::new(EK::Memory, a.id.to_string()),
                    EndpointRef::new(EK::Memory, b.id.to_string()),
                ))
                .await,
            Err(Error::InvalidOp(_))
        ));
        // `supersedes` is foundation-maintained lineage, not a free-form edge.
        assert!(matches!(
            store
                .put_relationship(rel_req(
                    "org/r",
                    RelationPredicate::Supersedes,
                    EndpointRef::new(EK::Memory, a.id.to_string()),
                    EndpointRef::new(EK::Memory, b.id.to_string()),
                ))
                .await,
            Err(Error::InvalidOp(_))
        ));
        // Unknown predicate strings never decode as a valid edge.
        assert!(RelationPredicate::parse("correlates_with").is_none());
        // Mismatched kind prefix in an endpoint id is rejected.
        assert!(EndpointRef::new(EK::Memory, "entity:abc".to_string())
            .bare_key()
            .is_err());
    }

    #[tokio::test]
    async fn rel_requires_existing_same_scope_endpoints() {
        let store = setup(8).await;
        let m = store.put(put_req("org/rs", "fact")).await.unwrap();
        let foreign = store.put(put_req("org/other-scope", "x")).await.unwrap();
        // Missing endpoint.
        assert!(matches!(
            store
                .put_relationship(rel_req(
                    "org/rs",
                    RelationPredicate::Contradicts,
                    EndpointRef::new(EK::Memory, m.id.to_string()),
                    EndpointRef::new(EK::Memory, "missing-key".to_string()),
                ))
                .await,
            Err(Error::NotFound(_))
        ));
        // Cross-scope endpoint rejected.
        assert!(matches!(
            store
                .put_relationship(rel_req(
                    "org/rs",
                    RelationPredicate::Contradicts,
                    EndpointRef::new(EK::Memory, m.id.to_string()),
                    EndpointRef::new(EK::Memory, foreign.id.to_string()),
                ))
                .await,
            Err(Error::ScopeDenied(_))
        ));
    }

    #[tokio::test]
    async fn rel_put_get_dedupe_and_conflict() {
        let store = setup(8).await;
        let a = store.put(put_req("org/rd", "claim a")).await.unwrap();
        let b = store.put(put_req("org/rd", "claim b")).await.unwrap();
        let req = rel_req(
            "org/rd",
            RelationPredicate::Contradicts,
            EndpointRef::new(EK::Memory, a.id.to_string()),
            EndpointRef::new(EK::Memory, b.id.to_string()),
        );
        let w1 = store.put_relationship(req.clone()).await.unwrap();
        assert!(!w1.replayed);
        assert_eq!(w1.record.version, 1);
        // Identical create replays the same edge.
        let w2 = store.put_relationship(req.clone()).await.unwrap();
        assert!(w2.replayed);
        assert_eq!(w2.record.id, w1.record.id);
        // Same live identity with a divergent payload conflicts.
        let mut divergent = req.clone();
        divergent.epistemic = EpistemicStatus::Verified;
        assert!(matches!(
            store.put_relationship(divergent).await,
            Err(Error::IdempotencyConflict)
        ));
        // get is scope-checked.
        let got = store
            .get_relationship(&w1.record.id, "org/rd")
            .await
            .unwrap();
        assert_eq!(got.predicate, RelationPredicate::Contradicts);
        assert!(matches!(
            store.get_relationship(&w1.record.id, "org/else").await,
            Err(Error::ScopeDenied(_))
        ));
    }

    #[tokio::test]
    async fn rel_update_cas_audits_and_guards() {
        let store = setup(8).await;
        let a = store.put(put_req("org/ru", "a")).await.unwrap();
        let b = store.put(put_req("org/ru", "b")).await.unwrap();
        let w = store
            .put_relationship(rel_req(
                "org/ru",
                RelationPredicate::Contradicts,
                EndpointRef::new(EK::Memory, a.id.to_string()),
                EndpointRef::new(EK::Memory, b.id.to_string()),
            ))
            .await
            .unwrap();
        // Closing requires a reason.
        assert!(matches!(
            store
                .update_relationship(UpdateRelationshipRequest {
                    id: w.record.id.clone(),
                    scope: "org/ru".into(),
                    expected_version: 1,
                    epistemic: None,
                    state: Some(RelationshipState::Closed),
                    state_reason: None,
                    valid_until: None,
                    evidence: None,
                })
                .await,
            Err(Error::InvalidOp(_))
        ));
        // Public purge is rejected.
        assert!(matches!(
            store
                .update_relationship(UpdateRelationshipRequest {
                    id: w.record.id.clone(),
                    scope: "org/ru".into(),
                    expected_version: 1,
                    epistemic: None,
                    state: Some(RelationshipState::Purged),
                    state_reason: Some("x".into()),
                    valid_until: None,
                    evidence: None,
                })
                .await,
            Err(Error::InvalidOp(_))
        ));
        let closed = store
            .update_relationship(UpdateRelationshipRequest {
                id: w.record.id.clone(),
                scope: "org/ru".into(),
                expected_version: 1,
                epistemic: Some(EpistemicStatus::Verified),
                state: Some(RelationshipState::Closed),
                state_reason: Some("resolved".into()),
                valid_until: None,
                evidence: None,
            })
            .await
            .unwrap();
        assert_eq!(closed.version, 2);
        assert_eq!(closed.state, RelationshipState::Closed);
        // Prior revision is preserved in the audit event.
        let mut r = store
            .db
            .query("SELECT * FROM relationship_event WHERE relationship_id = $rid ORDER BY at_sys;")
            .bind(("rid", w.record.id.clone()))
            .await
            .unwrap();
        ensure_ok(&mut r).unwrap();
        let events = take_json_rows(&mut r, 0).unwrap();
        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(events[1]["kind"], "update");
        assert_eq!(events[1]["payload"]["prior"]["version"], 1);
        // Stale CAS conflicts.
        assert!(matches!(
            store
                .update_relationship(UpdateRelationshipRequest {
                    id: w.record.id.clone(),
                    scope: "org/ru".into(),
                    expected_version: 1,
                    epistemic: None,
                    state: None,
                    state_reason: None,
                    valid_until: None,
                    evidence: None,
                })
                .await,
            Err(Error::Conflict { .. })
        ));
        // Closed edges free the live key: the same edge can be re-asserted.
        let w2 = store
            .put_relationship(rel_req(
                "org/ru",
                RelationPredicate::Contradicts,
                EndpointRef::new(EK::Memory, a.id.to_string()),
                EndpointRef::new(EK::Memory, b.id.to_string()),
            ))
            .await
            .unwrap();
        assert!(!w2.replayed);
        assert_ne!(w2.record.id, w.record.id);
    }

    #[tokio::test]
    async fn rel_supersede_and_erase_mark_edges() {
        let store = setup(8).await;
        use nomiso_core::ops::SupersedeRequest;
        let src = store.put(put_req("org/rm", "source fact")).await.unwrap();
        let view = store.put(put_req("org/rm", "derived view")).await.unwrap();
        let w = store
            .put_relationship(rel_req(
                "org/rm",
                RelationPredicate::DerivedFrom,
                EndpointRef::new(EK::Memory, view.id.to_string()),
                EndpointRef::new(EK::Memory, src.id.to_string()),
            ))
            .await
            .unwrap();
        // Superseding the source marks the derived edge stale atomically.
        store
            .supersede(SupersedeRequest {
                prior_id: src.id.clone(),
                expected_version: 1,
                new: put_req("org/rm", "corrected source"),
                close_at: None,
            })
            .await
            .unwrap();
        let stale = store
            .get_relationship(&w.record.id, "org/rm")
            .await
            .unwrap();
        assert_eq!(stale.state, RelationshipState::Stale);
        assert_eq!(stale.state_reason.as_deref(), Some("endpoint_superseded"));
        // Hard erase purges referencing edges in the same commit.
        let w2 = store
            .put_relationship(rel_req(
                "org/rm",
                RelationPredicate::DerivedFrom,
                EndpointRef::new(EK::Memory, view.id.to_string()),
                EndpointRef::new(EK::Memory, w.record.object.id.clone()),
            ))
            .await
            .unwrap();
        assert!(!w2.replayed, "stale edge freed the live key");
        store
            .forget(nomiso_core::ops::ForgetRequest {
                id: view.id.clone(),
                scope: "org/rm".into(),
                hard: true,
                expected_version: None,
                at: None,
            })
            .await
            .unwrap();
        let purged = store
            .get_relationship(&w2.record.id, "org/rm")
            .await
            .unwrap();
        assert_eq!(purged.state, RelationshipState::Purged);
        assert_eq!(purged.state_reason.as_deref(), Some("endpoint_erased"));
    }

    #[tokio::test]
    async fn traverse_walks_depth_direction_and_paths() {
        let store = setup(8).await;
        // a -> derived_from -> b -> derived_from -> c chain.
        let c = store.put(put_req("org/tv", "root evidence")).await.unwrap();
        let b = store.put(put_req("org/tv", "mid view")).await.unwrap();
        let a = store.put(put_req("org/tv", "top view")).await.unwrap();
        for (s, o) in [(&a, &b), (&b, &c)] {
            store
                .put_relationship(rel_req(
                    "org/tv",
                    RelationPredicate::DerivedFrom,
                    EndpointRef::new(EK::Memory, s.id.to_string()),
                    EndpointRef::new(EK::Memory, o.id.to_string()),
                ))
                .await
                .unwrap();
        }
        let seed = EndpointRef::new(EK::Memory, a.id.to_string());
        let res = store
            .traverse(TraverseRequest {
                scope: "org/tv".into(),
                seeds: vec![seed.clone()],
                predicates: Some(vec![RelationPredicate::DerivedFrom]),
                direction: TraverseDirection::Out,
                states: None,
                budget: TraverseBudget::default(),
            })
            .await
            .unwrap();
        assert_eq!(res.nodes.len(), 2, "{:?}", res.nodes);
        assert_eq!(res.edges.len(), 2);
        assert!(res.truncated.is_empty(), "{:?}", res.truncated);
        // Path provenance: c is reached through both edges in order.
        let cnode = res
            .nodes
            .iter()
            .find(|n| n.endpoint.id == c.id.bare_key())
            .expect("c reached");
        assert_eq!(cnode.depth, 2);
        assert_eq!(cnode.path.len(), 2);
        // Depth 1 stops at b and reports the depth bound.
        let shallow = store
            .traverse(TraverseRequest {
                scope: "org/tv".into(),
                seeds: vec![seed.clone()],
                predicates: None,
                direction: TraverseDirection::Out,
                states: None,
                budget: TraverseBudget {
                    max_depth: Some(1),
                    ..Default::default()
                },
            })
            .await
            .unwrap();
        assert_eq!(shallow.nodes.len(), 1);
        assert!(shallow.truncated.contains(&"depth".to_string()));
        // Inbound direction walks the chain backward from c.
        let back = store
            .traverse(TraverseRequest {
                scope: "org/tv".into(),
                seeds: vec![EndpointRef::new(EK::Memory, c.id.to_string())],
                predicates: Some(vec![RelationPredicate::DerivedFrom]),
                direction: TraverseDirection::In,
                states: None,
                budget: TraverseBudget::default(),
            })
            .await
            .unwrap();
        assert_eq!(back.nodes.len(), 2);
    }

    #[tokio::test]
    async fn traverse_handles_cycles_and_budgets() {
        let store = setup(8).await;
        let a = store.put(put_req("org/tc", "a")).await.unwrap();
        let b = store.put(put_req("org/tc", "b")).await.unwrap();
        // Cycle: a contradicts b (symmetric predicate).
        store
            .put_relationship(rel_req(
                "org/tc",
                RelationPredicate::Contradicts,
                EndpointRef::new(EK::Memory, a.id.to_string()),
                EndpointRef::new(EK::Memory, b.id.to_string()),
            ))
            .await
            .unwrap();
        let res = store
            .traverse(TraverseRequest {
                scope: "org/tc".into(),
                seeds: vec![EndpointRef::new(EK::Memory, a.id.to_string())],
                predicates: Some(vec![RelationPredicate::Contradicts]),
                direction: TraverseDirection::Out,
                states: None,
                budget: TraverseBudget {
                    max_depth: Some(8),
                    ..Default::default()
                },
            })
            .await
            .unwrap();
        // Cycle dedupes: b reached once, no infinite walk.
        assert_eq!(res.nodes.len(), 1);
        assert_eq!(res.visited, 2);
        // Edge budget truncates.
        let tight = store
            .traverse(TraverseRequest {
                scope: "org/tc".into(),
                seeds: vec![EndpointRef::new(EK::Memory, a.id.to_string())],
                predicates: None,
                direction: TraverseDirection::Both,
                states: None,
                budget: TraverseBudget {
                    max_edges: Some(1),
                    ..Default::default()
                },
            })
            .await
            .unwrap();
        assert!(tight.truncated.contains(&"edges".to_string()));
        // Closed edges are not traversed by default.
        let w = store
            .put_relationship(rel_req(
                "org/tc",
                RelationPredicate::Contradicts,
                EndpointRef::new(EK::Memory, b.id.to_string()),
                EndpointRef::new(EK::Memory, a.id.to_string()),
            ))
            .await
            .unwrap();
        store
            .update_relationship(UpdateRelationshipRequest {
                id: w.record.id.clone(),
                scope: "org/tc".into(),
                expected_version: 1,
                epistemic: None,
                state: Some(RelationshipState::Closed),
                state_reason: Some("done".into()),
                valid_until: None,
                evidence: None,
            })
            .await
            .unwrap();
        let only_active = store
            .traverse(TraverseRequest {
                scope: "org/tc".into(),
                seeds: vec![EndpointRef::new(EK::Memory, a.id.to_string())],
                predicates: None,
                direction: TraverseDirection::Both,
                states: None,
                budget: TraverseBudget::default(),
            })
            .await
            .unwrap();
        assert_eq!(only_active.edges.len(), 1, "{:?}", only_active.edges);
        // But explicit state selection includes it.
        let all_states = store
            .traverse(TraverseRequest {
                scope: "org/tc".into(),
                seeds: vec![EndpointRef::new(EK::Memory, a.id.to_string())],
                predicates: None,
                direction: TraverseDirection::Both,
                states: Some(vec![RelationshipState::Active, RelationshipState::Closed]),
                budget: TraverseBudget::default(),
            })
            .await
            .unwrap();
        assert_eq!(all_states.edges.len(), 2);
        // Missing and cross-scope seeds fail before traversal.
        assert!(matches!(
            store
                .traverse(TraverseRequest {
                    scope: "org/tc".into(),
                    seeds: vec![EndpointRef::new(EK::Memory, "nope".to_string())],
                    predicates: None,
                    direction: TraverseDirection::Out,
                    states: None,
                    budget: TraverseBudget::default(),
                })
                .await,
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            store
                .traverse(TraverseRequest {
                    scope: "org/other".into(),
                    seeds: vec![EndpointRef::new(EK::Memory, a.id.to_string())],
                    predicates: None,
                    direction: TraverseDirection::Out,
                    states: None,
                    budget: TraverseBudget::default(),
                })
                .await,
            Err(Error::ScopeDenied(_))
        ));
    }

    #[tokio::test]
    async fn rel_list_filters_endpoint_predicate_state() {
        let store = setup(8).await;
        let a = store.put(put_req("org/rl", "a")).await.unwrap();
        let b = store.put(put_req("org/rl", "b")).await.unwrap();
        let ent = store.put_entity(ent_req("org/rl", "Bob")).await.unwrap();
        store
            .put_relationship(rel_req(
                "org/rl",
                RelationPredicate::Contradicts,
                EndpointRef::new(EK::Memory, a.id.to_string()),
                EndpointRef::new(EK::Memory, b.id.to_string()),
            ))
            .await
            .unwrap();
        store
            .put_relationship(rel_req(
                "org/rl",
                RelationPredicate::Mentions,
                EndpointRef::new(EK::Memory, a.id.to_string()),
                EndpointRef::new(EK::Entity, ent.id.clone()),
            ))
            .await
            .unwrap();
        // Endpoint filter sees both edges touching a.
        let touching_a = store
            .list_relationships(ListRelationshipsRequest {
                scope: "org/rl".into(),
                endpoint: Some(EndpointRef::new(EK::Memory, a.id.to_string())),
                predicate: None,
                state: None,
                limit: None,
            })
            .await
            .unwrap();
        assert_eq!(touching_a.len(), 2);
        // Predicate filter narrows to one.
        let only_mentions = store
            .list_relationships(ListRelationshipsRequest {
                scope: "org/rl".into(),
                endpoint: None,
                predicate: Some(RelationPredicate::Mentions),
                state: None,
                limit: None,
            })
            .await
            .unwrap();
        assert_eq!(only_mentions.len(), 1);
        assert_eq!(
            only_mentions[0].object.kind,
            nomiso_core::relationship::EndpointKind::Entity
        );
        // Other scopes see nothing.
        assert!(store
            .list_relationships(ListRelationshipsRequest {
                scope: "org/else".into(),
                ..Default::default()
            })
            .await
            .unwrap()
            .is_empty());
    }

    // ---- Embedding identity / generations (MIG-004/005) ----

    fn test_identity(dim: u32, model: &str) -> nomiso_core::embedding::EmbeddingIdentity {
        nomiso_core::embedding::EmbeddingIdentity {
            family: "test-family".into(),
            model: model.into(),
            dimension: dim,
            normalization: nomiso_core::embedding::EmbeddingNormalization::L2,
            encoding: "f32".into(),
            limitation: None,
        }
    }

    fn vec_of(dim: usize, v: f32) -> Vec<f32> {
        vec![v; dim]
    }

    #[tokio::test]
    async fn egen_gen1_created_unknown_on_fresh_store() {
        let store = setup(8).await;
        let state = store.embedding_state().await.unwrap();
        let active = state.active.expect("active generation");
        assert_eq!(active.generation, 1);
        assert!(active.identity.is_unknown(), "{:?}", active.identity);
        assert_eq!(active.identity.dimension, 8);
        assert_eq!(state.generations.len(), 1);
    }

    #[tokio::test]
    async fn egen_declared_identity_bootstraps_fresh() {
        let mut cfg = StoreConfig::memory_test(8);
        cfg.embedding_identity = Some(test_identity(8, "model-a"));
        let store = SurrealMemoryStore::connect(cfg).await.unwrap();
        store.migrate().await.unwrap();
        let active = store.embedding_state().await.unwrap().active.unwrap();
        assert_eq!(active.identity.model, "model-a");
        assert!(!active.identity.is_unknown());
    }

    #[tokio::test]
    async fn egen_declared_mismatch_fails_closed_on_migrate() {
        let mut cfg = StoreConfig::memory_test(8);
        cfg.embedding_identity = Some(test_identity(8, "model-a"));
        let store = SurrealMemoryStore::connect(cfg.clone()).await.unwrap();
        store.migrate().await.unwrap();
        // Same store, same dim, different model — must not silently mix.
        let mut cfg2 = cfg.clone();
        cfg2.embedding_identity = Some(test_identity(8, "model-b"));
        let store2 = SurrealMemoryStore {
            db: store.db.clone(),
            config: cfg2,
        };
        let err = store2.migrate().await.unwrap_err();
        assert!(
            matches!(err, Error::IncompatibleStore(_)),
            "expected incompatible_store, got {err:?}"
        );
    }

    #[tokio::test]
    async fn egen_unknown_legacy_requires_attestation() {
        let store = setup(8).await;
        // Legacy-style write: a vector with no claimed identity.
        let mut req = put_req("org/e", "legacy embedded row");
        req.embedding = Some(vec_of(8, 0.1));
        store.put(req).await.unwrap();
        // Declaring an identity over unknown legacy vectors is refused.
        let mut cfg2 = StoreConfig::memory_test(8);
        cfg2.embedding_identity = Some(test_identity(8, "model-a"));
        let store2 = SurrealMemoryStore {
            db: store.db.clone(),
            config: cfg2,
        };
        let err = store2.migrate().await.unwrap_err();
        assert!(matches!(err, Error::IncompatibleStore(_)), "{err:?}");
        // Explicit operator attestation resolves it.
        let gen = store
            .attest_embedding_identity(test_identity(8, "model-a"))
            .await
            .unwrap();
        assert_eq!(gen.identity.model, "model-a");
        // Now the declared open succeeds.
        store2.migrate().await.unwrap();
    }

    #[tokio::test]
    async fn egen_write_stamps_generation_and_hit_reports_it() {
        let store = setup(8).await;
        let mut req = put_req("org/e", "stamped row");
        req.embedding = Some(vec_of(8, 0.5));
        let wr = store.put(req).await.unwrap();
        let rec = wr.record.unwrap();
        assert_eq!(rec.embedding_generation, Some(1));
        let hits = store
            .search(SearchQuery {
                query: "stamped".into(),
                scope: "org/e".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].embedding_generation, Some(1));
    }

    #[tokio::test]
    async fn egen_claimed_identity_adopts_then_enforces() {
        let store = setup(8).await;
        // First write with a claimed identity adopts it into gen 1.
        let mut req = put_req("org/e", "first");
        req.embedding = Some(vec_of(8, 0.5));
        req.embedding_identity = Some(test_identity(8, "model-x"));
        store.put(req).await.unwrap();
        let gen = store.embedding_state().await.unwrap().active.unwrap();
        assert_eq!(gen.identity.model, "model-x");
        // A different claimed identity on a later write is refused.
        let mut req2 = put_req("org/e", "second");
        req2.embedding = Some(vec_of(8, 0.5));
        req2.embedding_identity = Some(test_identity(8, "model-y"));
        let err = store.put(req2).await.unwrap_err();
        assert!(matches!(err, Error::IncompatibleStore(_)), "{err:?}");
    }

    #[tokio::test]
    async fn egen_staged_reindex_activate_swaps_vectors() {
        let store = setup(4).await;
        let mut r1 = put_req("org/e", "one");
        r1.embedding = Some(vec_of(4, 1.0));
        let m1 = store.put(r1).await.unwrap().id;
        let mut r2 = put_req("org/e", "two");
        r2.embedding = Some(vec_of(4, 0.0));
        let m2 = store.put(r2).await.unwrap().id;

        let gen2 = store
            .declare_embedding_generation(nomiso_core::embedding::DeclareGenerationRequest {
                identity: test_identity(4, "model-b"),
                note: Some("reindex".into()),
            })
            .await
            .unwrap();
        assert_eq!(gen2.generation, 2);
        assert_eq!(
            gen2.status,
            nomiso_core::embedding::GenerationStatus::Building
        );
        assert_eq!(gen2.source_frontier.unwrap().expected_count, 2);

        let staged = store
            .stage_embeddings(
                2,
                vec![
                    nomiso_core::embedding::StagedEmbedding {
                        memory: m1.clone(),
                        vector: vec_of(4, 9.0),
                    },
                    nomiso_core::embedding::StagedEmbedding {
                        memory: m2.clone(),
                        vector: vec_of(4, 8.0),
                    },
                ],
            )
            .await
            .unwrap();
        assert_eq!(staged, 2);

        let activated = store.activate_embedding_generation(2).await.unwrap();
        assert_eq!(
            activated.status,
            nomiso_core::embedding::GenerationStatus::Active
        );
        let state = store.embedding_state().await.unwrap();
        assert_eq!(state.active.unwrap().generation, 2);
        assert_eq!(
            state.generations[0].status,
            nomiso_core::embedding::GenerationStatus::Retired
        );
        // Vectors actually swapped onto the live rows.
        let rec = store
            .read(ReadRequest {
                ids: vec![m1],
                scope: "org/e".into(),
                scope_match: ScopeMatch::Exact,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
            })
            .await
            .unwrap()
            .remove(0);
        assert_eq!(rec.embedding, Some(vec_of(4, 9.0)));
        assert_eq!(rec.embedding_generation, Some(2));
    }

    #[tokio::test]
    async fn egen_activate_incomplete_generation_refused() {
        let store = setup(4).await;
        let mut r1 = put_req("org/e", "one");
        r1.embedding = Some(vec_of(4, 1.0));
        let m1 = store.put(r1).await.unwrap().id;
        let mut r2 = put_req("org/e", "two");
        r2.embedding = Some(vec_of(4, 0.0));
        store.put(r2).await.unwrap();

        store
            .declare_embedding_generation(nomiso_core::embedding::DeclareGenerationRequest {
                identity: test_identity(4, "model-b"),
                note: None,
            })
            .await
            .unwrap();
        store
            .stage_embeddings(
                2,
                vec![nomiso_core::embedding::StagedEmbedding {
                    memory: m1,
                    vector: vec_of(4, 9.0),
                }],
            )
            .await
            .unwrap();
        let err = store.activate_embedding_generation(2).await.unwrap_err();
        assert!(matches!(err, Error::InvalidOp(_)), "{err:?}");
        // Still building; generation 1 remains active.
        let state = store.embedding_state().await.unwrap();
        assert_eq!(state.active.unwrap().generation, 1);
        assert_eq!(
            state.generations[1].status,
            nomiso_core::embedding::GenerationStatus::Building
        );
    }

    #[tokio::test]
    async fn egen_stage_validates_status_dim_and_memory() {
        let store = setup(4).await;
        let mut r1 = put_req("org/e", "one");
        r1.embedding = Some(vec_of(4, 1.0));
        let m1 = store.put(r1).await.unwrap().id;
        // Staging into the active generation is refused.
        let err = store
            .stage_embeddings(
                1,
                vec![nomiso_core::embedding::StagedEmbedding {
                    memory: m1.clone(),
                    vector: vec_of(4, 9.0),
                }],
            )
            .await
            .unwrap_err();
        assert!(matches!(err, Error::InvalidOp(_)), "{err:?}");
        store
            .declare_embedding_generation(nomiso_core::embedding::DeclareGenerationRequest {
                identity: test_identity(4, "model-b"),
                note: None,
            })
            .await
            .unwrap();
        // Wrong dimension.
        let err = store
            .stage_embeddings(
                2,
                vec![nomiso_core::embedding::StagedEmbedding {
                    memory: m1.clone(),
                    vector: vec![9.0; 7],
                }],
            )
            .await
            .unwrap_err();
        assert!(matches!(err, Error::DimensionMismatch { .. }), "{err:?}");
        // Missing memory.
        let err = store
            .stage_embeddings(
                2,
                vec![nomiso_core::embedding::StagedEmbedding {
                    memory: MemoryId::new("memory:00000000-0000-7000-8000-000000000000"),
                    vector: vec_of(4, 1.0),
                }],
            )
            .await
            .unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "{err:?}");
        // Second concurrent builder refused.
        let err = store
            .declare_embedding_generation(nomiso_core::embedding::DeclareGenerationRequest {
                identity: test_identity(4, "model-c"),
                note: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, Error::InvalidOp(_)), "{err:?}");
    }

    #[tokio::test]
    async fn egen_attest_rejects_declared_generation() {
        let mut cfg = StoreConfig::memory_test(8);
        cfg.embedding_identity = Some(test_identity(8, "model-a"));
        let store = SurrealMemoryStore::connect(cfg).await.unwrap();
        store.migrate().await.unwrap();
        let err = store
            .attest_embedding_identity(test_identity(8, "model-b"))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::InvalidOp(_)), "{err:?}");
    }

    // ---- Durable job journal (JOB-001..007) ----

    fn job_req(scope: &str, kind: &str) -> nomiso_core::job::EnqueueJobRequest {
        nomiso_core::job::EnqueueJobRequest {
            scope: scope.into(),
            kind: kind.into(),
            inputs: vec![],
            composition: Some(json!({"producer": "test", "policy": "p1"})),
            payload: Some(json!({"goal": "summarize"})),
            budget: nomiso_core::job::JobBudget {
                max_attempts: 3,
                lease_ms: 5_000,
                retry_backoff_ms: Some(5),
                deadline_ms: None,
            },
            dedup_hint: None,
        }
    }

    fn claim_req(worker: &str, scopes: &[&str], kinds: &[&str]) -> ClaimJobRequest {
        ClaimJobRequest {
            worker: worker.into(),
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            kinds: kinds.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[tokio::test]
    async fn job_enqueue_is_durable_and_deduplicates() {
        let store = setup(8).await;
        let r1 = store
            .enqueue_job(job_req("org/acme", "summarize"))
            .await
            .unwrap();
        assert!(!r1.deduplicated);
        assert_eq!(r1.job.state, JobState::Pending);
        assert_eq!(r1.job.fence, 0);
        assert!(r1.job.id.starts_with("nomiso_job:"));

        // Identical intent replays the live job (JOB-001/002).
        let r2 = store
            .enqueue_job(job_req("org/acme", "summarize"))
            .await
            .unwrap();
        assert!(r2.deduplicated);
        assert_eq!(r2.job.id, r1.job.id);

        // Different kind or hint is a different intent.
        let mut other = job_req("org/acme", "summarize");
        other.dedup_hint = Some("different".into());
        let r3 = store.enqueue_job(other).await.unwrap();
        assert!(!r3.deduplicated);
        assert_ne!(r3.job.id, r1.job.id);

        // get/list round-trip; summaries omit payload bodies (JOB-007).
        let got = store.get_job("org/acme", &r1.job.id).await.unwrap();
        assert_eq!(got.payload, r1.job.payload);
        let list = store
            .list_jobs(ListJobsRequest {
                scope: "org/acme".into(),
                state: None,
                kind: None,
                limit: None,
            })
            .await
            .unwrap();
        assert_eq!(list.len(), 2);
        let s = list.iter().find(|s| s.id == r1.job.id).unwrap();
        assert_eq!(s.state, JobState::Pending);
        // JobSummary has no payload field at all — by construction.
        let ser = serde_json::to_value(s).unwrap();
        assert!(ser.get("payload").is_none());
        assert!(ser.get("checkpoint").is_none());

        // Scope isolation.
        assert!(matches!(
            store.get_job("org/other", &r1.job.id).await,
            Err(Error::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn job_enqueue_rejects_bad_input_and_validates() {
        let store = setup(8).await;
        // Missing input → refused.
        let mut req = job_req("org/acme", "summarize");
        req.inputs = vec![nomiso_core::job::JobInput {
            kind: nomiso_core::relationship::EndpointKind::Memory,
            id: "00000000-0000-7000-8000-000000000000".into(),
            revision: None,
        }];
        assert!(matches!(
            store.enqueue_job(req).await,
            Err(Error::InvalidOp(_))
        ));
        // Empty kind / zero budget → refused.
        let mut bad = job_req("org/acme", "");
        bad.budget.max_attempts = 1;
        assert!(store.enqueue_job(bad).await.is_err());
        let mut bad2 = job_req("org/acme", "k");
        bad2.budget.max_attempts = 0;
        assert!(store.enqueue_job(bad2).await.is_err());
        // Oversized payload → refused.
        let mut big = job_req("org/acme", "k");
        big.payload = Some(json!({"blob": "x".repeat(20 * 1024)}));
        assert!(matches!(
            store.enqueue_job(big).await,
            Err(Error::PayloadTooLarge(_))
        ));
    }

    #[tokio::test]
    async fn job_claim_fences_and_single_winner() {
        let store = setup(8).await;
        let r = store
            .enqueue_job(job_req("org/acme", "summarize"))
            .await
            .unwrap();
        let lease = store
            .claim_job(claim_req("w1", &["org/acme"], &["summarize"]))
            .await
            .unwrap()
            .expect("claimable");
        assert_eq!(lease.job.state, JobState::Leased);
        assert_eq!(lease.fence, 1);
        assert_eq!(lease.job.attempts, 1);
        assert_eq!(lease.job.owner.as_deref(), Some("w1"));

        // A second claimant finds nothing while the lease is held.
        let none = store
            .claim_job(claim_req("w2", &["org/acme"], &["summarize"]))
            .await
            .unwrap();
        assert!(none.is_none());

        // Wrong fence / wrong owner cannot checkpoint or complete (JOB-003).
        assert!(matches!(
            store
                .checkpoint_job(&lease.job.id, 99, "w1", json!({"done": 1}))
                .await,
            Err(Error::LeaseLost(_))
        ));
        assert!(matches!(
            store
                .checkpoint_job(&lease.job.id, lease.fence, "w2", json!({"done": 1}))
                .await,
            Err(Error::LeaseLost(_))
        ));
        assert!(matches!(
            store
                .complete_job(&lease.job.id, 99, "w1", json!({"ok": true}))
                .await,
            Err(Error::LeaseLost(_))
        ));

        // Valid fence checkpoints and completes.
        let cp = store
            .checkpoint_job(&lease.job.id, lease.fence, "w1", json!({"cursor": 5}))
            .await
            .unwrap();
        assert!(cp.checkpoint.is_some());
        let done = store
            .complete_job(&lease.job.id, lease.fence, "w1", json!({"written": 3}))
            .await
            .unwrap();
        assert_eq!(done.state, JobState::Succeeded);
        assert!(done.result.is_some());
        assert!(done.completed_at.is_some());
        // Terminal job frees the dedup slot: identical intent is new work.
        let r2 = store
            .enqueue_job(job_req("org/acme", "summarize"))
            .await
            .unwrap();
        assert!(!r2.deduplicated);
        assert_ne!(r2.job.id, r.job.id);
    }

    #[tokio::test]
    async fn job_claim_concurrent_single_winner() {
        let store = setup(8).await;
        store
            .enqueue_job(job_req("org/acme", "summarize"))
            .await
            .unwrap();
        let a = store.clone();
        let b = store.clone();
        let (ra, rb) = tokio::join!(
            a.claim_job(claim_req("wa", &["org/acme"], &["summarize"])),
            b.claim_job(claim_req("wb", &["org/acme"], &["summarize"])),
        );
        let winners = [ra, rb]
            .iter()
            .filter(|r| r.as_ref().unwrap().is_some())
            .count();
        assert_eq!(winners, 1);
    }

    #[tokio::test]
    async fn job_lease_expiry_reacquires_with_new_fence() {
        let store = setup(8).await;
        let mut req = job_req("org/acme", "summarize");
        req.budget.lease_ms = 30;
        let r = store.enqueue_job(req).await.unwrap();
        let lease1 = store
            .claim_job(claim_req("w1", &["org/acme"], &["summarize"]))
            .await
            .unwrap()
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        // Expired lease → reacquirable by another worker with a new fence.
        let lease2 = store
            .claim_job(claim_req("w2", &["org/acme"], &["summarize"]))
            .await
            .unwrap()
            .expect("expired lease reacquired");
        assert!(lease2.fence > lease1.fence);
        assert_eq!(lease2.job.attempts, 2);
        // The stale holder cannot commit anything now (JOB-003).
        assert!(matches!(
            store
                .complete_job(&r.job.id, lease1.fence, "w1", json!({"late": true}))
                .await,
            Err(Error::LeaseLost(_))
        ));
        store
            .complete_job(&r.job.id, lease2.fence, "w2", json!({"ok": true}))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn job_renew_checkpoint_fail_retry_and_budget() {
        let store = setup(8).await;
        let mut req = job_req("org/acme", "summarize");
        req.budget.max_attempts = 2;
        req.budget.retry_backoff_ms = Some(1);
        let r = store.enqueue_job(req).await.unwrap();
        let lease = store
            .claim_job(claim_req("w1", &["org/acme"], &["summarize"]))
            .await
            .unwrap()
            .unwrap();

        // Renew under the fence extends the lease.
        let renewed = store
            .renew_job_lease(&r.job.id, lease.fence, "w1")
            .await
            .unwrap();
        assert!(renewed.lease_until >= lease.lease_until);

        // Retryable failure returns to pending with attempt history.
        let failed_once = store
            .fail_job(
                &r.job.id,
                lease.fence,
                "w1",
                JobError {
                    code: "provider_timeout".into(),
                    message: "provider timed out".into(),
                    retryable: true,
                },
            )
            .await
            .unwrap();
        assert_eq!(failed_once.state, JobState::Pending);
        assert!(failed_once.not_before.is_some());
        assert_eq!(failed_once.attempts, 1);
        assert_eq!(failed_once.history.len(), 1);
        assert!(failed_once.history[0].ended_at.is_some());

        // Exhausting the attempt budget is terminal (JOB-005).
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let lease2 = store
            .claim_job(claim_req("w1", &["org/acme"], &["summarize"]))
            .await
            .unwrap()
            .unwrap();
        let dead = store
            .fail_job(
                &r.job.id,
                lease2.fence,
                "w1",
                JobError {
                    code: "provider_timeout".into(),
                    message: "again".into(),
                    retryable: true,
                },
            )
            .await
            .unwrap();
        assert_eq!(dead.state, JobState::Failed);
        assert_eq!(
            dead.terminal_reason.as_deref(),
            Some("attempt budget exhausted")
        );
        // Terminal jobs are never re-claimed.
        let none = store
            .claim_job(claim_req("w1", &["org/acme"], &["summarize"]))
            .await
            .unwrap();
        assert!(none.is_none());
    }

    #[tokio::test]
    async fn job_non_retryable_failure_is_terminal() {
        let store = setup(8).await;
        let r = store
            .enqueue_job(job_req("org/acme", "summarize"))
            .await
            .unwrap();
        let lease = store
            .claim_job(claim_req("w1", &["org/acme"], &["summarize"]))
            .await
            .unwrap()
            .unwrap();
        let dead = store
            .fail_job(
                &r.job.id,
                lease.fence,
                "w1",
                JobError {
                    code: "invalid_input".into(),
                    message: "payload unparseable".into(),
                    retryable: false,
                },
            )
            .await
            .unwrap();
        assert_eq!(dead.state, JobState::Failed);
        assert_eq!(
            dead.terminal_reason.as_deref(),
            Some("non-retryable failure")
        );
    }

    #[tokio::test]
    async fn job_cancel_and_supersede_transitions() {
        let store = setup(8).await;
        // Pending cancel.
        let r1 = store
            .enqueue_job(job_req("org/acme", "summarize"))
            .await
            .unwrap();
        let c1 = store
            .cancel_job("org/acme", &r1.job.id, "operator stop")
            .await
            .unwrap();
        assert_eq!(c1.state, JobState::Cancelled);
        assert_eq!(c1.terminal_reason.as_deref(), Some("operator stop"));

        // Leased cancel keeps committed checkpoint visible (JOB-006).
        let mut req2 = job_req("org/acme", "summarize");
        req2.dedup_hint = Some("two".into());
        let r2 = store.enqueue_job(req2).await.unwrap();
        let lease = store
            .claim_job(claim_req("w1", &["org/acme"], &["summarize"]))
            .await
            .unwrap()
            .unwrap();
        store
            .checkpoint_job(
                &r2.job.id,
                lease.fence,
                "w1",
                json!({"committed_ops": ["k1"]}),
            )
            .await
            .unwrap();
        let c2 = store
            .cancel_job("org/acme", &r2.job.id, "inputs changed")
            .await
            .unwrap();
        assert_eq!(c2.state, JobState::Cancelled);
        assert!(c2.checkpoint.is_some(), "committed checkpoint survives");
        // Cancelled lease holder cannot finish (JOB-003).
        assert!(matches!(
            store
                .complete_job(&r2.job.id, lease.fence, "w1", json!({}))
                .await,
            Err(Error::LeaseLost(_))
        ));
        // Terminal jobs cannot be cancelled again.
        assert!(store
            .cancel_job("org/acme", &r2.job.id, "again")
            .await
            .is_err());

        // Supersede records the replacement.
        let mut req3 = job_req("org/acme", "summarize");
        req3.dedup_hint = Some("three".into());
        let r3 = store.enqueue_job(req3).await.unwrap();
        let mut req4 = job_req("org/acme", "summarize");
        req4.dedup_hint = Some("four".into());
        let r4 = store.enqueue_job(req4).await.unwrap();
        let sup = store
            .supersede_job("org/acme", &r3.job.id, &r4.job.id, "newer inputs")
            .await
            .unwrap();
        assert_eq!(sup.state, JobState::Superseded);
        assert_eq!(sup.replaced_by.as_deref(), Some(r4.job.id.as_str()));
        // Superseded jobs are never claimed.
        let none = store
            .claim_job(claim_req("w1", &["org/acme"], &["summarize"]))
            .await
            .unwrap();
        // r4 remains claimable (it is pending).
        assert!(none.is_some());
        assert_eq!(none.unwrap().job.id, r4.job.id);
    }

    #[tokio::test]
    async fn job_claim_respects_scope_and_kind_grant() {
        let store = setup(8).await;
        store
            .enqueue_job(job_req("org/acme", "summarize"))
            .await
            .unwrap();
        // Wrong scope grant.
        assert!(store
            .claim_job(claim_req("w1", &["org/other"], &["summarize"]))
            .await
            .unwrap()
            .is_none());
        // Wrong kind allowlist.
        assert!(store
            .claim_job(claim_req("w1", &["org/acme"], &["reindex"]))
            .await
            .unwrap()
            .is_none());
        // Right grant claims.
        assert!(store
            .claim_job(claim_req("w1", &["org/acme"], &["summarize", "reindex"]))
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn job_complete_rejects_stale_and_purged_inputs() {
        let store = setup(8).await;
        let e = store.put_entity(ent_req("org/acme", "E1")).await.unwrap();
        let m = store
            .put(put_req("org/acme", "memory under summary"))
            .await
            .unwrap();

        // Pin the entity at revision 1, then move it forward → StaleInput.
        let mut req = job_req("org/acme", "summarize");
        req.inputs = vec![
            JobInput {
                kind: nomiso_core::relationship::EndpointKind::Entity,
                id: e.id.clone(),
                revision: Some(1),
            },
            JobInput {
                kind: nomiso_core::relationship::EndpointKind::Memory,
                id: m.id.as_str().to_string(),
                revision: Some(1),
            },
        ];
        let r = store.enqueue_job(req).await.unwrap();
        let lease = store
            .claim_job(claim_req("w1", &["org/acme"], &["summarize"]))
            .await
            .unwrap()
            .unwrap();
        store
            .update_entity(UpdateEntityRequest {
                id: e.id.clone(),
                scope: "org/acme".into(),
                expected_version: 1,
                name: Some("E1-renamed".into()),
                aliases: None,
                attrs: None,
            })
            .await
            .unwrap();
        assert!(matches!(
            store
                .complete_job(&r.job.id, lease.fence, "w1", json!({"ok": true}))
                .await,
            Err(Error::StaleInput(_))
        ));
        // The job stays leased — the worker decides how to replan.
        let still = store.get_job("org/acme", &r.job.id).await.unwrap();
        assert_eq!(still.state, JobState::Leased);

        // A purged input is likewise rejected (JOB-004).
        store
            .forget(ForgetRequest {
                id: m.id.clone(),
                scope: "org/acme".into(),
                expected_version: Some(1),
                hard: true,
                at: None,
            })
            .await
            .unwrap();
        assert!(matches!(
            store
                .complete_job(&r.job.id, lease.fence, "w1", json!({"ok": true}))
                .await,
            Err(Error::StaleInput(_))
        ));
        store
            .fail_job(
                &r.job.id,
                lease.fence,
                "w1",
                JobError {
                    code: "stale".into(),
                    message: "inputs moved".into(),
                    retryable: false,
                },
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn job_checkpoint_and_result_bounded() {
        let store = setup(8).await;
        let r = store
            .enqueue_job(job_req("org/acme", "summarize"))
            .await
            .unwrap();
        let lease = store
            .claim_job(claim_req("w1", &["org/acme"], &["summarize"]))
            .await
            .unwrap()
            .unwrap();
        let big = json!({"blob": "x".repeat(20 * 1024)});
        assert!(matches!(
            store
                .checkpoint_job(&r.job.id, lease.fence, "w1", big.clone())
                .await,
            Err(Error::PayloadTooLarge(_))
        ));
        assert!(matches!(
            store.complete_job(&r.job.id, lease.fence, "w1", big).await,
            Err(Error::PayloadTooLarge(_))
        ));
        // Non-object bodies refused.
        assert!(store
            .checkpoint_job(&r.job.id, lease.fence, "w1", json!([1, 2, 3]))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn job_deadline_expired_not_reclaimed() {
        let store = setup(8).await;
        let mut req = job_req("org/acme", "summarize");
        req.budget.deadline_ms = Some(20);
        let r = store.enqueue_job(req).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        // Claim scan terminal-fails the expired job rather than leasing it.
        assert!(store
            .claim_job(claim_req("w1", &["org/acme"], &["summarize"]))
            .await
            .unwrap()
            .is_none());
        let j = store.get_job("org/acme", &r.job.id).await.unwrap();
        assert_eq!(j.state, JobState::Failed);
        assert_eq!(j.terminal_reason.as_deref(), Some("deadline exceeded"));
    }

    #[tokio::test]
    async fn job_list_filters_state_and_kind() {
        let store = setup(8).await;
        store
            .enqueue_job(job_req("org/acme", "summarize"))
            .await
            .unwrap();
        let mut req2 = job_req("org/acme", "reindex");
        req2.dedup_hint = Some("x".into());
        let r2 = store.enqueue_job(req2).await.unwrap();
        store
            .cancel_job("org/acme", &r2.job.id, "stop")
            .await
            .unwrap();

        let pending = store
            .list_jobs(ListJobsRequest {
                scope: "org/acme".into(),
                state: Some(JobState::Pending),
                kind: None,
                limit: None,
            })
            .await
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].kind, "summarize");

        let cancelled = store
            .list_jobs(ListJobsRequest {
                scope: "org/acme".into(),
                state: Some(JobState::Cancelled),
                kind: Some("reindex".into()),
                limit: None,
            })
            .await
            .unwrap();
        assert_eq!(cancelled.len(), 1);
        assert_eq!(cancelled[0].id, r2.job.id);
        assert!(!cancelled[0].has_checkpoint);
    }

    fn intent(kind: &str, self_input: bool) -> JobIntent {
        JobIntent {
            kind: kind.into(),
            inputs: vec![],
            self_input,
            composition: Some(json!({"producer": "test", "policy": "p1"})),
            payload: Some(json!({"goal": "summarize"})),
            budget: nomiso_core::job::JobBudget {
                max_attempts: 3,
                lease_ms: 5_000,
                retry_backoff_ms: Some(5),
                deadline_ms: None,
            },
            dedup_hint: None,
        }
    }

    fn list_req(scope: &str) -> nomiso_core::list::ListRequest {
        nomiso_core::list::ListRequest {
            scope: scope.into(),
            scope_match: ScopeMatch::Exact,
            categories: None,
            as_of: None,
            known_as_of: None,
            sys_as_of: None,
            text: None,
            limit: Some(50),
            cursor: None,
        }
    }

    #[tokio::test]
    async fn put_with_jobs_commits_write_and_intents_atomically() {
        let store = setup(8).await;
        let res = store
            .put_with_jobs(
                put_req("org/acme", "durable write with jobs"),
                None,
                vec![intent("summarize", true), intent("extract", false)],
            )
            .await
            .expect("put_with_jobs");
        assert_eq!(res.write.version, 1);
        assert!(!res.write.replayed);
        assert_eq!(res.jobs.len(), 2);
        for j in &res.jobs {
            assert!(!j.deduplicated);
            assert_eq!(j.job.state, JobState::Pending);
        }
        // self_input pins the created record at revision 1.
        let self_job = res.jobs.iter().find(|j| j.job.kind == "summarize").unwrap();
        assert_eq!(self_job.job.inputs.len(), 1);
        assert_eq!(self_job.job.inputs[0].id, res.write.id.bare_key());
        assert_eq!(self_job.job.inputs[0].revision, Some(1));
        let plain = res.jobs.iter().find(|j| j.job.kind == "extract").unwrap();
        assert!(plain.job.inputs.is_empty());
        // Jobs are claimable by a worker in scope.
        let lease = store
            .claim_job(claim_req("w1", &["org/acme"], &["summarize"]))
            .await
            .unwrap()
            .expect("claim");
        assert_eq!(lease.job.id, self_job.job.id);
    }

    #[tokio::test]
    async fn put_with_jobs_bad_intent_aborts_the_write() {
        let store = setup(8).await;
        let mut bad = intent("summarize", false);
        bad.inputs.push(JobInput {
            kind: nomiso_core::relationship::EndpointKind::Memory,
            id: "does-not-exist".into(),
            revision: None,
        });
        let err = store
            .put_with_jobs(put_req("org/acme", "must not commit"), None, vec![bad])
            .await
            .expect_err("bad input must fail");
        assert_eq!(err.code(), "invalid_request", "{err}");
        // Atomicity: the canonical write rolled back with the intent.
        let rows = store.list(list_req("org/acme")).await.unwrap();
        assert!(rows.items.is_empty(), "write must not commit");
        let jobs = store
            .list_jobs(ListJobsRequest {
                scope: "org/acme".into(),
                state: None,
                kind: None,
                limit: None,
            })
            .await
            .unwrap();
        assert!(jobs.is_empty());
    }

    #[tokio::test]
    async fn put_with_jobs_dedups_against_live_identical_intent() {
        let store = setup(8).await;
        let mem = store
            .put(put_req("org/acme", "existing evidence"))
            .await
            .unwrap();
        // An identical live intent already covers the derived work.
        let mut req = job_req("org/acme", "summarize");
        req.inputs.push(JobInput {
            kind: nomiso_core::relationship::EndpointKind::Memory,
            id: mem.id.bare_key().into(),
            revision: Some(1),
        });
        let live = store.enqueue_job(req).await.unwrap();
        assert!(!live.deduplicated);

        let mut it = intent("summarize", false);
        it.inputs.push(JobInput {
            kind: nomiso_core::relationship::EndpointKind::Memory,
            id: mem.id.bare_key().into(),
            revision: Some(1),
        });
        let res = store
            .put_with_jobs(put_req("org/acme", "new write"), None, vec![it])
            .await
            .unwrap();
        assert_eq!(res.jobs.len(), 1);
        assert!(res.jobs[0].deduplicated);
        assert_eq!(res.jobs[0].job.id, live.job.id);
        // The write still commits — dedup is not a failure.
        assert_eq!(res.write.version, 1);
        let jobs = store
            .list_jobs(ListJobsRequest {
                scope: "org/acme".into(),
                state: None,
                kind: None,
                limit: None,
            })
            .await
            .unwrap();
        assert_eq!(jobs.len(), 1);
    }

    #[tokio::test]
    async fn put_with_jobs_keyed_replay_and_intent_conflict() {
        let store = setup(8).await;
        let mut req = put_req("org/acme", "keyed write");
        req.idempotency_key = Some("kw-1".into());
        let intents = vec![intent("summarize", true)];
        let first = store
            .put_with_jobs(req.clone(), None, intents.clone())
            .await
            .unwrap();
        assert!(!first.write.replayed);
        // Identical replay: write replays, intents dedup to committed jobs.
        let second = store
            .put_with_jobs(req.clone(), None, intents.clone())
            .await
            .unwrap();
        assert!(second.write.replayed);
        assert_eq!(second.write.id, first.write.id);
        assert_eq!(second.jobs.len(), 1);
        assert!(second.jobs[0].deduplicated);
        assert_eq!(second.jobs[0].job.id, first.jobs[0].job.id);
        // Same key, different declared intents → conflict, not silent loss.
        let err = store
            .put_with_jobs(req, None, vec![intent("different-kind", true)])
            .await
            .expect_err("divergent intents must conflict");
        assert_eq!(err.code(), "idempotency_conflict", "{err}");
        let jobs = store
            .list_jobs(ListJobsRequest {
                scope: "org/acme".into(),
                state: None,
                kind: None,
                limit: None,
            })
            .await
            .unwrap();
        assert_eq!(jobs.len(), 1, "no duplicate job on replay");
    }

    #[tokio::test]
    async fn supersede_with_jobs_commits_successor_and_intents() {
        let store = setup(8).await;
        let prior = store.put(put_req("org/acme", "old claim")).await.unwrap();
        let res = store
            .supersede_with_jobs(
                SupersedeRequest {
                    prior_id: prior.id.clone(),
                    expected_version: 1,
                    new: put_req("org/acme", "new claim"),
                    close_at: None,
                },
                vec![intent("rederive", true)],
            )
            .await
            .expect("supersede_with_jobs");
        assert_eq!(res.jobs.len(), 1);
        assert_eq!(res.jobs[0].job.inputs[0].id, res.write.id.bare_key());
        assert_eq!(res.jobs[0].job.inputs[0].revision, Some(1));
        // Prior closed in the same commit.
        let prior_row = store
            .read(ReadRequest {
                ids: vec![prior.id.clone()],
                scope: "org/acme".into(),
                scope_match: ScopeMatch::Exact,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
            })
            .await
            .unwrap();
        assert!(prior_row[0].superseded_by.is_some());
    }

    #[tokio::test]
    async fn supersede_with_jobs_bad_intent_leaves_prior_open() {
        let store = setup(8).await;
        let prior = store.put(put_req("org/acme", "old claim")).await.unwrap();
        let mut bad = intent("rederive", false);
        bad.inputs.push(JobInput {
            kind: nomiso_core::relationship::EndpointKind::Entity,
            id: "missing-entity".into(),
            revision: None,
        });
        let err = store
            .supersede_with_jobs(
                SupersedeRequest {
                    prior_id: prior.id.clone(),
                    expected_version: 1,
                    new: put_req("org/acme", "new claim"),
                    close_at: None,
                },
                vec![bad],
            )
            .await
            .expect_err("bad intent aborts supersede");
        assert_eq!(err.code(), "invalid_request", "{err}");
        // Prior stays open — the close did not commit.
        let prior_row = store
            .read(ReadRequest {
                ids: vec![prior.id.clone()],
                scope: "org/acme".into(),
                scope_match: ScopeMatch::Exact,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
            })
            .await
            .unwrap();
        assert!(prior_row[0].superseded_by.is_none());
        assert!(prior_row[0].valid_until.is_none());
    }

    fn gx(depth: u32) -> nomiso_core::ops::GraphExpand {
        nomiso_core::ops::GraphExpand {
            predicates: None,
            direction: None,
            max_depth: Some(depth),
            max_seeds: None,
            max_candidates: None,
        }
    }

    fn search_q(
        scope: &str,
        text: &str,
        expand: Option<nomiso_core::ops::GraphExpand>,
    ) -> SearchQuery {
        SearchQuery {
            query: text.into(),
            scope: scope.into(),
            scope_match: ScopeMatch::Exact,
            as_of: None,
            known_as_of: None,
            sys_as_of: None,
            categories: None,
            limit: Some(10),
            embedding: None,
            graph_enrich: Some(false),
            graph_expand: expand,
        }
    }

    #[tokio::test]
    async fn expand_surfaces_unmatched_neighbor() {
        let store = setup(8).await;
        let a = store
            .put(put_req("org/gx", "unique-alpha-token fact"))
            .await
            .unwrap();
        let b = store
            .put(put_req("org/gx", "completely different wording zebra"))
            .await
            .unwrap();
        store
            .put_relationship(rel_req(
                "org/gx",
                RelationPredicate::DependsOn,
                EndpointRef::new(EK::Memory, a.id.to_string()),
                EndpointRef::new(EK::Memory, b.id.to_string()),
            ))
            .await
            .unwrap();

        // Direct search cannot reach B.
        let direct = store
            .search(search_q("org/gx", "unique-alpha-token", None))
            .await
            .unwrap();
        assert_eq!(direct.len(), 1);
        assert_eq!(direct[0].id, a.id);

        // Expansion reaches B via the edge.
        let out = store
            .search_detailed(search_q("org/gx", "unique-alpha-token", Some(gx(1))))
            .await
            .unwrap();
        assert_eq!(out.hits.len(), 2);
        assert_eq!(out.hits[0].id, a.id);
        let bx = &out.hits[1];
        assert_eq!(bx.id, b.id);
        assert!(bx.signals.expanded);
        assert_eq!(bx.score_kind, ScoreKind::RankFallback);
        assert!(bx.score < out.hits[0].score);
        let info = bx.expansion.as_ref().unwrap();
        assert_eq!(info.depth, 1);
        assert_eq!(info.from, vec![a.id.clone()]);
        assert_eq!(info.via_predicates, vec!["depends_on".to_string()]);
        let st = out.stats.expansion.unwrap();
        assert_eq!(st.seeds, 1);
        assert_eq!(st.candidates_found, 1);
        assert_eq!(st.candidates_added, 1);
        // `depth` may be marked: at the bound, followable edges remained.
        assert!(!st.truncated.contains(&"candidates".to_string()));
    }

    #[tokio::test]
    async fn expand_respects_scope_and_edge_validity() {
        let store = setup(8).await;
        let a = store
            .put(put_req("org/gx2", "unique-bravo-token fact"))
            .await
            .unwrap();
        let foreign = store
            .put(put_req("org/other", "foreign scope memory"))
            .await
            .unwrap();
        // Foreign-scope edge never participates in org/gx2 expansion.
        let fx = store
            .put(put_req("org/other", "unique-bravo-token shadow"))
            .await
            .unwrap();
        store
            .put_relationship(rel_req(
                "org/other",
                RelationPredicate::DependsOn,
                EndpointRef::new(EK::Memory, fx.id.to_string()),
                EndpointRef::new(EK::Memory, foreign.id.to_string()),
            ))
            .await
            .unwrap();

        // A closed edge (valid_until in the past) is not followed.
        let past_from = Timestamp::UNIX_EPOCH + std::time::Duration::from_secs(1_000);
        let past_until = Timestamp::UNIX_EPOCH + std::time::Duration::from_secs(2_000);
        let c = store
            .put(put_req("org/gx2", "expired-link target wording"))
            .await
            .unwrap();
        let mut closed_edge = rel_req(
            "org/gx2",
            RelationPredicate::DependsOn,
            EndpointRef::new(EK::Memory, a.id.to_string()),
            EndpointRef::new(EK::Memory, c.id.to_string()),
        );
        closed_edge.valid_from = Some(past_from);
        closed_edge.valid_until = Some(past_until);
        store.put_relationship(closed_edge).await.unwrap();

        let out = store
            .search_detailed(search_q("org/gx2", "unique-bravo-token", Some(gx(2))))
            .await
            .unwrap();
        // The closed edge is never followed: if `c` appears it must be a
        // direct hit with no expansion provenance, and no foreign memory may
        // appear at all.
        for h in &out.hits {
            assert_ne!(h.id, foreign.id);
            assert!(h.id != c.id || h.expansion.is_none());
        }
        assert_eq!(out.stats.expansion.unwrap().candidates_found, 0);
    }

    #[tokio::test]
    async fn expand_predicate_filter_and_depth_bound() {
        let store = setup(8).await;
        let a = store
            .put(put_req("org/gx3", "unique-charlie-token"))
            .await
            .unwrap();
        let b = store
            .put(put_req("org/gx3", "hop one target"))
            .await
            .unwrap();
        let c = store
            .put(put_req("org/gx3", "hop two target"))
            .await
            .unwrap();
        let d = store
            .put(put_req("org/gx3", "contradicts-only target"))
            .await
            .unwrap();
        for (s, o, p) in [
            (&a, &b, RelationPredicate::DerivedFrom),
            (&b, &c, RelationPredicate::DerivedFrom),
            (&a, &d, RelationPredicate::Contradicts),
        ] {
            store
                .put_relationship(rel_req(
                    "org/gx3",
                    p,
                    EndpointRef::new(EK::Memory, s.id.to_string()),
                    EndpointRef::new(EK::Memory, o.id.to_string()),
                ))
                .await
                .unwrap();
        }

        // Predicate filter: only the Contradicts edge expands.
        let mut only_contra = gx(2);
        only_contra.predicates = Some(vec![RelationPredicate::Contradicts]);
        let out = store
            .search_detailed(search_q(
                "org/gx3",
                "unique-charlie-token",
                Some(only_contra),
            ))
            .await
            .unwrap();
        let ids: Vec<_> = out.hits.iter().map(|h| h.id.clone()).collect();
        assert!(ids.contains(&d.id));
        assert!(!ids.contains(&b.id));
        assert!(!ids.contains(&c.id));

        // Depth bound: hop-two target unreachable at depth 1; truncation is
        // reported because B's outgoing edge was refused.
        let out = store
            .search_detailed(search_q("org/gx3", "unique-charlie-token", Some(gx(1))))
            .await
            .unwrap();
        let ids: Vec<_> = out.hits.iter().map(|h| h.id.clone()).collect();
        assert!(ids.contains(&b.id));
        assert!(ids.contains(&d.id));
        assert!(!ids.contains(&c.id));
        assert!(out
            .stats
            .expansion
            .unwrap()
            .truncated
            .contains(&"depth".to_string()));
    }

    #[tokio::test]
    async fn expand_merges_direct_hit_provenance() {
        let store = setup(8).await;
        let a = store
            .put(put_req("org/gx4", "shared delta-token alpha"))
            .await
            .unwrap();
        let b = store
            .put(put_req("org/gx4", "shared delta-token beta"))
            .await
            .unwrap();
        store
            .put_relationship(rel_req(
                "org/gx4",
                RelationPredicate::Supports,
                EndpointRef::new(EK::Memory, a.id.to_string()),
                EndpointRef::new(EK::Memory, b.id.to_string()),
            ))
            .await
            .unwrap();
        let out = store
            .search_detailed(search_q("org/gx4", "delta-token", Some(gx(1))))
            .await
            .unwrap();
        assert_eq!(out.hits.len(), 2);
        // Both hits are direct; expansion records the corroborating path on
        // whichever hit the other's edge reached.
        let (hits_with_prov, expanded_only) =
            out.hits.iter().fold((0usize, 0usize), |(p, e), h| {
                (
                    p + h.expansion.is_some() as usize,
                    e + h.signals.expanded as usize,
                )
            });
        assert!(
            hits_with_prov >= 1,
            "direct hit keeps rank + discovery path"
        );
        assert_eq!(expanded_only, 0, "no expansion-only candidates needed");
    }

    #[tokio::test]
    async fn expand_candidate_cap_marks_truncation() {
        let store = setup(8).await;
        let a = store
            .put(put_req("org/gx5", "unique-echo-token"))
            .await
            .unwrap();
        for i in 0..3 {
            let n = store
                .put(put_req("org/gx5", &format!("neighbor {i} distinct words")))
                .await
                .unwrap();
            store
                .put_relationship(rel_req(
                    "org/gx5",
                    RelationPredicate::DerivedFrom,
                    EndpointRef::new(EK::Memory, a.id.to_string()),
                    EndpointRef::new(EK::Memory, n.id.to_string()),
                ))
                .await
                .unwrap();
        }
        let mut capped = gx(1);
        capped.max_candidates = Some(1);
        let out = store
            .search_detailed(search_q("org/gx5", "unique-echo-token", Some(capped)))
            .await
            .unwrap();
        assert_eq!(out.hits.len(), 2);
        let st = out.stats.expansion.unwrap();
        assert_eq!(st.candidates_added, 1);
        assert!(st.truncated.contains(&"candidates".to_string()));
    }

    #[tokio::test]
    async fn expand_prefix_scope_follows_descendant_edges() {
        let store = setup(8).await;
        let a = store
            .put(put_req("org/p/sub", "unique-foxtrot-token"))
            .await
            .unwrap();
        let b = store
            .put(put_req("org/p/sub", "descendant neighbor"))
            .await
            .unwrap();
        store
            .put_relationship(rel_req(
                "org/p/sub",
                RelationPredicate::DerivedFrom,
                EndpointRef::new(EK::Memory, a.id.to_string()),
                EndpointRef::new(EK::Memory, b.id.to_string()),
            ))
            .await
            .unwrap();
        let mut q = search_q("org/p", "unique-foxtrot-token", Some(gx(1)));
        q.scope_match = ScopeMatch::Prefix;
        let out = store.search_detailed(q).await.unwrap();
        assert_eq!(out.hits.len(), 2);
        assert!(out.hits[1].signals.expanded);
    }
}
