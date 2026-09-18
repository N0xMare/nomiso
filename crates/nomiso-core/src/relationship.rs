//! Typed relationship and entity primitives (REL-001..004).
//!
//! Relationships are assertions: opaque id, optimistic version, owning scope,
//! typed subject/object endpoints, a registered predicate, evidence,
//! epistemic status, validity interval, producer metadata, and explicit state.
//! Predicates are a closed registry — unknown or ill-typed relations are
//! rejected before mutation. `supersedes` is foundation-maintained lineage and
//! is never creatable through the public relationship API.

#![allow(missing_docs)]

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::types::Timestamp;

/// Kinds of records a relationship endpoint can reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EndpointKind {
    Memory,
    Entity,
    Artifact,
    Span,
}

impl EndpointKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Entity => "entity",
            Self::Artifact => "artifact",
            Self::Span => "span",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "memory" => Some(Self::Memory),
            "entity" => Some(Self::Entity),
            "artifact" => Some(Self::Artifact),
            "span" => Some(Self::Span),
            _ => None,
        }
    }

    /// Whether endpoints of this kind carry an optimistic version.
    pub fn versioned(self) -> bool {
        matches!(self, Self::Memory | Self::Entity)
    }
}

/// A typed endpoint reference: kind plus the record's bare key or
/// `kind:key` form (the kind prefix is stripped on normalize).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct EndpointRef {
    pub kind: EndpointKind,
    pub id: String,
}

impl EndpointRef {
    pub fn new(kind: EndpointKind, id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
        }
    }

    /// Bare record key with an optional matching `kind:` prefix removed.
    /// A mismatched prefix is rejected.
    pub fn bare_key(&self) -> Result<String> {
        let id = self.id.trim();
        let prefixed = format!("{}:", self.kind.as_str());
        let bare = if let Some(rest) = id.strip_prefix(&prefixed) {
            rest
        } else if id.contains(':') {
            return Err(Error::InvalidOp(format!(
                "endpoint id prefix does not match kind {}",
                self.kind.as_str()
            )));
        } else {
            id
        };
        let bare = bare.trim_matches(|c| c == '`' || c == '⟨' || c == '⟩' || c == '"');
        if bare.is_empty() {
            return Err(Error::InvalidOp("endpoint id is empty".into()));
        }
        Ok(bare.to_string())
    }

    /// Canonical `kind:key` display form.
    pub fn canonical(&self) -> Result<String> {
        Ok(format!("{}:{}", self.kind.as_str(), self.bare_key()?))
    }
}

/// Registered relation types (closed set; schema registry version 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RelationPredicate {
    Supports,
    DerivedFrom,
    Contradicts,
    Supersedes,
    Mentions,
    DependsOn,
    AppliesTo,
    ObservedIn,
    Attempted,
    ResolvedBy,
}

/// Static registry entry describing one predicate's contract.
pub struct RelationSpec {
    pub predicate: RelationPredicate,
    pub subject_kinds: &'static [EndpointKind],
    pub object_kinds: &'static [EndpointKind],
    /// Symmetric predicates traverse in both directions regardless of edge direction.
    pub symmetric: bool,
    /// Foundation-maintained lineage: never creatable via the public API.
    pub foundation_only: bool,
}

use EndpointKind::*;

const MEM: &[EndpointKind] = &[Memory];
const ENT: &[EndpointKind] = &[Entity];
const SPAN: &[EndpointKind] = &[Span];
const SPAN_OR_MEM: &[EndpointKind] = &[Span, Memory];
const EVIDENCE: &[EndpointKind] = &[Span, Artifact, Memory];
const ENT_OR_MEM: &[EndpointKind] = &[Entity, Memory];
const DEP_OBJ: &[EndpointKind] = &[Entity, Artifact, Memory];
const ENT_OR_ART: &[EndpointKind] = &[Entity, Artifact];

/// Registry version stamped on created edges.
pub const RELATION_REGISTRY_VERSION: u32 = 1;

impl RelationPredicate {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Supports => "supports",
            Self::DerivedFrom => "derived_from",
            Self::Contradicts => "contradicts",
            Self::Supersedes => "supersedes",
            Self::Mentions => "mentions",
            Self::DependsOn => "depends_on",
            Self::AppliesTo => "applies_to",
            Self::ObservedIn => "observed_in",
            Self::Attempted => "attempted",
            Self::ResolvedBy => "resolved_by",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "supports" => Self::Supports,
            "derived_from" => Self::DerivedFrom,
            "contradicts" => Self::Contradicts,
            "supersedes" => Self::Supersedes,
            "mentions" => Self::Mentions,
            "depends_on" => Self::DependsOn,
            "applies_to" => Self::AppliesTo,
            "observed_in" => Self::ObservedIn,
            "attempted" => Self::Attempted,
            "resolved_by" => Self::ResolvedBy,
            _ => return None,
        })
    }

    /// The registered contract for this predicate.
    pub fn spec(self) -> RelationSpec {
        match self {
            Self::Supports => RelationSpec {
                predicate: self,
                subject_kinds: SPAN_OR_MEM,
                object_kinds: MEM,
                symmetric: false,
                foundation_only: false,
            },
            Self::DerivedFrom => RelationSpec {
                predicate: self,
                subject_kinds: MEM,
                object_kinds: EVIDENCE,
                symmetric: false,
                foundation_only: false,
            },
            Self::Contradicts => RelationSpec {
                predicate: self,
                subject_kinds: MEM,
                object_kinds: MEM,
                symmetric: true,
                foundation_only: false,
            },
            Self::Supersedes => RelationSpec {
                predicate: self,
                subject_kinds: MEM,
                object_kinds: MEM,
                symmetric: false,
                foundation_only: true,
            },
            Self::Mentions => RelationSpec {
                predicate: self,
                subject_kinds: MEM,
                object_kinds: ENT,
                symmetric: false,
                foundation_only: false,
            },
            Self::DependsOn => RelationSpec {
                predicate: self,
                subject_kinds: ENT_OR_MEM,
                object_kinds: DEP_OBJ,
                symmetric: false,
                foundation_only: false,
            },
            Self::AppliesTo => RelationSpec {
                predicate: self,
                subject_kinds: MEM,
                object_kinds: ENT_OR_ART,
                symmetric: false,
                foundation_only: false,
            },
            Self::ObservedIn => RelationSpec {
                predicate: self,
                subject_kinds: MEM,
                object_kinds: SPAN,
                symmetric: false,
                foundation_only: false,
            },
            Self::Attempted => RelationSpec {
                predicate: self,
                subject_kinds: MEM,
                object_kinds: MEM,
                symmetric: false,
                foundation_only: false,
            },
            Self::ResolvedBy => RelationSpec {
                predicate: self,
                subject_kinds: MEM,
                object_kinds: MEM,
                symmetric: false,
                foundation_only: false,
            },
        }
    }

    /// Validate a user-supplied edge before mutation (REL-001).
    /// Foundation-maintained predicates are rejected for public creation.
    pub fn check_endpoints(self, subject: EndpointKind, object: EndpointKind) -> Result<()> {
        let spec = self.spec();
        if spec.foundation_only {
            return Err(Error::InvalidOp(format!(
                "predicate '{}' is foundation-maintained lineage",
                self.as_str()
            )));
        }
        if !spec.subject_kinds.contains(&subject) {
            return Err(Error::InvalidOp(format!(
                "predicate '{}' does not accept subject kind {}",
                self.as_str(),
                subject.as_str()
            )));
        }
        if !spec.object_kinds.contains(&object) {
            return Err(Error::InvalidOp(format!(
                "predicate '{}' does not accept object kind {}",
                self.as_str(),
                object.as_str()
            )));
        }
        Ok(())
    }
}

/// Epistemic status of the asserted relation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EpistemicStatus {
    Reported,
    Inferred,
    Verified,
    Disputed,
    #[default]
    Unknown,
}

impl EpistemicStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reported => "reported",
            Self::Inferred => "inferred",
            Self::Verified => "verified",
            Self::Disputed => "disputed",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "reported" => Self::Reported,
            "inferred" => Self::Inferred,
            "verified" => Self::Verified,
            "disputed" => Self::Disputed,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }
}

/// Lifecycle state of a relationship.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RelationshipState {
    #[default]
    Active,
    Closed,
    Stale,
    Purged,
}

impl RelationshipState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Closed => "closed",
            Self::Stale => "stale",
            Self::Purged => "purged",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "active" => Self::Active,
            "closed" => Self::Closed,
            "stale" => Self::Stale,
            "purged" => Self::Purged,
            _ => return None,
        })
    }
}

/// An evidence reference pinned to an optional endpoint revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EvidenceRef {
    pub kind: EndpointKind,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
}

/// Producer metadata: who/what asserted this edge.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProducerMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

// --- Entities ---

/// Create a scoped entity/resource record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PutEntityRequest {
    pub scope: String,
    pub kind: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attrs: Option<serde_json::Value>,
}

/// Stored entity record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EntityRecord {
    pub id: String,
    pub scope: String,
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attrs: Option<serde_json::Value>,
    pub version: u64,
    #[schemars(with = "String")]
    pub created_at: Timestamp,
    #[schemars(with = "String")]
    pub updated_at: Timestamp,
}

/// Versioned entity update (CAS on expected_version).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct UpdateEntityRequest {
    pub id: String,
    pub scope: String,
    pub expected_version: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aliases: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attrs: Option<serde_json::Value>,
}

// --- Relationships ---

/// Create a typed relationship.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PutRelationshipRequest {
    pub scope: String,
    pub predicate: RelationPredicate,
    pub subject: EndpointRef,
    pub object: EndpointRef,
    /// Pin endpoints to exact revisions (versioned kinds only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_rev: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_rev: Option<u64>,
    #[serde(default)]
    pub epistemic: EpistemicStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvidenceRef>,
    /// Half-open world interval; `valid_from` defaults to now when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub valid_from: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub valid_until: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producer: Option<ProducerMeta>,
    /// When true (default), an identical active edge returns the existing
    /// record with `replayed = true` instead of a duplicate.
    #[serde(default = "default_dedupe")]
    pub dedupe: bool,
}

fn default_dedupe() -> bool {
    true
}

/// Stored relationship record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RelationshipRecord {
    pub id: String,
    pub scope: String,
    pub predicate: RelationPredicate,
    pub predicate_version: u32,
    pub subject: EndpointRef,
    pub object: EndpointRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_rev: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_rev: Option<u64>,
    pub epistemic: EpistemicStatus,
    #[serde(default)]
    pub evidence: Vec<EvidenceRef>,
    pub state: RelationshipState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_reason: Option<String>,
    #[schemars(with = "String")]
    pub valid_from: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub valid_until: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producer: Option<ProducerMeta>,
    pub version: u64,
    #[schemars(with = "String")]
    pub created_at: Timestamp,
    #[schemars(with = "String")]
    pub updated_at: Timestamp,
}

/// Result of `put_relationship`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RelationshipWrite {
    pub record: RelationshipRecord,
    /// True when an identical active edge already existed and was returned.
    pub replayed: bool,
}

/// Versioned relationship update (CAS on expected_version). State may
/// transition to `closed` or `stale` with a required reason; `purged` is
/// endpoint-erasure-driven only and is rejected here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct UpdateRelationshipRequest {
    pub id: String,
    pub scope: String,
    pub expected_version: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epistemic: Option<EpistemicStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<RelationshipState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub valid_until: Option<Timestamp>,
    /// Replace the evidence set (prior set is preserved in the audit event).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Vec<EvidenceRef>>,
}

/// Query relationships under a scope.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ListRelationshipsRequest {
    pub scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<EndpointRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicate: Option<RelationPredicate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<RelationshipState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

// --- Bounded traversal ---

/// Direction edges are followed from a visited node.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TraverseDirection {
    /// Follow edges where the node is the subject (symmetric edges count both ways).
    #[default]
    Out,
    /// Follow edges where the node is the object (symmetric edges count both ways).
    In,
    Both,
}

/// Hard budgets for traversal (REL-003). `None` fields get defaults;
/// values above caps are clamped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TraverseBudget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_visited: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_edges: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<u64>,
}

impl Default for TraverseBudget {
    fn default() -> Self {
        Self {
            max_depth: Some(DEFAULT_MAX_DEPTH),
            max_visited: Some(DEFAULT_MAX_VISITED),
            max_edges: Some(DEFAULT_MAX_EDGES),
            deadline_ms: Some(DEFAULT_DEADLINE_MS),
        }
    }
}

pub const DEFAULT_MAX_DEPTH: u32 = 2;
pub const DEFAULT_MAX_VISITED: u32 = 128;
pub const DEFAULT_MAX_EDGES: u32 = 256;
pub const DEFAULT_DEADLINE_MS: u64 = 2_000;
pub const CAP_MAX_DEPTH: u32 = 8;
pub const CAP_MAX_VISITED: u32 = 4_096;
pub const CAP_MAX_EDGES: u32 = 8_192;
pub const CAP_DEADLINE_MS: u64 = 30_000;

/// Bounded same-scope graph traversal request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TraverseRequest {
    pub scope: String,
    /// Seed endpoints (must exist and share the scope).
    pub seeds: Vec<EndpointRef>,
    /// Edge types allowed; empty/None means all non-foundation predicates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicates: Option<Vec<RelationPredicate>>,
    #[serde(default)]
    pub direction: TraverseDirection,
    /// Only `active` edges are traversed unless explicit states are given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub states: Option<Vec<RelationshipState>>,
    #[serde(default)]
    pub budget: TraverseBudget,
}

/// A reached endpoint with its discovery path (relationship ids, root-first).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TraverseNode {
    pub endpoint: EndpointRef,
    pub depth: u32,
    pub path: Vec<String>,
}

/// Traversal outcome with completeness/cost reporting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TraverseResult {
    /// Reached endpoints excluding seeds, deduplicated (shortest path wins).
    pub nodes: Vec<TraverseNode>,
    /// Unique edges encountered during traversal.
    pub edges: Vec<RelationshipRecord>,
    /// Budgets that engaged, e.g. `depth`, `visited`, `edges`, `deadline`.
    /// Empty means the explored frontier was exhausted within budget.
    pub truncated: Vec<String>,
    pub depth_reached: u32,
    pub visited: u32,
    pub edges_scanned: u32,
    pub elapsed_ms: u64,
}
