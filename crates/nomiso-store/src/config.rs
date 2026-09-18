//! Store configuration.

use nomiso_core::embedding::EmbeddingIdentity;
use nomiso_core::validate::Limits;

/// Search tuning.
#[derive(Debug, Clone)]
pub struct SearchConfig {
    /// Default result limit.
    pub default_limit: u32,
    /// Max result limit.
    pub max_search_limit: u32,
    /// RRF k constant (default 60).
    pub rrf_k: u32,
    /// Per-channel candidate limit before fusion.
    pub candidate_limit: u32,
    /// Graph 1-hop enrich by default.
    pub enable_graph_enrich: bool,
    /// HNSW query ef (used when operator supports it).
    pub hnsw_ef: u32,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            default_limit: 8,
            max_search_limit: 32,
            rrf_k: 60,
            candidate_limit: 32,
            enable_graph_enrich: true,
            hnsw_ef: 100,
        }
    }
}

/// Connection and policy configuration for the store.
///
/// `Debug` is manually implemented to redact credentials and endpoint
/// userinfo/query components.
#[derive(Clone)]
pub struct StoreConfig {
    /// Surreal endpoint: `memory`, `mem://`, `ws://host:port`, rocksdb path, etc.
    pub endpoint: String,
    /// Namespace.
    pub namespace: String,
    /// Database.
    pub database: String,
    /// Optional root username (remote).
    pub username: Option<String>,
    /// Optional root password (remote).
    pub password: Option<String>,
    /// Embedding dimension (HNSW + validation).
    pub embedding_dim: usize,
    /// Declared embedding model identity (MIG-004). On open, a declaration
    /// incompatible with the store's active generation fails closed; an
    /// unknown active generation requires explicit attestation or a new
    /// generation before a declared identity is accepted.
    pub embedding_identity: Option<EmbeddingIdentity>,
    /// Validation limits.
    pub limits: Limits,
    /// Search config.
    pub search: SearchConfig,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            endpoint: "memory".into(),
            namespace: "nomiso".into(),
            database: "memory".into(),
            username: None,
            password: None,
            embedding_dim: 1536,
            embedding_identity: None,
            limits: Limits::default(),
            search: SearchConfig::default(),
        }
    }
}

impl StoreConfig {
    /// In-memory store with a small embedding dim (tests).
    pub fn memory_test(dim: usize) -> Self {
        Self {
            endpoint: "memory".into(),
            embedding_dim: dim,
            limits: Limits {
                embedding_dim: dim,
                ..Limits::default()
            },
            ..Self::default()
        }
    }

    /// Embedded RocksDB path for durable local agents (requires `embedded-rocks` feature).
    ///
    /// Endpoint form: `rocksdb://{absolute_or_relative_path}`.
    pub fn rocksdb_path(path: impl AsRef<std::path::Path>, dim: usize) -> Self {
        let p = path.as_ref().display().to_string();
        Self {
            endpoint: format!("rocksdb://{p}"),
            embedding_dim: dim,
            limits: Limits {
                embedding_dim: dim,
                ..Limits::default()
            },
            ..Self::default()
        }
    }

    /// True when endpoint is process-local ephemeral memory.
    pub fn is_ephemeral(&self) -> bool {
        matches!(
            self.endpoint.as_str(),
            "memory" | "mem://" | "memory://" | "mem"
        )
    }

    /// Align limits.embedding_dim with store embedding_dim.
    pub fn normalized(mut self) -> Self {
        self.limits.embedding_dim = self.embedding_dim;
        self.limits.max_search_limit = self.search.max_search_limit;
        self
    }
}

/// Strip URL userinfo and query/fragment so embedded credentials never appear in logs.
fn redacted_endpoint(endpoint: &str) -> String {
    let mut ep = endpoint
        .split(['?', '#'])
        .next()
        .unwrap_or(endpoint)
        .to_string();
    if let Some(scheme_end) = ep.find("://") {
        let rest = &ep[scheme_end + 3..];
        if let Some(at) = rest.rfind('@') {
            ep = format!("{}://[REDACTED]@{}", &ep[..scheme_end], &rest[at + 1..]);
        }
    }
    ep
}

impl std::fmt::Debug for StoreConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreConfig")
            .field("endpoint", &redacted_endpoint(&self.endpoint))
            .field("namespace", &self.namespace)
            .field("database", &self.database)
            .field("username", &self.username.as_ref().map(|_| "[REDACTED]"))
            .field("password", &self.password.as_ref().map(|_| "[REDACTED]"))
            .field("embedding_dim", &self.embedding_dim)
            .field("embedding_identity", &self.embedding_identity)
            .field("limits", &self.limits)
            .field("search", &self.search)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_config_debug_redacts_credentials() {
        let cfg = StoreConfig {
            endpoint: "ws://admin:hunter2@db.internal:8000?token=sekret".into(),
            username: Some("root".into()),
            password: Some("hunter2".into()),
            ..StoreConfig::default()
        };
        let dbg = format!("{cfg:?}");
        assert!(dbg.contains("[REDACTED]"), "{dbg}");
        assert!(!dbg.contains("hunter2"), "{dbg}");
        assert!(!dbg.contains("sekret"), "{dbg}");
        assert!(!dbg.contains("admin"), "{dbg}");
        assert!(dbg.contains("db.internal"), "{dbg}");
    }

    #[test]
    fn store_config_debug_plain_endpoint_untouched() {
        let cfg = StoreConfig::memory_test(8);
        let dbg = format!("{cfg:?}");
        assert!(dbg.contains("memory"), "{dbg}");
        assert!(dbg.contains("username: None"), "{dbg}");
    }
}
