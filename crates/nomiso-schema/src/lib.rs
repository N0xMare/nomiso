//! Embedded SurrealQL schema migrations for Nomiso.
//!
//! Migrations are authored in this crate's `schema/` directory and embedded
//! at compile time. This directory is the single source of truth.

#![forbid(unsafe_code)]

use include_dir::{include_dir, Dir};
use nomiso_core::error::{Error, Result};

/// Compile-time embedded schema files (copied into this crate's schema/ tree).
static SCHEMA_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/schema");

/// A single ordered migration file.
#[derive(Debug, Clone)]
pub struct Migration {
    /// File stem e.g. `001_core`.
    pub name: String,
    /// SurrealQL source with placeholders unresolved.
    pub source: String,
}

/// Options for rendering migrations.
#[derive(Debug, Clone, Copy)]
pub struct MigrateOptions {
    /// HNSW embedding dimension (must match put vectors).
    pub embedding_dim: usize,
}

impl Default for MigrateOptions {
    fn default() -> Self {
        Self {
            embedding_dim: 1536,
        }
    }
}

/// Return ordered migrations (001, 002, 003, …) with `{{EMBEDDING_DIM}}` substituted.
pub fn migrations(opts: MigrateOptions) -> Result<Vec<Migration>> {
    if opts.embedding_dim == 0 {
        return Err(Error::invalid("embedding_dim must be > 0"));
    }
    let mut files: Vec<_> = SCHEMA_DIR
        .files()
        .filter(|f| {
            f.path()
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e == "surql")
        })
        .collect();
    files.sort_by_key(|f| f.path().to_path_buf());

    if files.is_empty() {
        return Err(Error::internal("no schema/*.surql files embedded"));
    }

    let mut out = Vec::with_capacity(files.len());
    for f in files {
        let name = f
            .path()
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| Error::internal("invalid migration filename"))?
            .to_string();
        let raw = f
            .contents_utf8()
            .ok_or_else(|| Error::internal(format!("migration {name} is not utf-8")))?;
        let source = raw.replace("{{EMBEDDING_DIM}}", &opts.embedding_dim.to_string());
        out.push(Migration { name, source });
    }
    Ok(out)
}

/// Concatenate all migrations into one script (for single-shot bootstrap).
pub fn combined_script(opts: MigrateOptions) -> Result<String> {
    let parts = migrations(opts)?;
    Ok(parts
        .into_iter()
        .map(|m| format!("-- {}\n{}\n", m.name, m.source))
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Schema package version marker stored in `nomiso_meta`.
pub const SCHEMA_VERSION: &str = "0.3.7";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embeds_three_migrations() {
        let m = migrations(MigrateOptions { embedding_dim: 8 }).expect("migrations");
        assert!(m.len() >= 3, "expected >=3 migrations, got {}", m.len());
        assert!(m[0].name.starts_with("001"));
        assert!(m.iter().any(|x| x.source.contains("DEFINE TABLE")));
        assert!(m.iter().any(|x| x.source.contains("HNSW DIMENSION 8")));
        assert!(!m.iter().any(|x| x.source.contains("{{EMBEDDING_DIM}}")));
    }

    #[test]
    fn embeds_phase_3e_audit_migration() {
        let m = migrations(MigrateOptions { embedding_dim: 8 }).expect("migrations");
        assert!(
            m.iter().any(|x| x.name.starts_with("006")),
            "expected 006_audit migration"
        );
        let audit = m.iter().find(|x| x.name.starts_with("006")).unwrap();
        assert!(audit.source.contains("sys_created"));
        assert!(audit.source.contains("belief_event"));
        assert!(audit.source.contains("task_state"));
        assert!(SCHEMA_VERSION.starts_with("0.3."));
        assert!(
            m.iter().any(|x| x.name.starts_with("007")),
            "expected 007 attrs flexible migration"
        );
    }

    #[test]
    fn combined_nonempty() {
        let s = combined_script(MigrateOptions::default()).unwrap();
        assert!(s.contains("memory"));
        assert!(s.contains("HNSW DIMENSION 1536"));
        assert!(s.contains("belief_event"));
        assert!(s.contains("task_state"));
    }
}
