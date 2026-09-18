//! Map Surreal records ↔ nomiso-core types (via JSON intermediate for SDK 3.x).

use jiff::Timestamp;
use nomiso_core::error::{Error, Result};
use nomiso_core::types::{
    Category, Content, MemoryId, MemoryRecord, Provenance, ScoreKind, SearchHit, SearchSignals,
};
use serde::Deserialize;
use serde_json::Value;

/// Wire representation of a memory row as returned by Surreal (JSON form).
#[derive(Debug, Clone, Deserialize)]
pub struct MemoryRow {
    pub id: Value,
    pub category: String,
    pub scope: String,
    pub content: ContentWire,
    #[serde(default)]
    pub embedding: Option<Vec<f32>>,
    #[serde(default)]
    pub embedding_generation: Option<u64>,
    pub valid_from: Value,
    #[serde(default)]
    pub valid_until: Option<Value>,
    pub known_at: Value,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub provenance: Option<Provenance>,
    pub version: u64,
    #[serde(default)]
    pub supersedes: Option<Value>,
    #[serde(default)]
    pub superseded_by: Option<Value>,
    #[serde(default)]
    pub entity_links: Option<Vec<String>>,
    #[serde(default)]
    pub stale: Option<bool>,
    #[serde(default)]
    pub extractor_version: Option<String>,
    #[serde(default)]
    pub model_version: Option<String>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub valid_rev_from: Option<String>,
    #[serde(default)]
    pub valid_rev_until: Option<String>,
    #[serde(default)]
    pub sys_created: Option<Value>,
    #[serde(default)]
    pub sys_updated: Option<Value>,
    #[serde(default)]
    pub sys_closed: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ContentWire {
    pub text: String,
    #[serde(default)]
    pub attrs: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SearchRow {
    pub id: Value,
    #[serde(default)]
    pub rrf_score: Option<f64>,
    #[serde(default)]
    pub score: Option<f64>,
    #[serde(default)]
    pub distance: Option<f64>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub content: Option<ContentWire>,
    #[serde(default)]
    pub valid_from: Option<Value>,
    #[serde(default)]
    pub valid_until: Option<Value>,
    #[serde(default)]
    pub provenance: Option<Provenance>,
    #[serde(default)]
    pub version: Option<u64>,
    #[serde(default)]
    pub embedding_generation: Option<u64>,
}

pub fn record_id_to_string(v: &Value) -> Result<String> {
    match v {
        Value::String(s) => Ok(normalize_id(s)),
        Value::Object(map) => {
            if let Some(Value::String(s)) = map.get("String") {
                return Ok(normalize_id(s));
            }
            // Surreal record object: { "tb": "memory", "id": ... }
            if let (Some(tb), Some(id)) = (map.get("tb"), map.get("id")) {
                let id_s = match id {
                    Value::String(s) => s.clone(),
                    Value::Object(o) => o
                        .get("String")
                        .and_then(|x| x.as_str())
                        .or_else(|| {
                            o.values().find_map(|x| match x {
                                Value::String(s) => Some(s.as_str()),
                                Value::Number(_) => None,
                                _ => x.as_str(),
                            })
                        })
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| id.to_string().trim_matches('"').to_string()),
                    other => other.to_string().trim_matches('"').to_string(),
                };
                let tb_s = tb.as_str().unwrap_or("memory");
                return Ok(format!("{tb_s}:{id_s}"));
            }
            Ok(normalize_id(v.to_string().trim_matches('"')))
        }
        other => Ok(normalize_id(other.to_string().trim_matches('"'))),
    }
}

fn normalize_id(s: &str) -> String {
    let s = s
        .trim()
        .trim_matches(|c| c == '`' || c == '"' || c == '⟨' || c == '⟩');
    // Surreal sometimes returns memory:`uuid` — strip nested backticks
    let s = s.replace('`', "");
    if let Some(rest) = s.strip_prefix("memory:") {
        let rest = rest.trim_matches(|c| c == '`' || c == '⟨' || c == '⟩');
        format!("memory:{rest}")
    } else if s.contains(':') {
        s.to_string()
    } else {
        format!("memory:{s}")
    }
}

pub fn parse_timestamp(v: &Value) -> Result<Timestamp> {
    match v {
        Value::String(s) => s
            .parse::<Timestamp>()
            .map_err(|e| Error::store(format!("invalid timestamp '{s}': {e}"))),
        Value::Object(map) => {
            if let Some(Value::String(s)) = map.get("dt").or_else(|| map.get("String")) {
                return s
                    .parse::<Timestamp>()
                    .map_err(|e| Error::store(format!("invalid timestamp: {e}")));
            }
            // datetime as object with seconds
            if let Some(Value::Number(n)) = map.get("timestamp").or_else(|| map.get("secs")) {
                let secs = n.as_i64().unwrap_or(0);
                return Timestamp::from_second(secs)
                    .map_err(|e| Error::store(format!("timestamp: {e}")));
            }
            Err(Error::store(format!("unsupported timestamp object: {v}")))
        }
        Value::Number(n) => {
            let f = n.as_f64().unwrap_or(0.0);
            let secs = if f > 1e12 { f / 1000.0 } else { f };
            let whole = secs.trunc() as i64;
            Timestamp::from_second(whole).map_err(|e| Error::store(format!("timestamp: {e}")))
        }
        _ => Err(Error::store(format!("unsupported timestamp: {v}"))),
    }
}

pub fn now() -> Timestamp {
    Timestamp::now()
}

pub fn ts_param(ts: Timestamp) -> String {
    ts.to_string()
}

pub fn value_to_memory_row(v: Value) -> Result<MemoryRow> {
    serde_json::from_value(v).map_err(|e| Error::store(format!("decode memory row: {e}")))
}

pub fn value_to_search_row(v: Value) -> Result<SearchRow> {
    serde_json::from_value(v).map_err(|e| Error::store(format!("decode search row: {e}")))
}

pub fn row_to_record(row: MemoryRow) -> Result<MemoryRecord> {
    let id_s = record_id_to_string(&row.id)?;
    let category = Category::parse(&row.category)
        .ok_or_else(|| Error::store(format!("unknown category {}", row.category)))?;
    let valid_from = parse_timestamp(&row.valid_from)?;
    let valid_until = match row.valid_until {
        None | Some(Value::Null) => None,
        Some(v) => Some(parse_timestamp(&v)?),
    };
    let known_at = parse_timestamp(&row.known_at)?;
    let supersedes = match row.supersedes {
        None | Some(Value::Null) => None,
        Some(v) => Some(MemoryId::new(record_id_to_string(&v)?)),
    };
    let superseded_by = match row.superseded_by {
        None | Some(Value::Null) => None,
        Some(v) => Some(MemoryId::new(record_id_to_string(&v)?)),
    };
    Ok(MemoryRecord {
        id: MemoryId::new(id_s),
        category,
        scope: row.scope,
        content: Content {
            text: row.content.text,
            attrs: row.content.attrs,
        },
        embedding: row.embedding,
        embedding_generation: row.embedding_generation,
        valid_from,
        valid_until,
        known_at,
        confidence: row.confidence,
        provenance: row.provenance.unwrap_or_default(),
        version: row.version,
        supersedes,
        superseded_by,
        entity_links: row.entity_links.unwrap_or_default(),
        stale: row.stale,
        extractor_version: row.extractor_version,
        model_version: row.model_version,
        idempotency_key: row.idempotency_key,
        valid_rev_from: row.valid_rev_from,
        valid_rev_until: row.valid_rev_until,
        sys_created: match row.sys_created {
            None | Some(Value::Null) => None,
            Some(v) => Some(parse_timestamp(&v)?),
        },
        sys_updated: match row.sys_updated {
            None | Some(Value::Null) => None,
            Some(v) => Some(parse_timestamp(&v)?),
        },
        sys_closed: match row.sys_closed {
            None | Some(Value::Null) => None,
            Some(v) => Some(parse_timestamp(&v)?),
        },
    })
}

/// Extract a finite score from common Surreal/JSON shapes (tests + diagnostics).
#[allow(dead_code)]
pub fn extract_score(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64().filter(|x| x.is_finite()),
        Value::Object(map) => {
            for key in ["rrf_score", "score", "ft_score", "linear_score", "distance"] {
                if let Some(raw) = map.get(key) {
                    if let Some(s) = extract_score(raw) {
                        if key == "distance" {
                            return Some(1.0 / (1.0 + s.max(0.0)));
                        }
                        if s != 0.0 {
                            return Some(s);
                        }
                        // keep zero as candidate but prefer other keys
                        if key != "distance" {
                            return Some(s);
                        }
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// Derive per-hit channel signals from row keys (not branch constants alone).
pub fn signals_from_search_row(row: &SearchRow, branch: &SearchSignals) -> SearchSignals {
    let has_distance = row.distance.filter(|d| d.is_finite()).is_some();
    let has_bm25 = row.score.filter(|s| s.is_finite()).is_some();
    let has_rrf = row.rrf_score.filter(|s| s.is_finite()).is_some();
    // When the row only carries rrf_score, inherit branch participation (hybrid path).
    // When it carries channel-specific keys, mark only those channels.
    if has_distance || has_bm25 {
        SearchSignals {
            vector: has_distance || (has_rrf && branch.vector),
            bm25: has_bm25 || (has_rrf && branch.bm25),
            graph: branch.graph,
            expanded: branch.expanded,
        }
    } else if has_rrf {
        SearchSignals {
            vector: branch.vector,
            bm25: branch.bm25,
            graph: branch.graph,
            expanded: branch.expanded,
        }
    } else {
        // No engine keys — still inherit branch for empty-score rows (labeled fallback).
        branch.clone()
    }
}

/// Map a search row to a hit with honest score kind labeling.
pub fn search_row_to_hit(
    row: SearchRow,
    branch_signals: SearchSignals,
    rank_fallback: f64,
) -> Result<SearchHit> {
    let id_s = record_id_to_string(&row.id)?;
    let signals = signals_from_search_row(&row, &branch_signals);
    // Honest mapping:
    // - Non-zero finite engine values (RRF / BM25 / distance-derived) → Engine
    // - Engine 0.0 (BM25 IDF floor when df ≥ N/2) or missing → RankFallback
    //   with an explicit rank-position score (never silently sold as Engine)
    let engine_score = row
        .rrf_score
        .filter(|s| s.is_finite() && *s != 0.0)
        .or_else(|| row.score.filter(|s| s.is_finite() && *s != 0.0))
        .or_else(|| {
            row.distance
                .filter(|d| d.is_finite())
                .map(|d| 1.0 / (1.0 + d.max(0.0)))
                .filter(|s| s.is_finite())
        });
    let (score, score_kind) = match engine_score {
        Some(s) => (s, ScoreKind::Engine),
        None => (rank_fallback, ScoreKind::RankFallback),
    };
    let category = row
        .category
        .as_deref()
        .and_then(Category::parse)
        .unwrap_or(Category::Semantic);
    let scope = row.scope.unwrap_or_default();
    let preview = row
        .content
        .as_ref()
        .map(|c| c.text.clone())
        .unwrap_or_default();
    let valid_from = match row.valid_from {
        Some(v) => parse_timestamp(&v)?,
        None => Timestamp::UNIX_EPOCH,
    };
    let valid_until = match row.valid_until {
        None | Some(Value::Null) => None,
        Some(v) => Some(parse_timestamp(&v)?),
    };
    Ok(SearchHit {
        id: MemoryId::new(id_s),
        score,
        score_kind,
        signals,
        preview,
        category,
        scope,
        valid_from,
        valid_until,
        provenance: row.provenance.unwrap_or_default(),
        version: row.version.unwrap_or(1),
        entities: vec![],
        embedding_generation: row.embedding_generation,
        expansion: None,
    })
}

#[cfg(test)]
mod score_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_rrf_score() {
        let v = json!({"id": "memory:1", "rrf_score": 0.032});
        assert!((extract_score(&v).unwrap() - 0.032).abs() < 1e-9);
    }

    #[test]
    fn extract_distance_as_similarity() {
        let v = json!({"distance": 0.0});
        assert!((extract_score(&v).unwrap() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn extract_bm25_score() {
        let v = json!({"score": 1.5});
        assert!((extract_score(&v).unwrap() - 1.5).abs() < 1e-9);
    }

    #[test]
    fn score_kind_engine_vs_fallback() {
        let engine_row = SearchRow {
            id: json!("memory:1"),
            rrf_score: Some(0.03),
            score: None,
            distance: None,
            category: Some("semantic".into()),
            scope: Some("org/x".into()),
            content: Some(ContentWire {
                text: "a".into(),
                attrs: None,
            }),
            valid_from: None,
            valid_until: None,
            provenance: None,
            version: Some(1),
            embedding_generation: None,
        };
        let hit = search_row_to_hit(
            engine_row,
            SearchSignals {
                vector: true,
                bm25: true,
                graph: false,
                expanded: false,
            },
            0.001,
        )
        .unwrap();
        assert_eq!(hit.score_kind, ScoreKind::Engine);
        assert!((hit.score - 0.03).abs() < 1e-9);

        let empty_row = SearchRow {
            id: json!("memory:2"),
            rrf_score: None,
            score: None,
            distance: None,
            category: Some("semantic".into()),
            scope: Some("org/x".into()),
            content: Some(ContentWire {
                text: "b".into(),
                attrs: None,
            }),
            valid_from: None,
            valid_until: None,
            provenance: None,
            version: Some(1),
            embedding_generation: None,
        };
        let hit2 = search_row_to_hit(
            empty_row,
            SearchSignals {
                vector: false,
                bm25: true,
                graph: false,
                expanded: false,
            },
            0.016,
        )
        .unwrap();
        assert_eq!(hit2.score_kind, ScoreKind::RankFallback);
        assert!((hit2.score - 0.016).abs() < 1e-9);
    }

    #[test]
    fn signals_from_distance_only_row() {
        let row = SearchRow {
            id: json!("memory:3"),
            rrf_score: None,
            score: None,
            distance: Some(0.1),
            category: None,
            scope: None,
            content: None,
            valid_from: None,
            valid_until: None,
            provenance: None,
            version: None,
            embedding_generation: None,
        };
        let s = signals_from_search_row(
            &row,
            &SearchSignals {
                vector: true,
                bm25: true,
                graph: false,
                expanded: false,
            },
        );
        assert!(s.vector);
        assert!(!s.bm25, "distance-only row must not force bm25");
    }
}
