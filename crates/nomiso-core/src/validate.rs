//! Validation for memory ops.

use jiff::Timestamp;

use crate::error::{Error, Result};
use crate::ops::{ForgetRequest, PutRequest, ReadRequest, SearchQuery, SupersedeRequest};
use crate::scope::ScopePath;
use crate::types::Category;

/// Limits applied during validation.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Max UTF-8 bytes of content.text.
    pub max_content_bytes: usize,
    /// Max serialized attrs bytes.
    pub max_attrs_bytes: usize,
    /// Max search limit.
    pub max_search_limit: u32,
    /// Configured embedding dimension (for mismatch checks).
    pub embedding_dim: usize,
    /// Max batch ids on read.
    pub max_read_ids: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_content_bytes: 8192,
            max_attrs_bytes: 4096,
            max_search_limit: 32,
            embedding_dim: 1536,
            max_read_ids: 64,
        }
    }
}

/// Validate a put body.
pub fn validate_put(req: &PutRequest, limits: &Limits) -> Result<ScopePath> {
    let scope = validate_put_shape(req, limits)?;
    validate_interval(req.valid_from, req.valid_until)?;
    Ok(scope)
}

#[doc = "Validate stable request shape without re-evaluating omitted time defaults on receipt replay."]
pub fn validate_put_shape(req: &PutRequest, limits: &Limits) -> Result<ScopePath> {
    let scope = ScopePath::parse(&req.scope)?;
    validate_content(&req.content, req.category, limits)?;
    if let Some(from) = req.valid_from {
        validate_interval_after_defaults(from, req.valid_until)?;
    }
    validate_confidence(req.confidence)?;
    validate_embedding(req.embedding.as_deref(), limits)?;
    if let Some(id) = &req.embedding_identity {
        if id.dimension as usize != limits.embedding_dim {
            return Err(Error::DimensionMismatch {
                expected: limits.embedding_dim,
                got: id.dimension as usize,
            });
        }
    }
    if let Some(key) = &req.idempotency_key {
        if key.trim().is_empty() || key.len() > 256 {
            return Err(Error::invalid(
                "idempotency_key must be non-blank and at most 256 bytes",
            ));
        }
    }
    Ok(scope)
}

/// Validate supersede.
pub fn validate_supersede(req: &SupersedeRequest, limits: &Limits) -> Result<ScopePath> {
    if req.prior_id.bare_key().trim().is_empty() {
        return Err(Error::invalid("prior_id is required"));
    }
    if req.expected_version == 0 || req.expected_version > i64::MAX as u64 {
        return Err(Error::invalid("expected_version must be in 1..=i64::MAX"));
    }
    if req.new.idempotency_key.is_some() {
        return Err(Error::invalid(
            "keyed supersede is not supported; reconcile the prior version before retrying",
        ));
    }
    validate_put(&req.new, limits)
}

/// Validate search.
pub fn validate_search(req: &SearchQuery, limits: &Limits) -> Result<ScopePath> {
    let scope = ScopePath::parse(&req.scope)?;
    let has_text = !req.query.trim().is_empty();
    let has_vec = req
        .embedding
        .as_ref()
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    if !has_text && !has_vec {
        return Err(Error::invalid(
            "search requires non-empty query text and/or embedding",
        ));
    }
    let qbytes = req.query.len();
    if qbytes > limits.max_content_bytes {
        return Err(Error::PayloadTooLarge(format!(
            "search.query is {qbytes} bytes (max {})",
            limits.max_content_bytes
        )));
    }
    if let Some(lim) = req.limit {
        if lim == 0 {
            return Err(Error::invalid("limit must be >= 1"));
        }
        if lim > limits.max_search_limit {
            return Err(Error::PayloadTooLarge(format!(
                "limit {lim} exceeds max_search_limit {}",
                limits.max_search_limit
            )));
        }
    }
    validate_embedding(req.embedding.as_deref(), limits)?;
    Ok(scope)
}

/// Validate read.
pub fn validate_read(req: &ReadRequest, limits: &Limits) -> Result<ScopePath> {
    let scope = ScopePath::parse(&req.scope)?;
    if req.ids.is_empty() {
        return Err(Error::invalid("read requires at least one id"));
    }
    if req.ids.len() > limits.max_read_ids {
        return Err(Error::PayloadTooLarge(format!(
            "read ids exceed max {}",
            limits.max_read_ids
        )));
    }
    Ok(scope)
}

/// Validate forget.
pub fn validate_forget(req: &ForgetRequest) -> Result<ScopePath> {
    if req.id.bare_key().trim().is_empty() {
        return Err(Error::invalid("forget requires id"));
    }
    if req
        .expected_version
        .is_some_and(|v| v == 0 || v > i64::MAX as u64)
    {
        return Err(Error::invalid("expected_version must be in 1..=i64::MAX"));
    }
    ScopePath::parse(&req.scope)
}

fn validate_content(
    content: &crate::types::Content,
    category: Category,
    limits: &Limits,
) -> Result<()> {
    let bytes = content.text.len();
    if bytes > limits.max_content_bytes {
        return Err(Error::PayloadTooLarge(format!(
            "content.text is {bytes} bytes (max {})",
            limits.max_content_bytes
        )));
    }
    let attrs_bytes = content.attrs_bytes();
    if attrs_bytes > limits.max_attrs_bytes {
        return Err(Error::PayloadTooLarge(format!(
            "content.attrs is {attrs_bytes} bytes (max {})",
            limits.max_attrs_bytes
        )));
    }
    // Searchable categories need non-empty text.
    match category {
        Category::Trace => {}
        _ if content.text.trim().is_empty() => {
            return Err(Error::invalid(
                "content.text must be non-empty for this category",
            ));
        }
        _ => {}
    }
    Ok(())
}

/// Require `valid_until >= valid_from` after put/supersede defaults.
///
/// `from` is the effective valid-from (caller-supplied, or defaulted to now /
/// close_at). Open-ended `until` (`None`) is allowed.
pub fn validate_interval_after_defaults(from: Timestamp, until: Option<Timestamp>) -> Result<()> {
    if let Some(u) = until {
        if u < from {
            return Err(Error::invalid(format!(
                "valid_until ({u}) must be >= valid_from ({from})"
            )));
        }
    }
    Ok(())
}

fn validate_interval(from: Option<Timestamp>, until: Option<Timestamp>) -> Result<()> {
    // Omitted valid_from is now on write; historical ingest must set both bounds.
    match (from, until) {
        (_, None) => Ok(()),
        (Some(f), Some(_)) => validate_interval_after_defaults(f, until),
        (None, Some(_)) => validate_interval_after_defaults(Timestamp::now(), until),
    }
}

fn validate_confidence(c: Option<f64>) -> Result<()> {
    if let Some(v) = c {
        if !(0.0..=1.0).contains(&v) || v.is_nan() {
            return Err(Error::invalid("confidence must be in [0, 1]"));
        }
    }
    Ok(())
}

fn validate_embedding(embedding: Option<&[f32]>, limits: &Limits) -> Result<()> {
    if let Some(v) = embedding {
        if v.is_empty() {
            return Err(Error::invalid("embedding must not be empty when provided"));
        }
        if v.len() != limits.embedding_dim {
            return Err(Error::DimensionMismatch {
                expected: limits.embedding_dim,
                got: v.len(),
            });
        }
        if v.iter().any(|x| !x.is_finite()) {
            return Err(Error::invalid("embedding must contain only finite floats"));
        }
    }
    Ok(())
}

/// Pure supersession version check.
pub fn check_version(expected: u64, found: u64) -> Result<()> {
    if expected != found {
        return Err(Error::Conflict { expected, found });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Content;

    fn put(text: &str) -> PutRequest {
        PutRequest {
            scope: "org/acme".into(),
            category: Category::Semantic,
            content: Content::text(text),
            valid_from: None,
            valid_until: None,
            known_at: None,
            confidence: Some(0.9),
            provenance: Default::default(),
            entity_links: vec![],
            embedding: None,
            embedding_identity: None,
            idempotency_key: None,
            extractor_version: None,
            model_version: None,
            valid_rev_from: None,
            valid_rev_until: None,
        }
    }

    #[test]
    fn put_ok() {
        let limits = Limits::default();
        assert!(validate_put(&put("hello"), &limits).is_ok());
    }

    #[test]
    fn empty_text_rejected() {
        let limits = Limits::default();
        assert!(validate_put(&put("  "), &limits).is_err());
    }

    #[test]
    fn dimension_mismatch() {
        let limits = Limits {
            embedding_dim: 4,
            ..Default::default()
        };
        let mut req = put("vec");
        req.embedding = Some(vec![0.1, 0.2]);
        match validate_put(&req, &limits) {
            Err(Error::DimensionMismatch {
                expected: 4,
                got: 2,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn search_requires_signal() {
        let limits = Limits::default();
        let q = SearchQuery {
            query: String::new(),
            scope: "org/acme".into(),
            scope_match: Default::default(),
            as_of: None,
            known_as_of: None,
            sys_as_of: None,
            categories: None,
            limit: Some(8),
            embedding: None,
            graph_enrich: None,
            graph_expand: None,
        };
        assert!(validate_search(&q, &limits).is_err());
    }

    #[test]
    fn search_query_length_and_empty_vector() {
        let limits = Limits::default();
        let q = |query: String, embedding: Option<Vec<f32>>| SearchQuery {
            query,
            scope: "org/acme".into(),
            scope_match: Default::default(),
            as_of: None,
            known_as_of: None,
            sys_as_of: None,
            categories: None,
            limit: Some(8),
            embedding,
            graph_enrich: None,
            graph_expand: None,
        };
        let too_long = q("x".repeat(limits.max_content_bytes + 1), None);
        assert!(matches!(
            validate_search(&too_long, &limits),
            Err(Error::PayloadTooLarge(_))
        ));
        let at_limit = q("x".repeat(limits.max_content_bytes), None);
        assert!(validate_search(&at_limit, &limits).is_ok());
        let padded = q(
            format!("{}needle", " ".repeat(limits.max_content_bytes)),
            None,
        );
        assert!(
            matches!(
                validate_search(&padded, &limits),
                Err(Error::PayloadTooLarge(_))
            ),
            "whitespace must not bypass the query byte cap"
        );
        let empty_vec = q("needle".into(), Some(vec![]));
        assert!(
            matches!(
                validate_search(&empty_vec, &limits),
                Err(Error::InvalidOp(_))
            ),
            "a supplied empty embedding must be rejected, not re-embedded"
        );
    }

    #[test]
    fn version_conflict() {
        assert!(check_version(1, 1).is_ok());
        assert!(matches!(
            check_version(1, 2),
            Err(Error::Conflict {
                expected: 1,
                found: 2
            })
        ));
    }

    #[test]
    fn omitted_from_past_until_rejected() {
        let limits = Limits::default();
        let mut req = put("past until only");
        req.valid_until = Some("2000-01-01T00:00:00Z".parse().unwrap());
        assert!(matches!(
            validate_put(&req, &limits),
            Err(Error::InvalidOp(_))
        ));
    }

    #[test]
    fn historical_interval_both_bounds_ok() {
        let limits = Limits::default();
        let mut req = put("historical both bounds");
        req.valid_from = Some("2000-01-01T00:00:00Z".parse().unwrap());
        req.valid_until = Some("2001-01-01T00:00:00Z".parse().unwrap());
        assert!(validate_put(&req, &limits).is_ok());
    }

    #[test]
    fn inverted_interval_both_set_rejected() {
        let limits = Limits::default();
        let mut req = put("inverted");
        req.valid_from = Some("2001-01-01T00:00:00Z".parse().unwrap());
        req.valid_until = Some("2000-01-01T00:00:00Z".parse().unwrap());
        assert!(matches!(
            validate_put(&req, &limits),
            Err(Error::InvalidOp(_))
        ));
    }
}
