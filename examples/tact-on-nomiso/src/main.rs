//! CLI demo for Tact-on-Nomiso.

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use nomiso::StoreConfig;
use tact_on_nomiso::{Actor, ReplaceTarget, TactMemory, TactMemoryConfig};

#[derive(Parser)]
#[command(name = "tact-on-nomiso", about = "Tact-shaped memory on Nomiso")]
struct Cli {
    #[arg(long, default_value = "memory")]
    endpoint: String,

    #[arg(long, default_value_t = 8)]
    embed_dim: usize,

    /// Act as child agent (scan/read only).
    #[arg(long, default_value_t = false)]
    child: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Scan {
        #[arg(long)]
        query: String,
        #[arg(long, default_value_t = 5)]
        limit: u32,
    },
    Read {
        #[arg(long)]
        id: String,
    },
    Put {
        #[arg(long)]
        content: String,
        #[arg(long)]
        replace_id: Option<String>,
        #[arg(long)]
        replace_version: Option<u64>,
    },
    Delete {
        #[arg(long)]
        id: String,
        #[arg(long)]
        version: u64,
    },
    /// Print content-free review checkpoint text (for harnesses).
    CheckpointText,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let actor = if cli.child { Actor::Child } else { Actor::Root };

    if matches!(cli.command, Commands::CheckpointText) {
        println!("{}", TactMemory::review_checkpoint_text());
        return Ok(());
    }

    if cli.endpoint.starts_with("rocksdb://") {
        #[cfg(not(feature = "embedded-rocks"))]
        {
            bail!(
                "endpoint {} needs --features embedded-rocks (Surreal file engine)",
                cli.endpoint
            );
        }
    }
    let store = if cli.endpoint == "memory" || cli.endpoint == "mem://" {
        StoreConfig::memory_test(cli.embed_dim)
    } else if cli.endpoint.starts_with("rocksdb://") {
        let path = cli
            .endpoint
            .strip_prefix("rocksdb://")
            .unwrap_or(&cli.endpoint);
        StoreConfig::rocksdb_path(path, cli.embed_dim)
    } else {
        StoreConfig {
            endpoint: cli.endpoint.clone(),
            embedding_dim: cli.embed_dim,
            limits: nomiso::Limits {
                embedding_dim: cli.embed_dim,
                ..Default::default()
            },
            ..StoreConfig::default()
        }
    };

    let mem = TactMemory::connect(store, TactMemoryConfig::default())
        .await
        .context("connect")?;

    match cli.command {
        Commands::Scan { query, limit } => {
            let scan = mem.scan(actor, &query, limit).await?;
            println!("{}", serde_json::to_string_pretty(&scan)?);
        }
        Commands::Read { id } => {
            let id = nomiso::MemoryId::new(id);
            let rows = mem.read(actor, &[id]).await?;
            println!("{}", serde_json::to_string_pretty(&rows)?);
        }
        Commands::Put {
            content,
            replace_id,
            replace_version,
        } => {
            let replace = match (replace_id, replace_version) {
                (Some(id), Some(v)) => Some(ReplaceTarget {
                    id: nomiso::MemoryId::new(id),
                    expected_version: v,
                }),
                (None, None) => None,
                _ => bail!("replace requires both --replace-id and --replace-version"),
            };
            // CLI convenience: arm scan if needed
            if mem.config().require_scan_before_put {
                let _ = mem.scan(Actor::Root, &content, 5).await?;
            }
            let out = mem.put(actor, &content, replace).await?;
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        Commands::Delete { id, version } => {
            mem.delete(actor, nomiso::MemoryId::new(id), version)
                .await?;
            println!(r#"{{"status":"deleted"}}"#);
        }
        Commands::CheckpointText => unreachable!(),
    }
    Ok(())
}
