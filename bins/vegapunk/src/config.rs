//! Project/user config for the AXI CLI (`vegapunk.toml`).

use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

/// Base for resolving relative storage paths declared in a config file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
#[value(rename_all = "lowercase")]
pub enum PathBase {
    /// Relative to the process working directory (legacy behavior).
    Cwd,
    /// Relative to the config file's directory.
    Config,
}

/// Defaults loaded from TOML / env / flags (flags applied by caller).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VegapunkFileConfig {
    /// Surreal endpoint (`rocksdb://…`, `ws://…`, or explicit `memory` demo).
    #[serde(default = "default_endpoint")]
    pub endpoint: String,
    /// Profile name.
    #[serde(default = "default_profile")]
    pub profile: String,
    /// Embedding dimension (must match store HNSW + embedder).
    #[serde(default = "default_embed_dim")]
    pub embed_dim: usize,
    /// Default scope when flag/env omitted.
    #[serde(default)]
    pub default_scope: Option<String>,
    /// Host soft-inject opt-in (product never auto-injects).
    #[serde(default)]
    pub soft_inject: bool,
    /// Where relative `endpoint`/`blob_root` resolve: `cwd` (legacy) or `config`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_base: Option<PathBase>,
    /// Local CAS blob root (default `.nomiso-blobs` on the selected base;
    /// `VEGAPUNK_BLOB_ROOT` overrides, always resolved against cwd).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob_root: Option<PathBuf>,
    /// OpenAI-compatible embeddings base URL (e.g. `https://api.openai.com/v1`).
    /// When set, replaces the hashing embedder. Key is read from `embed_api_key_env`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embed_url: Option<String>,
    /// Embed model id (HTTP path). Default `text-embedding-3-small` when URL is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embed_model: Option<String>,
    /// Env var holding the embed API key (never store the key in this file).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embed_api_key_env: Option<String>,
    /// Set by `load_config` when `endpoint` was present in the file.
    #[serde(skip)]
    pub endpoint_set: bool,
    /// Set by `load_config` when `blob_root` was present in the file.
    #[serde(skip)]
    pub blob_root_set: bool,
}

/// Local durable default (Surreal embedded Rocks). Explicit `memory` is the demo escape.
pub const DEFAULT_ENDPOINT: &str = "rocksdb://./.nomiso-data";

/// Default CAS blob root (relative to the selected `path_base`).
pub const DEFAULT_BLOB_ROOT: &str = ".nomiso-blobs";

fn default_endpoint() -> String {
    DEFAULT_ENDPOINT.into()
}
fn default_profile() -> String {
    "coding-agent".into()
}
fn default_embed_dim() -> usize {
    32
}

impl Default for VegapunkFileConfig {
    fn default() -> Self {
        Self {
            endpoint: default_endpoint(),
            profile: default_profile(),
            embed_dim: default_embed_dim(),
            default_scope: None,
            soft_inject: false,
            path_base: None,
            blob_root: None,
            embed_url: None,
            embed_model: None,
            embed_api_key_env: None,
            endpoint_set: false,
            blob_root_set: false,
        }
    }
}

/// Locate config: `VEGAPUNK_CONFIG` > `./vegapunk.toml` > `~/.config/vegapunk/config.toml`.
pub fn find_config_path() -> Result<Option<PathBuf>> {
    let explicit = std::env::var_os("VEGAPUNK_CONFIG").map(PathBuf::from);
    let cwd = std::env::current_dir().context("current directory")?;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    find_config_path_from(explicit, &cwd, home.as_deref())
}

/// Pure resolver: an explicit `VEGAPUNK_CONFIG` path must exist and be a
/// regular file; a discovered candidate that exists but is not a readable
/// regular file is an error, not a fallback.
pub fn find_config_path_from(
    explicit: Option<PathBuf>,
    cwd: &Path,
    home: Option<&Path>,
) -> Result<Option<PathBuf>> {
    if let Some(p) = explicit {
        let p = if p.is_absolute() { p } else { cwd.join(p) };
        let m = std::fs::metadata(&p)
            .with_context(|| format!("VEGAPUNK_CONFIG path not found: {}", p.display()))?;
        if !m.is_file() {
            bail!("VEGAPUNK_CONFIG is not a regular file: {}", p.display());
        }
        return Ok(Some(p));
    }
    let mut candidates = vec![cwd.join("vegapunk.toml")];
    if let Some(h) = home {
        candidates.push(h.join(".config/vegapunk/config.toml"));
    }
    for cand in candidates {
        match std::fs::metadata(&cand) {
            Ok(m) => {
                if !m.is_file() {
                    bail!("config path is not a regular file: {}", cand.display());
                }
                return Ok(Some(cand));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(e).with_context(|| format!("cannot access config {}", cand.display()));
            }
        }
    }
    Ok(None)
}

/// Load config file if present; otherwise defaults.
pub fn load_config() -> Result<(VegapunkFileConfig, Option<PathBuf>)> {
    let Some(path) = find_config_path()? else {
        return Ok((VegapunkFileConfig::default(), None));
    };
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("read config {}", path.display()))?;
    let mut cfg: VegapunkFileConfig =
        toml::from_str(&raw).with_context(|| format!("parse config {}", path.display()))?;
    let table: toml::Table = raw
        .parse()
        .with_context(|| format!("parse config {}", path.display()))?;
    cfg.endpoint_set = table.contains_key("endpoint");
    cfg.blob_root_set = table.contains_key("blob_root");
    Ok((cfg, Some(path)))
}

fn local_endpoint_path(endpoint: &str) -> Option<&str> {
    for scheme in ["rocksdb://", "surrealkv://"] {
        if let Some(rest) = endpoint.strip_prefix(scheme) {
            return Some(rest);
        }
    }
    None
}

fn joined(base: &Path, rel: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in base.join(rel).components() {
        match c {
            Component::CurDir => {}
            _ => out.push(c.as_os_str()),
        }
    }
    out
}

pub(crate) fn resolve_local_endpoint(endpoint: &str, base: &Path) -> Result<String> {
    for scheme in ["rocksdb://", "surrealkv://"] {
        if let Some(rest) = endpoint.strip_prefix(scheme) {
            if rest.trim().is_empty() {
                bail!("local endpoint path must be non-empty");
            }
            let p = Path::new(rest);
            if p.is_relative() {
                return Ok(format!("{scheme}{}", joined(base, p).display()));
            }
        }
    }
    Ok(endpoint.to_string())
}

fn same_directory(a: &Path, b: &Path) -> bool {
    a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b)
}

/// Resolve `endpoint`/`blob_root` relative paths in a file config against the
/// selected base (`cwd` legacy, or the config directory with `path_base =
/// "config"`). Remote and memory endpoints pass through. A legacy config that
/// lives outside cwd and declares explicit relative storage paths is
/// ambiguous: refuse rather than silently relocate data.
pub fn resolve_storage_paths(
    cfg: &mut VegapunkFileConfig,
    config_path: Option<&Path>,
    cwd: &Path,
) -> Result<()> {
    let cfg_dir = config_path.and_then(|p| p.parent()).map(Path::to_path_buf);
    let base = match cfg.path_base {
        Some(PathBase::Config) => cfg_dir.clone().unwrap_or_else(|| cwd.to_path_buf()),
        Some(PathBase::Cwd) | None => cwd.to_path_buf(),
    };
    if cfg.path_base.is_none() && cfg_dir.as_deref().is_some_and(|d| !same_directory(d, cwd)) {
        let rel_endpoint = cfg.endpoint_set
            && local_endpoint_path(&cfg.endpoint).is_some_and(|p| Path::new(p).is_relative());
        let rel_blob =
            cfg.blob_root_set && cfg.blob_root.as_deref().is_some_and(|p| p.is_relative());
        if rel_endpoint || rel_blob {
            bail!(
                "config {} declares relative storage paths but lives outside working \
                 directory {}; set `path_base = \"cwd\"` or `path_base = \"config\"` to choose",
                config_path.unwrap_or_else(|| Path::new("<none>")).display(),
                cwd.display()
            );
        }
    }
    cfg.endpoint = resolve_local_endpoint(&cfg.endpoint, &base)?;
    let blob = cfg
        .blob_root
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_BLOB_ROOT));
    cfg.blob_root = Some(if blob.is_relative() {
        joined(&base, &blob)
    } else {
        blob
    });
    Ok(())
}

/// Write example config to path (does not overwrite unless `force`).
pub fn write_example(path: &Path, force: bool) -> Result<()> {
    if path.exists() && !force {
        anyhow::bail!(
            "{} already exists (pass --force to overwrite)",
            path.display()
        );
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(path, EXAMPLE_TOML)?;
    Ok(())
}

pub const EXAMPLE_TOML: &str = r#"# Vegapunk product defaults (project-local or ~/.config/vegapunk/config.toml)
# Precedence: CLI flags / env > this file > built-ins
# Docs: docs/spec/10-interfaces-and-integration.md
#
# Default is local durable Surreal (embedded Rocks). Data dir is gitignored.
# CAS bytes: VEGAPUNK_BLOB_ROOT or ./.nomiso-blobs (also gitignored).
# Do not run CLI + long-lived mcp/serve against the same rocksdb:// at once.
# Demo escape (ephemeral per process):  endpoint = "memory"
# Shared daemon:                       endpoint = "ws://127.0.0.1:8000/rpc"

endpoint = "rocksdb://./.nomiso-data"
path_base = "config"
profile = "coding-agent"
embed_dim = 32
default_scope = "org/local/user/dev"
# soft_inject is reserved Layer 3 (capped dump). Layer 1 standing header is also off.
# Product never auto-injects turn facts (Layer 2 is pack.block after hard-recall).
# soft_inject = false

# Production embed (OpenAI-compatible). Hashing stays the offline/test default.
# When embed_url is set, hashing is not attached. Match embed_dim to the model.
#   embed_url = "https://api.openai.com/v1"
#   embed_model = "text-embedding-3-small"
#   embed_dim = 1536
#   embed_api_key_env = "VEGAPUNK_EMBED_API_KEY"   # or OPENAI_API_KEY
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_endpoint_is_durable_rocks() {
        let cfg = VegapunkFileConfig::default();
        assert!(cfg.endpoint.starts_with("rocksdb://"));
        assert!(!matches!(
            cfg.endpoint.as_str(),
            "memory" | "mem://" | "memory://" | "mem" | ""
        ));
    }

    #[test]
    fn example_toml_parses() {
        let cfg: VegapunkFileConfig = toml::from_str(EXAMPLE_TOML).expect("example toml");
        assert_eq!(cfg.endpoint, DEFAULT_ENDPOINT);
        assert_eq!(cfg.profile, "coding-agent");
        assert!(cfg.embed_url.is_none());
        assert_eq!(cfg.path_base, Some(PathBase::Config));
    }

    #[test]
    fn find_explicit_missing_and_nonfile_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("nope.toml");
        assert!(find_config_path_from(Some(missing), tmp.path(), None).is_err());
        let dir = tmp.path().join("adir");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(find_config_path_from(Some(dir), tmp.path(), None).is_err());
    }

    #[test]
    fn find_discovered_local_wins_and_invalid_type_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(home.join(".config/vegapunk")).unwrap();
        let user = home.join(".config/vegapunk/config.toml");
        std::fs::write(&user, "profile = \"x\"\n").unwrap();
        let found = find_config_path_from(None, tmp.path(), Some(&home)).unwrap();
        assert_eq!(found, Some(user));
        std::fs::create_dir_all(tmp.path().join("vegapunk.toml")).unwrap();
        assert!(find_config_path_from(None, tmp.path(), Some(&home)).is_err());
    }

    #[test]
    fn resolve_storage_paths_config_base() {
        let tmp = PathBuf::from("/tmp/vg-base-test");
        let cfg_dir = tmp.join("cfg");
        let cwd = tmp.join("cwd");
        let mut cfg = VegapunkFileConfig {
            endpoint: "rocksdb://./data".into(),
            endpoint_set: true,
            path_base: Some(PathBase::Config),
            ..Default::default()
        };
        resolve_storage_paths(&mut cfg, Some(&cfg_dir.join("vegapunk.toml")), &cwd).unwrap();
        assert_eq!(
            cfg.endpoint,
            format!("rocksdb://{}", cfg_dir.join("data").display())
        );
        assert_eq!(cfg.blob_root, Some(cfg_dir.join(".nomiso-blobs")));
    }

    #[test]
    fn resolve_storage_paths_legacy_ambiguity() {
        let tmp = PathBuf::from("/tmp/vg-base-test2");
        let cfg_dir = tmp.join("cfg");
        let cwd = tmp.join("cwd");
        let mut cfg = VegapunkFileConfig {
            endpoint: "rocksdb://./data".into(),
            endpoint_set: true,
            ..Default::default()
        };
        let err = resolve_storage_paths(&mut cfg, Some(&cfg_dir.join("vegapunk.toml")), &cwd)
            .unwrap_err();
        assert!(err.to_string().contains("path_base"), "{err}");

        // Same file in cwd: legacy cwd behavior preserved.
        let mut in_cwd = cfg.clone();
        resolve_storage_paths(&mut in_cwd, Some(&cwd.join("vegapunk.toml")), &cwd).unwrap();
        assert_eq!(
            in_cwd.endpoint,
            format!("rocksdb://{}", cwd.join("data").display())
        );

        // Explicit relative blob_root outside cwd is also ambiguous.
        let mut blob_only = VegapunkFileConfig {
            endpoint: "memory".into(),
            blob_root: Some(PathBuf::from("blobs")),
            blob_root_set: true,
            ..Default::default()
        };
        assert!(
            resolve_storage_paths(&mut blob_only, Some(&cfg_dir.join("vegapunk.toml")), &cwd)
                .is_err()
        );

        // Implicit default blob_root never triggers ambiguity on its own.
        let mut default_blob = VegapunkFileConfig {
            endpoint: "memory".into(),
            ..Default::default()
        };
        resolve_storage_paths(
            &mut default_blob,
            Some(&cfg_dir.join("vegapunk.toml")),
            &cwd,
        )
        .unwrap();
        assert_eq!(default_blob.blob_root, Some(cwd.join(".nomiso-blobs")));
    }

    #[test]
    fn explicit_relative_config_uses_supplied_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let selected = tmp.path().join("chosen.toml");
        std::fs::write(&selected, "endpoint = \"memory\"\n").unwrap();
        assert_eq!(
            find_config_path_from(Some("chosen.toml".into()), tmp.path(), None).unwrap(),
            Some(selected)
        );
        assert!(resolve_local_endpoint("rocksdb://", tmp.path()).is_err());
        assert!(resolve_local_endpoint("surrealkv://   ", tmp.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn parent_components_preserve_symlink_resolution() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("base");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::create_dir_all(outside.join("child")).unwrap();
        std::os::unix::fs::symlink(outside.join("child"), base.join("link")).unwrap();
        let endpoint = resolve_local_endpoint("rocksdb://link/../data", &base).unwrap();
        let path = Path::new(endpoint.strip_prefix("rocksdb://").unwrap());
        std::fs::create_dir_all(path).unwrap();
        assert_eq!(
            path.canonicalize().unwrap(),
            outside.join("data").canonicalize().unwrap()
        );
        assert!(!base.join("data").exists());
    }
}
