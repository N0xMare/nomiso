//! Coding-agent scenario pack → plane [`EvalSuite`].
//!
//! Source of truth is `evals/coding_agent/scenarios/*.json`.
//! Gold ops / transcripts ride along for the skill track; plane ingest uses
//! `gold_memories` + `probes` only.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{EvalSuite, FixtureMemory, FixtureProbe, ProbeTag};

/// Compiled suite name.
pub const PACK_NAME: &str = "sota_coding_v2";

/// Locked pattern ids (P1–P20).
pub const ALLOWED_PATTERNS: &[&str] = &[
    "p01_toolchain",
    "p02_supersede",
    "p03_arch_lock",
    "p04_error_lesson",
    "p05_module_locality",
    "p06_abi_contract",
    "p07_scope_isolation",
    "p08_valid_time",
    "p09_known_time",
    "p10_uncertainty",
    "p11_abstain",
    "p12_compaction",
    "p13_working_state",
    "p14_failed_approach",
    "p15_procedural",
    "p16_cross_session",
    "p17_near_dup",
    "p18_citation",
    "p19_multi_hop",
    "p20_identity",
];

/// Required exemplar coverage (P19 is the formal skip).
pub const REQUIRED_EXEMPLAR_PATTERNS: &[&str] = &[
    "p01_toolchain",
    "p02_supersede",
    "p03_arch_lock",
    "p04_error_lesson",
    "p05_module_locality",
    "p06_abi_contract",
    "p07_scope_isolation",
    "p08_valid_time",
    "p09_known_time",
    "p10_uncertainty",
    "p11_abstain",
    "p12_compaction",
    "p13_working_state",
    "p14_failed_approach",
    "p15_procedural",
    "p16_cross_session",
    "p17_near_dup",
    "p18_citation",
    "p19_multi_hop",
    "p20_identity",
];

/// One turn of a coding-agent transcript (skill track / generation context).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptTurn {
    pub role: String,
    pub text: String,
}

/// One locked-or-generated coding-agent scenario.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodingScenario {
    pub id: String,
    pub pattern: String,
    pub scope: String,
    #[serde(default)]
    pub transcript: Vec<TranscriptTurn>,
    /// WriterOp-shaped JSON. `supersede.prior_id` is a gold **key**, not a UUID.
    #[serde(default)]
    pub gold_ops: Vec<serde_json::Value>,
    pub gold_memories: Vec<FixtureMemory>,
    pub probes: Vec<FixtureProbe>,
    #[serde(default)]
    pub notes: Option<String>,
    /// `exemplar` (hand-checked) or `generated` (LLM fill under the schema).
    #[serde(default)]
    pub origin: Option<String>,
}

static BUNDLED_SCENARIOS: include_dir::Dir<'_> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/fixtures/coding_agent");

#[doc = "Load the frozen coding scenarios embedded in the package, sorted by filename."]
pub fn bundled_coding_scenarios() -> nomiso_core::Result<Vec<CodingScenario>> {
    let mut files: Vec<_> = BUNDLED_SCENARIOS
        .files()
        .filter(|f| f.path().extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();
    files.sort_by_key(|f| f.path());
    files
        .into_iter()
        .map(|f| {
            serde_json::from_slice::<CodingScenario>(f.contents()).map_err(|e| {
                nomiso_core::Error::invalid(format!("bundled scenario {}: {e}", f.path().display()))
            })
        })
        .collect()
}

/// Checkout-only scenario directory; portable callers use bundled_coding_scenarios.
pub fn coding_agent_pack_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../evals/coding_agent/scenarios")
}

/// Load every `*.json` in the pack directory (sorted by filename).
pub fn load_coding_scenarios(dir: &Path) -> nomiso_core::error::Result<Vec<CodingScenario>> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| nomiso_core::error::Error::invalid(format!("read {}: {e}", dir.display())))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
        .collect();
    paths.sort();
    if paths.is_empty() {
        return Err(nomiso_core::error::Error::invalid(format!(
            "no scenario JSON in {}",
            dir.display()
        )));
    }
    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        let raw = std::fs::read_to_string(&path).map_err(|e| {
            nomiso_core::error::Error::invalid(format!("read {}: {e}", path.display()))
        })?;
        let s: CodingScenario = serde_json::from_str(&raw).map_err(|e| {
            nomiso_core::error::Error::invalid(format!("parse {}: {e}", path.display()))
        })?;
        out.push(s);
    }
    Ok(out)
}

/// Compile the bundled pack into a plane suite.
pub fn coding_agent_suite() -> nomiso_core::error::Result<EvalSuite> {
    let scenarios = bundled_coding_scenarios()?;
    validate_coding_pack(&scenarios)?;
    Ok(compile_coding_pack(&scenarios))
}

/// Flatten scenarios. Keys stay as authored (must be pack-unique).
pub fn compile_coding_pack(scenarios: &[CodingScenario]) -> EvalSuite {
    let mut memories = Vec::new();
    let mut probes = Vec::new();
    for s in scenarios {
        memories.extend(s.gold_memories.iter().cloned());
        probes.extend(s.probes.iter().cloned());
    }
    EvalSuite {
        name: PACK_NAME.into(),
        embedding_dim: 32,
        memories,
        probes,
    }
}

/// Fail closed on schema / gold-key / op hygiene.
pub fn validate_coding_pack(scenarios: &[CodingScenario]) -> nomiso_core::error::Result<()> {
    let allowed: HashSet<&str> = ALLOWED_PATTERNS.iter().copied().collect();
    let mut ids = HashSet::new();
    let mut probe_names = HashSet::new();
    let mut keys: HashMap<String, String> = HashMap::new();
    let mut keys_by_scenario: HashMap<String, HashSet<String>> = HashMap::new();
    let mut exemplar_patterns = BTreeSet::new();

    for s in scenarios {
        if !allowed.contains(s.pattern.as_str()) {
            return Err(nomiso_core::error::Error::invalid(format!(
                "scenario {}: unknown pattern {}",
                s.id, s.pattern
            )));
        }
        if !ids.insert(s.id.clone()) {
            return Err(nomiso_core::error::Error::invalid(format!(
                "duplicate scenario id {}",
                s.id
            )));
        }
        if s.scope.trim().is_empty() {
            return Err(nomiso_core::error::Error::invalid(format!(
                "scenario {}: empty scope",
                s.id
            )));
        }
        if s.probes.is_empty() {
            return Err(nomiso_core::error::Error::invalid(format!(
                "scenario {}: no probes",
                s.id
            )));
        }
        if s.origin.as_deref() == Some("exemplar") {
            exemplar_patterns.insert(s.pattern.clone());
        }
        let local = keys_by_scenario.entry(s.id.clone()).or_default();
        for m in &s.gold_memories {
            if m.key.trim().is_empty() || m.text.trim().is_empty() {
                return Err(nomiso_core::error::Error::invalid(format!(
                    "scenario {}: empty gold memory key/text",
                    s.id
                )));
            }
            if keys.insert(m.key.clone(), s.id.clone()).is_some() {
                return Err(nomiso_core::error::Error::invalid(format!(
                    "duplicate gold key {} (scenario {})",
                    m.key, s.id
                )));
            }
            local.insert(m.key.clone());
            parse_opt_rfc3339(s, "gold memory valid_from", m.valid_from.as_deref())?;
            parse_opt_rfc3339(s, "gold memory valid_until", m.valid_until.as_deref())?;
            parse_opt_rfc3339(s, "gold memory known_at", m.known_at.as_deref())?;
            if let Some(cat) = m.category.as_deref() {
                if nomiso_core::types::Category::parse(cat).is_none() {
                    return Err(nomiso_core::error::Error::invalid(format!(
                        "scenario {} memory {} bad category {cat}",
                        s.id, m.key
                    )));
                }
            }
        }
        for p in &s.probes {
            if !probe_names.insert(p.name.clone()) {
                return Err(nomiso_core::error::Error::invalid(format!(
                    "duplicate probe name {}",
                    p.name
                )));
            }
            if p.name.trim().is_empty() {
                return Err(nomiso_core::error::Error::invalid(format!(
                    "scenario {}: empty probe name",
                    s.id
                )));
            }
            if p.pattern_tag_mismatch() {
                return Err(nomiso_core::error::Error::invalid(format!(
                    "probe {} tag/skip mismatch",
                    p.name
                )));
            }
            parse_opt_rfc3339(s, &format!("probe {} as_of", p.name), p.as_of.as_deref())?;
            parse_opt_rfc3339(
                s,
                &format!("probe {} known_as_of", p.name),
                p.known_as_of.as_deref(),
            )?;
            parse_opt_rfc3339(
                s,
                &format!("probe {} sys_as_of", p.name),
                p.sys_as_of.as_deref(),
            )?;
            if let Some(cat) = p.category.as_deref() {
                if nomiso_core::types::Category::parse(cat).is_none() {
                    return Err(nomiso_core::error::Error::invalid(format!(
                        "probe {} bad category {cat}",
                        p.name
                    )));
                }
            }
        }
        validate_gold_ops(s, local)?;
        if s.pattern == "p19_multi_hop" {
            let ok = s
                .probes
                .iter()
                .any(|p| p.skip_if_no_graph || p.tag == Some(ProbeTag::MultiHop));
            if !ok {
                return Err(nomiso_core::error::Error::invalid(
                    "p19_multi_hop must skip_if_no_graph or tag multi_hop",
                ));
            }
        }
    }

    for req in REQUIRED_EXEMPLAR_PATTERNS {
        if !exemplar_patterns.contains(*req) {
            return Err(nomiso_core::error::Error::invalid(format!(
                "missing hand-checked exemplar for {req}"
            )));
        }
    }

    for s in scenarios {
        for p in &s.probes {
            for k in &p.expect_keys_in_top {
                if !keys.contains_key(k) {
                    return Err(nomiso_core::error::Error::invalid(format!(
                        "probe {} expect_key {k} not in pack",
                        p.name
                    )));
                }
            }
            for k in &p.must_not_include_keys {
                if !keys.contains_key(k) {
                    return Err(nomiso_core::error::Error::invalid(format!(
                        "probe {} must_not key {k} not in pack",
                        p.name
                    )));
                }
            }
            if p.expect_abstain && !p.expect_keys_in_top.is_empty() {
                return Err(nomiso_core::error::Error::invalid(format!(
                    "probe {} abstain must not expect keys",
                    p.name
                )));
            }
        }
    }
    Ok(())
}

fn parse_opt_rfc3339(
    s: &CodingScenario,
    field: &str,
    raw: Option<&str>,
) -> nomiso_core::error::Result<()> {
    if let Some(raw) = raw {
        raw.parse::<nomiso_core::types::Timestamp>()
            .map(|_| ())
            .map_err(|e| {
                nomiso_core::error::Error::invalid(format!(
                    "scenario {} {field} '{raw}': {e}",
                    s.id
                ))
            })?;
    }
    Ok(())
}

fn validate_gold_ops(
    s: &CodingScenario,
    local_keys: &HashSet<String>,
) -> nomiso_core::error::Result<()> {
    for (i, op) in s.gold_ops.iter().enumerate() {
        let kind = op.get("op").and_then(|v| v.as_str()).unwrap_or("");
        match kind {
            "put" => {
                if op.get("prior_id").is_some() {
                    return Err(nomiso_core::error::Error::invalid(format!(
                        "scenario {} gold_ops[{i}] put must not invent prior_id",
                        s.id
                    )));
                }
            }
            "supersede" => {
                let prior = op.get("prior_id").and_then(|v| v.as_str()).unwrap_or("");
                if prior.is_empty() || !local_keys.contains(prior) {
                    return Err(nomiso_core::error::Error::invalid(format!(
                        "scenario {} gold_ops[{i}] supersede prior_id must be a gold key",
                        s.id
                    )));
                }
            }
            "noop" | "forget" => {}
            "" => {
                return Err(nomiso_core::error::Error::invalid(format!(
                    "scenario {} gold_ops[{i}] missing op",
                    s.id
                )));
            }
            other => {
                return Err(nomiso_core::error::Error::invalid(format!(
                    "scenario {} gold_ops[{i}] unknown op {other}",
                    s.id
                )));
            }
        }
    }
    Ok(())
}

impl FixtureProbe {
    fn pattern_tag_mismatch(&self) -> bool {
        // v2: multi_hop is an explicit skip until a graph writer exists.
        matches!(self.tag, Some(ProbeTag::MultiHop)) && !self.skip_if_no_graph
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_bytes_match_checkout_when_available() {
        let source = coding_agent_pack_dir();
        if !source.is_dir() {
            eprintln!(
                "checkout parity unavailable in packaged source; bundled validation remains active"
            );
            return;
        }
        let mut names: Vec<_> = std::fs::read_dir(&source)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().and_then(|s| s.to_str()) == Some("json"))
            .collect();
        names.sort();
        assert_eq!(names.len(), BUNDLED_SCENARIOS.files().count());
        for path in names {
            let bundled = BUNDLED_SCENARIOS
                .get_file(path.file_name().unwrap())
                .expect("bundled file");
            assert_eq!(std::fs::read(&path).unwrap(), bundled.contents());
        }
    }

    #[test]
    fn coding_pack_validates_and_compiles() {
        let scenarios = bundled_coding_scenarios().expect("load");
        validate_coding_pack(&scenarios).expect("validate");
        let suite = compile_coding_pack(&scenarios);
        assert_eq!(suite.name, PACK_NAME);
        assert!(!suite.memories.is_empty());
        assert!(!suite.probes.is_empty());
        assert!(suite
            .probes
            .iter()
            .any(|p| p.tag == Some(ProbeTag::Temporal) && p.as_of.is_some()));
        assert!(suite
            .probes
            .iter()
            .any(|p| p.tag == Some(ProbeTag::Temporal) && p.known_as_of.is_some()));
        assert!(suite
            .probes
            .iter()
            .any(|p| p.tag == Some(ProbeTag::Uncertainty)));
        assert!(suite
            .probes
            .iter()
            .any(|p| p.tag == Some(ProbeTag::MultiHop) && p.skip_if_no_graph));
    }
}
