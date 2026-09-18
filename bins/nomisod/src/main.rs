//! nomisod — HTTP + MCP daemon for Nomiso.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use figment::providers::{Env, Format, Serialized, Toml};
use figment::Figment;
use nomiso::http_router;
use nomiso::{HttpAppState, NomisoClient, StoreConfig};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use tracing::{info, Level};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "nomisod", about = "Nomiso memory plane daemon (HTTP + MCP)")]
struct Cli {
    /// Config file path (TOML).
    #[arg(long, global = true, env = "NOMI_SO_CONFIG")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Run the daemon (HTTP and/or MCP stdio).
    Serve {
        /// Bind address for HTTP.
        #[arg(long, env = "NOMI_SO_BIND")]
        bind: Option<String>,
        /// Enable MCP on stdio (disables concurrent interactive HTTP logs mixing).
        #[arg(long, env = "NOMI_SO_MCP_STDIO")]
        mcp_stdio: bool,
        /// Disable HTTP even if bind is set.
        #[arg(long)]
        no_http: bool,
    },
    /// Apply schema migrations and exit.
    Migrate,
    /// Print health after connecting.
    Health,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DaemonConfig {
    surreal: SurrealSection,
    search: SearchSection,
    embedding: EmbeddingSection,
    limits: LimitsSection,
    daemon: DaemonSection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SurrealSection {
    endpoint: String,
    namespace: String,
    database: String,
    username: Option<String>,
    password: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SearchSection {
    default_limit: u32,
    max_search_limit: u32,
    rrf_k: u32,
    enable_graph_enrich: bool,
    candidate_limit: u32,
    hnsw_ef: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EmbeddingSection {
    dimension: usize,
    /// Attach offline HashingEmbedder for read/write auto-embed (demo / tests).
    /// Not a production embedding model — labels geometry as hashing-only.
    #[serde(default)]
    hashing: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LimitsSection {
    max_content_bytes: usize,
    max_attrs_bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DaemonSection {
    bind: String,
    api_key: Option<String>,
    mcp_stdio: bool,
    http: bool,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            surreal: SurrealSection {
                endpoint: "memory".into(),
                namespace: "nomiso".into(),
                database: "memory".into(),
                username: None,
                password: None,
            },
            search: SearchSection {
                default_limit: 8,
                max_search_limit: 32,
                rrf_k: 60,
                enable_graph_enrich: true,
                candidate_limit: 32,
                hnsw_ef: 100,
            },
            embedding: EmbeddingSection {
                dimension: 1536,
                hashing: false,
            },
            limits: LimitsSection {
                max_content_bytes: 8192,
                max_attrs_bytes: 4096,
            },
            daemon: DaemonSection {
                bind: "127.0.0.1:9090".into(),
                api_key: None,
                mcp_stdio: false,
                http: true,
            },
        }
    }
}

fn load_config(path: Option<&PathBuf>) -> Result<DaemonConfig> {
    let mut fig = Figment::new().merge(Serialized::defaults(DaemonConfig::default()));
    if let Some(p) = path {
        fig = fig.merge(Toml::file(p));
    } else if std::path::Path::new("nomiso.toml").exists() {
        fig = fig.merge(Toml::file("nomiso.toml"));
    }
    // NOMI_SO__SURREAL__ENDPOINT style
    fig = fig.merge(Env::prefixed("NOMI_SO_").split("__"));
    fig.extract().context("load config")
}

#[allow(clippy::field_reassign_with_default)]
fn to_store_config(cfg: &DaemonConfig) -> StoreConfig {
    let mut sc = StoreConfig::default();
    sc.endpoint = cfg.surreal.endpoint.clone();
    sc.namespace = cfg.surreal.namespace.clone();
    sc.database = cfg.surreal.database.clone();
    sc.username = cfg.surreal.username.clone();
    sc.password = cfg.surreal.password.clone();
    sc.embedding_dim = cfg.embedding.dimension;
    sc.limits.embedding_dim = cfg.embedding.dimension;
    sc.limits.max_content_bytes = cfg.limits.max_content_bytes;
    sc.limits.max_attrs_bytes = cfg.limits.max_attrs_bytes;
    sc.limits.max_search_limit = cfg.search.max_search_limit;
    sc.search.default_limit = cfg.search.default_limit;
    sc.search.max_search_limit = cfg.search.max_search_limit;
    sc.search.rrf_k = cfg.search.rrf_k;
    sc.search.enable_graph_enrich = cfg.search.enable_graph_enrich;
    sc.search.candidate_limit = cfg.search.candidate_limit;
    sc.search.hnsw_ef = cfg.search.hnsw_ef;
    sc
}

async fn connect_client(cfg: &DaemonConfig) -> Result<NomisoClient> {
    let mut client = NomisoClient::connect(to_store_config(cfg)).await?;
    if cfg.embedding.hashing {
        use nomiso::HashingEmbedder;
        use std::sync::Arc;
        let emb = HashingEmbedder::new(cfg.embedding.dimension)
            .map_err(|e| anyhow::anyhow!("hashing embedder: {e}"))?;
        client = client.with_embedder(Arc::new(emb));
        info!(
            dim = cfg.embedding.dimension,
            "attached HashingEmbedder (demo/offline; not production vector quality)"
        );
    }
    Ok(client)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(Level::INFO.into())
                .from_env_lossy(),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let mut cfg = load_config(cli.config.as_ref())?;

    match cli.command {
        Commands::Migrate => {
            let client = connect_client(&cfg).await?;
            client.health().await?;
            info!("migrate complete (schema applied on connect)");
        }
        Commands::Health => {
            let client = connect_client(&cfg).await?;
            client.health().await?;
            println!("{{\"status\":\"ok\"}}");
        }
        Commands::Serve {
            bind,
            mcp_stdio,
            no_http,
        } => {
            if let Some(b) = bind {
                cfg.daemon.bind = b;
            }
            if mcp_stdio {
                cfg.daemon.mcp_stdio = true;
            }
            if no_http {
                cfg.daemon.http = false;
            }

            let client = connect_client(&cfg).await?;
            client.health().await?;

            let run_http = cfg.daemon.http && !cfg.daemon.mcp_stdio;
            let run_mcp = cfg.daemon.mcp_stdio;

            if run_mcp {
                info!("starting MCP stdio transport");
                nomiso::mcp_serve_stdio(client).await?;
                return Ok(());
            }

            if run_http {
                let addr: SocketAddr = cfg.daemon.bind.parse().context("parse bind")?;
                let api_key = cfg.daemon.api_key.map(SecretString::from);
                let app = http_router(HttpAppState { client, api_key });
                info!(%addr, "starting HTTP server");
                let listener = tokio::net::TcpListener::bind(addr).await?;
                axum::serve(listener, app)
                    .with_graceful_shutdown(shutdown_signal())
                    .await?;
                return Ok(());
            }

            anyhow::bail!("nothing to serve: enable HTTP or --mcp-stdio");
        }
    }
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    info!("shutdown signal received");
}
