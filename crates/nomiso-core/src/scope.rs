//! Scope path grammar and matching.

use crate::error::{Error, Result};

/// How a search scope should be matched against stored scopes.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ScopeMatch {
    /// Exact string equality (default; fail closed).
    #[default]
    Exact,
    /// Hierarchical prefix: query `org/acme` matches `org/acme/user/alice`.
    Prefix,
}

use serde::{Deserialize, Serialize};

/// Validated multi-dimensional scope path.
///
/// Convention: slash-separated segments, no leading/trailing slash,
/// no empty segments. Lowercase recommended but not required.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ScopePath(String);

impl ScopePath {
    /// Parse and validate a scope path.
    pub fn parse(raw: &str) -> Result<Self> {
        let s = raw.trim();
        if s.is_empty() {
            return Err(Error::invalid("scope must not be empty"));
        }
        if s.starts_with('/') || s.ends_with('/') {
            return Err(Error::invalid(
                "scope must not have leading or trailing '/'",
            ));
        }
        if s.contains("//") {
            return Err(Error::invalid("scope must not contain empty segments"));
        }
        if s.contains(char::is_whitespace) {
            return Err(Error::invalid("scope must not contain whitespace"));
        }
        if s.len() > 512 {
            return Err(Error::invalid("scope exceeds 512 characters"));
        }
        for seg in s.split('/') {
            if seg.is_empty() {
                return Err(Error::invalid("scope must not contain empty segments"));
            }
            if !seg
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
            {
                return Err(Error::invalid(format!(
                    "scope segment '{seg}' has invalid characters"
                )));
            }
        }
        Ok(Self(s.to_string()))
    }

    /// Borrow the path string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Does `stored` match this query scope under the given mode?
    pub fn matches(&self, stored: &str, mode: ScopeMatch) -> bool {
        match mode {
            ScopeMatch::Exact => self.0 == stored,
            ScopeMatch::Prefix => {
                stored == self.0
                    || stored
                        .strip_prefix(&self.0)
                        .is_some_and(|rest| rest.starts_with('/'))
            }
        }
    }
}

impl std::fmt::Display for ScopePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl AsRef<str> for ScopePath {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid() {
        assert!(ScopePath::parse("org/acme/user/alice").is_ok());
        assert!(ScopePath::parse("a").is_ok());
        assert!(ScopePath::parse("project-1/repo_2").is_ok());
    }

    #[test]
    fn parse_invalid() {
        assert!(ScopePath::parse("").is_err());
        assert!(ScopePath::parse("/org/acme").is_err());
        assert!(ScopePath::parse("org/acme/").is_err());
        assert!(ScopePath::parse("org//acme").is_err());
        assert!(ScopePath::parse("org acme").is_err());
        assert!(ScopePath::parse("org/acme!").is_err());
    }

    #[test]
    fn exact_and_prefix() {
        let q = ScopePath::parse("org/acme").unwrap();
        assert!(q.matches("org/acme", ScopeMatch::Exact));
        assert!(!q.matches("org/acme/user/alice", ScopeMatch::Exact));
        assert!(q.matches("org/acme", ScopeMatch::Prefix));
        assert!(q.matches("org/acme/user/alice", ScopeMatch::Prefix));
        assert!(!q.matches("org/acme2", ScopeMatch::Prefix));
        assert!(!q.matches("org/other", ScopeMatch::Prefix));
    }
}
