//! Active scope resolution: flag > env > config > error.
//! Empty flag/env is treated as unset (does not block config default).

use anyhow::{bail, Result};

/// Resolve scope for commands that require one.
pub fn resolve_scope(flag: Option<String>, config_default: Option<&str>) -> Result<String> {
    if let Some(s) = flag {
        let t = s.trim();
        if !t.is_empty() {
            return Ok(t.to_string());
        }
    }
    if let Ok(env) = std::env::var("VEGAPUNK_SCOPE") {
        let t = env.trim();
        if !t.is_empty() {
            return Ok(t.to_string());
        }
    }
    if let Some(s) = config_default {
        let t = s.trim();
        if !t.is_empty() {
            return Ok(t.to_string());
        }
    }
    bail!("scope required: pass --scope, set VEGAPUNK_SCOPE, or default_scope in vegapunk.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_wins() {
        let s = resolve_scope(Some("org/a".into()), Some("org/b")).unwrap();
        assert_eq!(s, "org/a");
    }

    #[test]
    fn empty_flag_falls_to_config() {
        let s = resolve_scope(Some("  ".into()), Some("org/b")).unwrap();
        assert_eq!(s, "org/b");
    }

    #[test]
    fn missing_errors() {
        assert!(resolve_scope(None, None).is_err());
        assert!(resolve_scope(Some("".into()), Some("  ")).is_err());
    }
}
