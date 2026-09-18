//! Shared Nomiso store connect for CLI / serve / MCP.

use std::sync::Arc;

use anyhow::{Context, Result};
use nomiso_embed::HashingEmbedder;
use nomiso_store::StoreConfig;
use vegapunk::{Profile, RuleWriter, Vegapunk};

use crate::config::VegapunkFileConfig;

/// True when endpoint is process-local ephemeral memory.
pub fn is_ephemeral(endpoint: &str) -> bool {
    matches!(endpoint, "memory" | "mem://" | "memory://" | "mem" | "")
}

/// Fail early with an actionable message (unknown engine / missing feature).
pub fn check_endpoint(endpoint: &str) -> Result<()> {
    if endpoint.starts_with("rocksdb://") {
        #[cfg(not(feature = "embedded-rocks"))]
        {
            anyhow::bail!(
                "endpoint {endpoint} needs Surreal's Rocks engine — rebuild vegapunk-cli \
                 with --features embedded-rocks (on by default)"
            );
        }
    }
    let _ = endpoint;
    Ok(())
}

/// Build store config from endpoint + dim (memory / rocksdb:// / ws:// / …).
pub fn store_config(endpoint: &str, embed_dim: usize) -> StoreConfig {
    if is_ephemeral(endpoint) {
        StoreConfig::memory_test(embed_dim)
    } else if endpoint.starts_with("rocksdb://") {
        let path = endpoint.strip_prefix("rocksdb://").unwrap_or(endpoint);
        StoreConfig::rocksdb_path(path, embed_dim)
    } else {
        StoreConfig {
            endpoint: endpoint.to_string(),
            embedding_dim: embed_dim,
            limits: nomiso::Limits {
                embedding_dim: embed_dim,
                ..nomiso::Limits::default()
            },
            ..StoreConfig::default()
        }
    }
}

/// How to attach an embedder (HTTP wins over hashing).
#[derive(Debug, Clone, Default)]
pub struct EmbedPlan {
    pub hash: bool,
    pub url: Option<String>,
    pub model: Option<String>,
    pub api_key_env: Option<String>,
    pub dim: usize,
}

impl EmbedPlan {
    pub fn mode_label(&self) -> &'static str {
        if self.url.as_deref().is_some_and(|u| !u.trim().is_empty()) {
            "http"
        } else if self.hash {
            "hashing"
        } else {
            "none"
        }
    }
}

/// Attach HTTP embedder when URL is set; else hashing when `hash`; else none.
pub fn apply_embedder(vp: Vegapunk, plan: &EmbedPlan) -> Result<Vegapunk> {
    if let Some(url) = plan.url.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        #[cfg(feature = "embed-http")]
        {
            use nomiso_embed::{HttpEmbedder, HttpEmbedderConfig};
            let model = plan
                .model
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("text-embedding-3-small")
                .to_string();
            let env_name = plan
                .api_key_env
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("VEGAPUNK_EMBED_API_KEY");
            let emb = HttpEmbedder::new(HttpEmbedderConfig {
                base_url: url.to_string(),
                api_key: String::new(),
                model,
                dimension: plan.dim,
            })
            .map_err(|e| anyhow::anyhow!("http embedder: {e}"))?
            .with_api_key_env(env_name);
            return Ok(vp.with_embedder(Arc::new(emb)));
        }
        #[cfg(not(feature = "embed-http"))]
        {
            let _ = url;
            anyhow::bail!("HTTP embedder requires vegapunk-cli feature embed-http (on by default)");
        }
    }
    if plan.hash {
        let emb = HashingEmbedder::new(plan.dim).context("hash embedder")?;
        return Ok(vp.with_embedder(Arc::new(emb)));
    }
    Ok(vp)
}

/// Connect Vegapunk with profile, rule writer, optional embedder.
pub async fn connect_vegapunk(
    endpoint: &str,
    embed_dim: usize,
    profile: Profile,
    plan: EmbedPlan,
) -> Result<Vegapunk> {
    check_endpoint(endpoint)?;
    let vp = Vegapunk::connect(store_config(endpoint, embed_dim))
        .await
        .context("connect vegapunk/nomiso")?
        .with_profile(profile)
        .with_writer(Arc::new(RuleWriter))
        .with_blob_store(Arc::new(vegapunk::BlobConfig::from_env().fs_store()));
    apply_embedder(vp, &plan)
}

/// Connect using resolved file config fields.
pub async fn connect_from_parts(
    endpoint: &str,
    embed_dim: usize,
    profile_name: &str,
    plan: EmbedPlan,
) -> Result<Vegapunk> {
    let profile = Profile::parse(profile_name)
        .with_context(|| format!("unknown profile '{profile_name}'"))?;
    connect_vegapunk(endpoint, embed_dim, profile, plan).await
}

/// Convenience from file config defaults.
#[allow(dead_code)]
pub async fn connect_from_file_cfg(cfg: &VegapunkFileConfig, hash_embed: bool) -> Result<Vegapunk> {
    connect_from_parts(
        &cfg.endpoint,
        cfg.embed_dim,
        &cfg.profile,
        EmbedPlan {
            hash: hash_embed,
            url: cfg.embed_url.clone(),
            model: cfg.embed_model.clone(),
            api_key_env: cfg.embed_api_key_env.clone(),
            dim: cfg.embed_dim,
        },
    )
    .await
}
