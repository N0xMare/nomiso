//! Sleep v1 — deterministic consolidation (dry-run default; optional apply).
//!
//! `sleep_pass` is **proposal-only**. Products apply approved proposals via
//! [`crate::writer::apply_ops`] (lock 12b: never `client.forget` directly).

use std::collections::HashSet;

use nomiso_core::{Category, ListRequest, MemoryId, ScopeMatch};
use nomiso_service::NomisoClient;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::writer::{ApplyOpOutcome, WriterOp};

/// Jaccard floor for near-duplicate proposals (token **sets**, stopword-filtered).
const NEAR_DUP_JACCARD: f64 = 0.6;

const STOPWORDS: &[&str] = &[
    "the", "and", "for", "that", "with", "this", "from", "have", "was", "are", "but", "not", "you",
    "all", "can", "her", "his", "she", "they", "them", "were", "been", "has", "had", "its", "it's",
    "into", "than", "then", "also", "just", "only", "over", "such", "when", "what", "which", "who",
    "will", "would", "could", "should", "about", "there", "their", "your", "more", "some", "very",
];

/// One proposed consolidation action.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum SleepProposal {
    /// Candidate near-duplicate pair; review only, apply never soft-forgets it.
    NearDuplicate {
        keep_id: String,
        forget_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        forget_version: Option<u64>,
        reason: String,
        /// True when the pair shows mechanical contradiction signals
        /// (negation asymmetry or differing numeric values) — review as a
        /// possible conflict, not a merge candidate.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        possible_conflict: bool,
    },
    /// Episodic item listed for host review (proposal only unless apply).
    AgeOutEpisodic {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<u64>,
        preview: String,
        reason: String,
    },
}

/// Applied op result (when not dry-run).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SleepApplied {
    pub op: String,
    pub target_id: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Report from a sleep/consolidate pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SleepReport {
    pub scope: String,
    /// True only when apply ran and at least one op succeeded.
    pub consolidated: bool,
    pub dry_run: bool,
    pub message: String,
    pub proposals: Vec<SleepProposal>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applied: Vec<SleepApplied>,
    pub scanned: usize,
}

impl SleepReport {
    /// True when every attempted apply op succeeded (dry runs apply nothing).
    pub fn is_ok(&self) -> bool {
        self.applied.iter().all(|a| a.ok)
    }
}

/// Options for sleep v1.
#[derive(Debug, Clone)]
pub struct SleepOptions {
    /// When true (default), only propose. When false, soft-forget only guarded
    /// episodic age-outs; near-dup proposals stay review-only.
    pub dry_run: bool,
    pub max_scan: u32,
    /// Unused. Near-dup uses stopword-filtered Jaccard ≥ 0.6 on token **sets**, not this count.
    /// Kept so existing CLI/MCP `SleepOptions` construction does not break.
    pub near_dup_shared_tokens: usize,
    /// When applying, also soft-forget AgeOutEpisodic proposals.
    /// Apply is allowed only if this is true **and** `older_than_secs` is `Some`.
    pub apply_age_out: bool,
    /// Age threshold (seconds) for episodic age-out.
    /// `None` (default): still list AgeOutEpisodic for review; never apply them.
    pub older_than_secs: Option<i64>,
}

impl Default for SleepOptions {
    fn default() -> Self {
        Self {
            dry_run: true,
            max_scan: 64,
            near_dup_shared_tokens: 4,
            apply_age_out: false,
            older_than_secs: None,
        }
    }
}

/// Deterministic sleep: list scope, propose near-dups and episodic age-outs.
///
/// Always proposal-only (`dry_run` on the report is true). Apply via
/// via [`crate::writer::apply_ops`].
pub async fn sleep_pass(
    client: &NomisoClient,
    scope: &str,
    opts: SleepOptions,
) -> Result<SleepReport> {
    if opts.older_than_secs.is_some_and(|s| s < 0) {
        return Err(crate::Error::invalid(
            "older_than_secs must be non-negative",
        ));
    }
    // Page through list so max_scan is not silently clamped to store page size.
    let mut items = Vec::new();
    let mut cursor = None;
    let page_size = opts.max_scan.clamp(1, 32);
    while items.len() < opts.max_scan as usize {
        let page = client
            .list(ListRequest {
                scope: scope.into(),
                scope_match: ScopeMatch::Exact,
                categories: None,
                as_of: None,
                known_as_of: None,
                sys_as_of: None,
                text: None,
                limit: Some(page_size),
                cursor,
            })
            .await?;
        let n = page.items.len();
        items.extend(page.items);
        if items.len() >= opts.max_scan as usize {
            items.truncate(opts.max_scan as usize);
            break;
        }
        match page.next_cursor {
            Some(c) if n > 0 => cursor = Some(c),
            _ => break,
        }
    }
    let scanned = items.len();
    let mut proposals = Vec::new();
    let mut seen_forget = HashSet::new();
    let mut keepers = HashSet::new();

    // Near-dup: stopword-filtered Jaccard on token sets; same category; keeper survives.
    for i in 0..items.len() {
        for j in (i + 1)..items.len() {
            let a = &items[i].record;
            let b = &items[j].record;
            if a.category != b.category {
                continue;
            }
            // Conflict register is host-owned; never auto-forget uncertainty rows.
            if a.category == Category::Uncertainty {
                continue;
            }
            let ta = tokenize(&a.content.text);
            let tb = tokenize(&b.content.text);
            let jac = jaccard(&ta, &tb);
            if jac < NEAR_DUP_JACCARD {
                continue;
            }
            let (keep, forget) = if a.content.text.len() >= b.content.text.len() {
                (a, b)
            } else {
                (b, a)
            };
            let kid = keep.id.to_string();
            let fid = forget.id.to_string();
            if kid == fid {
                continue;
            }
            // Never propose forgetting a keep_id (this pair or a prior keeper).
            if keepers.contains(&fid) || seen_forget.contains(&kid) {
                continue;
            }
            if !seen_forget.insert(fid.clone()) {
                continue;
            }
            keepers.insert(kid.clone());
            let possible_conflict = looks_contradictory(&a.content.text, &b.content.text);
            let reason = if possible_conflict {
                format!(
                    "jaccard={jac:.2}; possible contradiction (negation/value mismatch) — \
                     review as conflict, never merge"
                )
            } else {
                format!("jaccard={jac:.2}; review only; not automatically applied")
            };
            proposals.push(SleepProposal::NearDuplicate {
                keep_id: kid,
                forget_id: fid,
                forget_version: Some(forget.version),
                reason,
                possible_conflict,
            });
        }
    }

    let now_secs = nomiso_core::Timestamp::now().as_second();
    for item in &items {
        if item.record.category != Category::Episodic {
            continue;
        }
        let id = item.record.id.to_string();
        let (include, reason) = match opts.older_than_secs {
            Some(secs) => {
                let age = now_secs.saturating_sub(item.record.valid_from.as_second());
                if age >= secs {
                    (true, format!("episodic older than {secs}s"))
                } else {
                    (false, String::new())
                }
            }
            None => (
                true,
                "episodic listed for host review (age-out apply requires older_than)".into(),
            ),
        };
        if include {
            proposals.push(SleepProposal::AgeOutEpisodic {
                id,
                version: Some(item.record.version),
                preview: item.record.content.text.chars().take(80).collect(),
                reason,
            });
        }
    }

    proposals.truncate(32);

    Ok(SleepReport {
        scope: scope.into(),
        consolidated: false,
        dry_run: true,
        message: format!(
            "sleep v1 dry-run: scanned {scanned} rows, {} proposals (no writes)",
            proposals.len()
        ),
        proposals,
        applied: vec![],
        scanned,
    })
}

/// Build soft-forget writer ops from proposals (lock 12b).
///
/// NearDuplicate proposals are review-only and produce no ops. Age-out ops are
/// included only when `apply_age_out` is true (caller must also require
/// `older_than_secs.is_some()`).
pub fn forget_ops_from_proposals(
    scope: &str,
    proposals: &[SleepProposal],
    apply_age_out: bool,
) -> Vec<(WriterOp, &'static str)> {
    let mut out = Vec::new();
    for p in proposals {
        match p {
            SleepProposal::NearDuplicate { .. } => {}
            SleepProposal::AgeOutEpisodic { id, version, .. } if apply_age_out => out.push((
                WriterOp::Forget {
                    id: MemoryId::new(id.clone()),
                    scope: scope.into(),
                    expected_version: *version,
                    hard: false,
                },
                "age_out_soft_forget",
            )),
            SleepProposal::AgeOutEpisodic { .. } => {}
        }
    }
    out
}

pub fn applied_from_outcomes(
    planned: &[(WriterOp, &'static str)],
    outcomes: &[ApplyOpOutcome],
) -> Vec<SleepApplied> {
    outcomes
        .iter()
        .map(|o| {
            let (index, ok, error) = match o {
                ApplyOpOutcome::Ok { index, .. } => (*index, true, None),
                ApplyOpOutcome::Err { index, error, .. } => (*index, false, Some(error.clone())),
            };
            let (target_id, op_name) = planned
                .get(index)
                .map(|(op, name)| {
                    let tid = match op {
                        WriterOp::Forget { id, .. } => id.to_string(),
                        _ => String::new(),
                    };
                    (tid, (*name).to_string())
                })
                .unwrap_or_else(|| (String::new(), "forget".into()));
            SleepApplied {
                op: op_name,
                target_id,
                ok,
                error,
            }
        })
        .collect()
}

/// Mechanical contradiction signals between a high-similarity pair.
///
/// Catches the two shapes a token heuristic can honestly detect:
/// polarity asymmetry (exactly one side negates) and differing numeric
/// values in otherwise-shared phrasing ("timeout is 30s" vs "…60s").
/// Semantic slot changes without such markers ("prefers Rust" vs
/// "prefers Go") are out of scope for a deterministic check — callers
/// must still review every near-dup proposal.
fn looks_contradictory(a: &str, b: &str) -> bool {
    if has_negation(a) != has_negation(b) {
        return true;
    }
    let va = value_tokens(a);
    let vb = value_tokens(b);
    !va.is_empty() && !vb.is_empty() && va != vb
}

/// Alphanumeric tokens containing a digit ("30", "v2", "8080") — the
/// value-carrying tokens `tokenize` drops via its length/stopword filters.
fn value_tokens(s: &str) -> HashSet<String> {
    s.to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| t.chars().any(|c| c.is_ascii_digit()))
        .map(|t| t.to_string())
        .collect()
}

fn has_negation(text: &str) -> bool {
    let padded = format!(" {} ", text.to_ascii_lowercase());
    [
        " not ",
        "n't",
        " never ",
        " no longer ",
        " cannot ",
        " can't ",
        " won't ",
        " isn't ",
        " aren't ",
        " don't ",
        " doesn't ",
        " didn't ",
    ]
    .iter()
    .any(|cue| padded.contains(cue))
}

fn tokenize(s: &str) -> HashSet<String> {
    s.to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| t.len() > 2)
        .filter(|t| !STOPWORDS.contains(t))
        .map(|t| t.to_string())
        .collect()
}

fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count();
    let union = a.union(b).count();
    if union == 0 {
        0.0
    } else {
        inter as f64 / union as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jaccard_paraphrase_above_floor() {
        let a = tokenize("Alice prefers Rust for systems programming work");
        let b = tokenize("Alice prefers Rust for systems programming tasks");
        assert!(
            jaccard(&a, &b) >= NEAR_DUP_JACCARD,
            "jaccard={}",
            jaccard(&a, &b)
        );
    }

    #[test]
    fn jaccard_unrelated_below_floor() {
        let a = tokenize("The weather report said rain is expected downtown this evening");
        let b = tokenize("Please remember to buy milk and bread at the grocery store");
        assert!(
            jaccard(&a, &b) < NEAR_DUP_JACCARD,
            "jaccard={}",
            jaccard(&a, &b)
        );
    }

    #[test]
    fn tokenize_drops_stopwords_and_short() {
        let t = tokenize("the and for that with this from have was are");
        assert!(t.is_empty(), "{t:?}");
    }

    #[test]
    fn contradictory_flags_numeric_and_negation_pairs() {
        assert!(looks_contradictory(
            "service timeout is 30 seconds",
            "service timeout is 60 seconds"
        ));
        assert!(looks_contradictory(
            "caching is enabled for the dashboard",
            "caching is not enabled for the dashboard"
        ));
        // Same-valued additions and paraphrases are not conflicts.
        assert!(!looks_contradictory(
            "Alice prefers Rust for systems programming work",
            "Alice prefers Rust for systems programming tasks"
        ));
        assert!(!looks_contradictory(
            "deploy took 30 minutes",
            "deploy took 30 minutes yesterday"
        ));
    }

    #[tokio::test]
    async fn sleep_marks_contradictory_near_dup_pairs() {
        let client =
            nomiso_service::NomisoClient::connect(nomiso_store::StoreConfig::memory_test(8))
                .await
                .unwrap();
        for text in [
            "service timeout is 30 seconds",
            "service timeout is 60 seconds",
        ] {
            client
                .put(nomiso_core::PutRequest {
                    scope: "org/sleep".into(),
                    category: nomiso_core::Category::Semantic,
                    content: nomiso_core::Content::text(text),
                    ..Default::default()
                })
                .await
                .unwrap();
        }
        let report = sleep_pass(&client, "org/sleep", SleepOptions::default())
            .await
            .unwrap();
        let pair = report
            .proposals
            .iter()
            .find_map(|p| match p {
                SleepProposal::NearDuplicate {
                    possible_conflict, ..
                } => Some(*possible_conflict),
                _ => None,
            })
            .expect("near-dup pair proposed");
        assert!(
            pair,
            "conflicting values must be flagged, not proposed as a merge"
        );
    }
}
