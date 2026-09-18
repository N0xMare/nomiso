//! System track for the coding-agent pack.
//!
//! Gold ingest via Vegapunk CLI, then a host (Grok CLI) must `hard-recall --pack`
//! before answering probes. Never blended with plane hit@k or skill P/R.
//!
//! Skip-honest without a host. Temporal `as_of` / `known_as_of` probes are
//! skipped here (plane owns intervals). Closed `valid_until` gold is not encoded
//! (system store is valid-now). `multi_hop` / `skip_if_no_graph` stay skipped.
//! Discipline (`recall_skip`) is the vegapunk shim log, not host self-report.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use tokio::process::Command as TokioCommand;
use tokio::time::timeout;

use nomiso_eval::{CodingScenario, FixtureProbe, ProbeTag};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::llm::parse_json_from_model;

/// One probe's system-track row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemProbeRow {
    pub scenario: String,
    pub probe: String,
    pub tag: String,
    pub skipped: Option<String>,
    /// Host did not invoke `hard-recall` for this probe (discipline miss).
    pub recall_skip: bool,
    pub fact_hit: bool,
    pub leak: bool,
    pub abstain_ok: Option<bool>,
    pub passed: bool,
    pub detail: String,
}

/// System-track report (never averaged into plane/skill).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodingSystemReport {
    pub track: String,
    pub host: String,
    pub embedder: String,
    pub skipped: Option<String>,
    pub rows: Vec<SystemProbeRow>,
    /// Report-only: true unless a runner error. Quality is the counts.
    pub passed: bool,
    pub n_scored: u32,
    pub n_skip_graph_or_time: u32,
    pub recall_skip_n: u32,
    pub fact_hit_n: u32,
    pub leak_n: u32,
}

impl CodingSystemReport {
    /// Markdown table for last-miss / stdout.
    pub fn markdown(&self) -> String {
        let mut s = format!(
            "# Coding system `{}`\n\ntrack={} embedder={} passed={} skipped={:?}\n",
            self.host, self.track, self.embedder, self.passed, self.skipped
        );
        s.push_str(&format!(
            "scored={} graph_or_time_skip={} recall_skip={} fact_hit={} leak={}\n",
            self.n_scored,
            self.n_skip_graph_or_time,
            self.recall_skip_n,
            self.fact_hit_n,
            self.leak_n
        ));
        s.push_str("(passed=runner ok; recall_skip is discipline; fact_hit is full pack.block + answer vs gold text)\n\n");
        s.push_str("| scenario | probe | tag | pass | skip_recall | hit | leak | detail |\n|---|---|---|---|---|---|---|---|\n");
        for r in &self.rows {
            if r.skipped.is_some() {
                continue;
            }
            s.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} |\n",
                r.scenario,
                r.probe,
                r.tag,
                r.passed,
                r.recall_skip,
                r.fact_hit,
                r.leak,
                r.detail.replace('|', "/")
            ));
        }
        s
    }
}

/// Skip-honest report (CI). Does not call a host.
pub fn skip_system(why: &str) -> CodingSystemReport {
    CodingSystemReport {
        track: "system".into(),
        host: "none".into(),
        embedder: "n/a".into(),
        skipped: Some(why.into()),
        rows: vec![],
        passed: true,
        n_scored: 0,
        n_skip_graph_or_time: 0,
        recall_skip_n: 0,
        fact_hit_n: 0,
        leak_n: 0,
    }
}

/// CI entry: skip unless `VEGAPUNK_SYSTEM_LIVE=1` (live goes through [`run_system_grok`]).
pub async fn run_coding_system() -> Result<CodingSystemReport> {
    let live = std::env::var("VEGAPUNK_SYSTEM_LIVE")
        .map(|v| matches!(v.as_str(), "1" | "true" | "yes"))
        .unwrap_or(false);
    if !live {
        return Ok(skip_system(
            "VEGAPUNK_SYSTEM_LIVE unset (skip-honest; use just eval-coding-system-live)",
        ));
    }
    run_system_grok().await
}

/// Re-score gitignored system dumps (`{scenario_id}.json`). No host call.
///
/// `pack_log` (vegapunk stdout records) is required for full-pack `fact_hit`.
/// Preview-only dumps fall back to the host `pack_preview` (the 500-char cut).
pub async fn run_system_from_dir(dir: &Path) -> Result<CodingSystemReport> {
    if !dir.is_dir() {
        return Ok(skip_system(&format!(
            "system dump dir missing: {} (skip-honest)",
            dir.display()
        )));
    }
    let scenarios = load_pack()?;
    let mut rows = Vec::new();
    let mut any = false;
    for s in &scenarios {
        let path = dir.join(format!("{}.json", s.id));
        if !path.is_file() {
            continue;
        }
        any = true;
        let raw = std::fs::read_to_string(&path).map_err(|e| Error::io(e.to_string()))?;
        let dump: SystemDump = serde_json::from_str(&raw)
            .map_err(|e| Error::invalid(format!("system dump {}: {e}", path.display())))?;
        if !dump.scenario.is_empty() && dump.scenario != s.id {
            return Err(Error::invalid(format!(
                "system dump {} scenario={} expected {}",
                path.display(),
                dump.scenario,
                s.id
            )));
        }
        let probes: Vec<&FixtureProbe> = s.probes.iter().collect();
        let scored: Vec<&FixtureProbe> = probes
            .iter()
            .copied()
            .filter(|p| system_skip_reason(p).is_none())
            .collect();
        for p in &probes {
            if let Some(why) = system_skip_reason(p) {
                rows.push(SystemProbeRow {
                    scenario: s.id.clone(),
                    probe: p.name.clone(),
                    tag: tag_name(p),
                    skipped: Some(why),
                    recall_skip: false,
                    fact_hit: false,
                    leak: false,
                    abstain_ok: None,
                    passed: true,
                    detail: "skipped".into(),
                });
            }
        }
        if scored.is_empty() {
            continue;
        }
        let parsed = parse_host_probes(&dump.host).unwrap_or_default();
        let recalled = assign_recalls(&dump.log_delta, &scored);
        let packs = assign_pack_blocks(&dump.pack_log, &scored);
        for p in scored {
            rows.push(score_probe(
                s,
                p,
                &parsed,
                recalled.get(&p.name).copied().unwrap_or(false),
                packs.get(&p.name).map(String::as_str).unwrap_or(""),
            ));
        }
    }
    if !any {
        return Ok(skip_system(&format!(
            "no system dumps in {} (skip-honest)",
            dir.display()
        )));
    }
    Ok(summarize("dump-dir", "n/a", rows))
}

/// Live Grok CLI host. Skip-honest if `grok` is missing.
pub async fn run_system_grok() -> Result<CodingSystemReport> {
    let grok = std::env::var("VEGAPUNK_LLM_BIN").unwrap_or_else(|_| "grok".into());
    if which(&grok).is_none() {
        return Ok(skip_system(&format!("{grok} not on PATH (skip-honest)")));
    }
    let vp_bin = vegapunk_bin()?;
    let scenarios = load_pack()?;
    let limit = std::env::var("EVAL_SYSTEM_LIMIT")
        .ok()
        .and_then(|s| s.parse::<usize>().ok());
    let scenarios: Vec<&CodingScenario> = match limit {
        Some(n) => scenarios.iter().take(n).collect(),
        None => scenarios.iter().collect(),
    };

    let dir = std::env::temp_dir().join(format!("nomiso-system-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| Error::io(e.to_string()))?;
    let data = dir.join("rocks");
    std::fs::create_dir_all(&data).map_err(|e| Error::io(e.to_string()))?;
    let blobs = dir.join("blobs");
    std::fs::create_dir_all(&blobs).map_err(|e| Error::io(e.to_string()))?;
    let log_path = dir.join("recall.log");
    let pack_log_path = dir.join("recall.packs");
    let cfg_path = dir.join("vegapunk.toml");
    let (embedder, toml) = isolated_toml(&data);
    std::fs::write(&cfg_path, toml).map_err(|e| Error::io(e.to_string()))?;

    let real_copy = dir.join("vegapunk.bin");
    std::fs::copy(&vp_bin, &real_copy).map_err(|e| Error::io(e.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&real_copy, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| Error::io(e.to_string()))?;
    }
    write_shim(&dir.join("vegapunk"), &real_copy, &log_path, &pack_log_path)?;

    struct TmpHold(PathBuf);
    impl Drop for TmpHold {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _tmp = TmpHold(dir.clone());

    ingest_gold(&vp_bin, &cfg_path, &blobs, &scenarios)?;

    let mut rows = Vec::new();
    for s in &scenarios {
        let probes: Vec<&FixtureProbe> = s.probes.iter().collect();
        let scored: Vec<&FixtureProbe> = probes
            .iter()
            .copied()
            .filter(|p| system_skip_reason(p).is_none())
            .collect();
        for p in &probes {
            if let Some(why) = system_skip_reason(p) {
                rows.push(SystemProbeRow {
                    scenario: s.id.clone(),
                    probe: p.name.clone(),
                    tag: tag_name(p),
                    skipped: Some(why),
                    recall_skip: false,
                    fact_hit: false,
                    leak: false,
                    abstain_ok: None,
                    passed: true,
                    detail: "skipped".into(),
                });
            }
        }
        if scored.is_empty() {
            continue;
        }
        let before = read_log(&log_path);
        let pack_before = read_log(&pack_log_path);
        let host_out = grok_scenario(&grok, &cfg_path, &dir, &blobs, s, &scored).await?;
        let after = read_log(&log_path);
        let pack_after = read_log(&pack_log_path);
        let delta = after.strip_prefix(&before).unwrap_or(after.as_str());
        let pack_delta = pack_after
            .strip_prefix(&pack_before)
            .unwrap_or(pack_after.as_str());
        let parsed = parse_host_probes(&host_out).unwrap_or_default();
        let recalled = assign_recalls(delta, &scored);
        let packs = assign_pack_blocks(pack_delta, &scored);
        for p in scored {
            rows.push(score_probe(
                s,
                p,
                &parsed,
                recalled.get(&p.name).copied().unwrap_or(false),
                packs.get(&p.name).map(String::as_str).unwrap_or(""),
            ));
        }
        if let Ok(dump) = std::env::var("VEGAPUNK_SYSTEM_DUMP") {
            let path = PathBuf::from(dump).join(format!("{}.json", s.id));
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(
                path,
                serde_json::to_string_pretty(&serde_json::json!({
                    "scenario": s.id,
                    "host": host_out,
                    "log_delta": delta,
                    "pack_log": pack_delta,
                }))
                .unwrap_or_default(),
            );
        }
    }

    Ok(summarize("grok-cli", embedder, rows))
}

fn load_pack() -> Result<Vec<CodingScenario>> {
    let scenarios =
        nomiso_eval::bundled_coding_scenarios().map_err(|e| Error::invalid(e.to_string()))?;
    nomiso_eval::validate_coding_pack(&scenarios).map_err(|e| Error::invalid(e.to_string()))?;
    Ok(scenarios)
}

fn vegapunk_bin() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("VEGAPUNK_BIN") {
        return Ok(PathBuf::from(p));
    }
    let exe = std::env::current_exe().map_err(|e| Error::io(e.to_string()))?;
    let name = exe.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if name == "vegapunk" || name.starts_with("vegapunk") {
        return Ok(exe);
    }
    let cand = PathBuf::from("target/debug/vegapunk");
    if cand.is_file() {
        return Ok(cand.canonicalize().unwrap_or(cand));
    }
    Err(Error::invalid(
        "no vegapunk binary; run via vegapunk-cli or set VEGAPUNK_BIN",
    ))
}

fn isolated_toml(data: &Path) -> (&'static str, String) {
    let endpoint = format!("rocksdb://{}", data.display());
    if std::env::var("VEGAPUNK_EMBED_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .is_some()
        && curl_ok("http://127.0.0.1:11434/api/tags")
    {
        let url = std::env::var("VEGAPUNK_EMBED_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:11434/v1".into());
        let model = std::env::var("VEGAPUNK_EMBED_MODEL")
            .unwrap_or_else(|_| "qllama/bge-small-en-v1.5".into());
        (
            "http",
            format!(
                "endpoint = \"{endpoint}\"\nprofile = \"coding-agent\"\nembed_dim = 384\n\
                 embed_url = \"{url}\"\nembed_model = \"{model}\"\n\
                 embed_api_key_env = \"VEGAPUNK_EMBED_API_KEY\"\n"
            ),
        )
    } else {
        (
            "hashing",
            format!("endpoint = \"{endpoint}\"\nprofile = \"coding-agent\"\nembed_dim = 32\n"),
        )
    }
}

fn curl_ok(url: &str) -> bool {
    Command::new("curl")
        .args(["-fsS", "-m", "2", url])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Parent-shell VEGAPUNK_* would beat vegapunk.toml via clap `env =`.
const ISOLATE_UNSET: &[&str] = &[
    "VEGAPUNK_ENDPOINT",
    "VEGAPUNK_EMBED_URL",
    "VEGAPUNK_EMBED_MODEL",
    "VEGAPUNK_EMBED_DIM",
    "VEGAPUNK_PROFILE",
    "VEGAPUNK_SCOPE",
];

fn isolate_std(cmd: &mut Command, cfg: &Path, blobs: &Path) {
    for k in ISOLATE_UNSET {
        cmd.env_remove(*k);
    }
    cmd.env("VEGAPUNK_CONFIG", cfg);
    cmd.env("VEGAPUNK_BLOB_ROOT", blobs);
}

fn isolate_tokio(cmd: &mut TokioCommand, cfg: &Path, blobs: &Path) {
    for k in ISOLATE_UNSET {
        cmd.env_remove(*k);
    }
    cmd.env("VEGAPUNK_CONFIG", cfg);
    cmd.env("VEGAPUNK_BLOB_ROOT", blobs);
}

fn system_timeout() -> Duration {
    let secs = std::env::var("VEGAPUNK_SYSTEM_TIMEOUT")
        .ok()
        .and_then(|s| s.parse().ok())
        .or_else(|| {
            std::env::var("VEGAPUNK_LLM_TIMEOUT")
                .ok()
                .and_then(|s| s.parse().ok())
        })
        .unwrap_or(300u64);
    Duration::from_secs(secs.max(30))
}

fn write_shim(path: &Path, real: &Path, log: &Path, pack_log: &Path) -> Result<()> {
    // Log argv for recall_skip. Capture stdout so fact_hit can use full pack.block
    // instead of the host's 500-char pack_preview. Forward stdout to the host.
    let body = format!(
        "#!/bin/sh\n\
         printf '%s\\n' \"$*\" >> \"{log}\"\n\
         tmp=`mktemp` || exit 1\n\
         \"{real}\" \"$@\" > \"$tmp\"\n\
         st=$?\n\
         {{\n\
         printf '===VP_ARGV===\\n'\n\
         printf '%s\\n' \"$*\"\n\
         printf '===VP_STDOUT===\\n'\n\
         cat \"$tmp\"\n\
         printf '\\n===VP_END===\\n'\n\
         }} >> \"{packs}\"\n\
         cat \"$tmp\"\n\
         rm -f \"$tmp\"\n\
         exit $st\n",
        log = log.display(),
        real = real.display(),
        packs = pack_log.display()
    );
    std::fs::write(path, body).map_err(|e| Error::io(e.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| Error::io(e.to_string()))?;
    }
    Ok(())
}

fn ingest_gold(vp: &Path, cfg: &Path, blobs: &Path, scenarios: &[&CodingScenario]) -> Result<()> {
    for s in scenarios {
        for m in &s.gold_memories {
            // Closed historical rows are plane as_of; system store is valid-now.
            if m.valid_until.is_some() {
                continue;
            }
            let mut cmd = Command::new(vp);
            cmd.args([
                "--no-help",
                "--format",
                "json",
                "encode",
                "--scope",
                &m.scope,
                "--text",
                &m.text,
            ]);
            if let Some(c) = m.category.as_deref() {
                cmd.args(["--category", c]);
            }
            isolate_std(&mut cmd, cfg, blobs);
            let out = cmd.output().map_err(|e| Error::io(e.to_string()))?;
            if !out.status.success() {
                return Err(Error::invalid(format!(
                    "encode {} failed: {}",
                    m.key,
                    String::from_utf8_lossy(&out.stderr)
                )));
            }
        }
    }
    Ok(())
}

fn grok_prompts(s: &CodingScenario, probes: &[&FixtureProbe]) -> (&'static str, String) {
    let mut probe_lines = String::new();
    for (i, p) in probes.iter().enumerate() {
        probe_lines.push_str(&format!(
            "{}. name={} query={:?} scope={}\n",
            i + 1,
            p.name,
            p.query,
            p.scope
        ));
        if let Some(c) = &p.category {
            probe_lines.push_str(&format!("   category={c}\n"));
        }
    }
    let user = format!(
        "Scenario: {}\nWorking directory already contains ./vegapunk. For every probe you MUST run this command with the terminal tool:\n  \
         ./vegapunk --no-help --format json hard-recall --query <query> --scope <scope> --pack\n\
         Add --category if listed. After ALL probes, output ONLY a JSON array:\n\
         [{{\"probe\":\"<name>\",\"recalled\":true,\"abstain\":false,\"pack_preview\":\"<pack.block first 500 chars>\",\"answer\":\"<one sentence or ABSTAIN>\"}}]\n\n\
         Probes:\n{probe_lines}",
        s.id
    );
    let system = "You are a coding agent using Vegapunk for durable memory. \
         Do not invent project facts. Empty pack → abstain. \
         Run ./vegapunk with the host terminal tool. \
         After the recalls, print the JSON array and stop.";
    (system, user)
}

async fn grok_scenario(
    grok: &str,
    cfg: &Path,
    cwd: &Path,
    blobs: &Path,
    s: &CodingScenario,
    probes: &[&FixtureProbe],
) -> Result<String> {
    let (system, user) = grok_prompts(s, probes);

    let path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{path}", cwd.display());
    let mut cmd = TokioCommand::new(grok);
    cmd.args([
        "--output-format",
        "plain",
        "--system-prompt-override",
        system,
        "--always-approve",
        "--no-subagents",
        "--no-plan",
        "--verbatim",
        "--no-memory",
        "--max-turns",
        "24",
        "-p",
        &user,
    ])
    .env("PATH", new_path)
    .current_dir(cwd)
    .stdin(std::process::Stdio::null())
    .kill_on_drop(true);
    #[cfg(unix)]
    {
        cmd.process_group(0);
    }
    isolate_tokio(&mut cmd, cfg, blobs);
    if let Ok(m) = std::env::var("VEGAPUNK_LLM_MODEL") {
        cmd.args(["--model", &m]);
    }
    if std::env::var("VEGAPUNK_EMBED_API_KEY").is_err() {
        cmd.env("VEGAPUNK_EMBED_API_KEY", "ollama");
    }
    let dur = system_timeout();
    let out = match timeout(dur, cmd.output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(Error::llm(format!("grok spawn: {e}"))),
        Err(_) => {
            return Err(Error::llm(format!(
                "grok timed out after {}s",
                dur.as_secs()
            )));
        }
    };
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if stdout.trim().is_empty() {
        return Err(Error::llm(format!(
            "grok empty stdout status={:?} stderr={stderr}",
            out.status.code()
        )));
    }
    // Max-turns / tool loops still yield a body we can score (recall_skip if no pack).
    let _ = stderr;
    Ok(stdout)
}

#[derive(Debug, Clone, Deserialize)]
struct SystemDump {
    scenario: String,
    host: String,
    #[serde(default)]
    log_delta: String,
    /// Vegapunk shim stdout records (`===VP_ARGV===` …). Empty on preview-era dumps.
    #[serde(default)]
    pack_log: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct HostProbe {
    probe: String,
    #[serde(default)]
    recalled: bool,
    #[serde(default)]
    abstain: bool,
    #[serde(default)]
    pack_preview: String,
    #[serde(default)]
    answer: String,
}

fn parse_host_probes(raw: &str) -> Result<HashMap<String, HostProbe>> {
    let v: Vec<HostProbe> = parse_json_from_model(raw)?;
    Ok(v.into_iter().map(|p| (p.probe.clone(), p)).collect())
}

fn system_skip_reason(p: &FixtureProbe) -> Option<String> {
    if p.skip_if_no_graph || p.tag == Some(ProbeTag::MultiHop) {
        return Some("multi_hop / no graph writer".into());
    }
    if p.as_of.is_some() || p.known_as_of.is_some() {
        return Some("temporal lens is plane-owned (CLI encode has no gold valid_*)".into());
    }
    None
}

fn tag_name(p: &FixtureProbe) -> String {
    p.tag
        .map(|t| format!("{t:?}").to_ascii_lowercase())
        .unwrap_or_else(|| "untagged".into())
}

fn gold_by_key(s: &CodingScenario) -> HashMap<&str, &str> {
    s.gold_memories
        .iter()
        .map(|m| (m.key.as_str(), m.text.as_str()))
        .collect()
}

fn blob_has(blob: &str, gold: &str) -> bool {
    text_overlap(gold, blob)
}

/// `must_not` leak: forbidden gold sentence appears, not 50% token overlap.
fn leak_has(blob: &str, gold: &str) -> bool {
    if gold.is_empty() || blob.is_empty() {
        return false;
    }
    let a = normalize(gold);
    let b = normalize(blob);
    if a.is_empty() {
        return false;
    }
    b.contains(&a)
}

fn text_overlap(gold: &str, pred: &str) -> bool {
    if gold.is_empty() || pred.is_empty() {
        return false;
    }
    let a = normalize(gold);
    let b = normalize(pred);
    if a.contains(&b) || b.contains(&a) {
        return true;
    }
    let ta: std::collections::HashSet<&str> =
        a.split_whitespace().filter(|w| w.len() > 3).collect();
    let tb: std::collections::HashSet<&str> =
        b.split_whitespace().filter(|w| w.len() > 3).collect();
    if ta.is_empty() || tb.is_empty() {
        return false;
    }
    let inter = ta.intersection(&tb).count();
    let shorter = ta.len().min(tb.len());
    inter * 2 >= shorter
}

fn normalize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || c.is_whitespace() {
                c
            } else {
                ' '
            }
        })
        .flat_map(|c| c.to_lowercase())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// One shim `hard-recall` line credits at most one probe (longest query first).
const PACK_ARGV: &str = "===VP_ARGV===";
const PACK_STDOUT: &str = "===VP_STDOUT===";
const PACK_END: &str = "===VP_END===";

fn parse_pack_records(raw: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = raw;
    while let Some(i) = rest.find(PACK_ARGV) {
        rest = &rest[i + PACK_ARGV.len()..];
        let Some(j) = rest.find(PACK_STDOUT) else {
            break;
        };
        let argv = rest[..j].trim().to_string();
        rest = &rest[j + PACK_STDOUT.len()..];
        let Some(k) = rest.find(PACK_END) else {
            break;
        };
        let stdout = rest[..k].trim().to_string();
        rest = &rest[k + PACK_END.len()..];
        out.push((argv, stdout));
    }
    out
}

fn pack_block_from_stdout(stdout: &str) -> String {
    let Some(start) = stdout.find('{') else {
        return String::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&stdout[start..]) else {
        return String::new();
    };
    v.get("pack")
        .and_then(|p| p.get("block"))
        .and_then(|b| b.as_str())
        .or_else(|| v.get("block").and_then(|b| b.as_str()))
        .unwrap_or("")
        .to_string()
}

/// One captured vegapunk stdout credits at most one probe (longest query first).
fn assign_pack_blocks(pack_log: &str, probes: &[&FixtureProbe]) -> HashMap<String, String> {
    let mut recs = parse_pack_records(pack_log);
    let mut order: Vec<&FixtureProbe> = probes.to_vec();
    order.sort_by_key(|p| std::cmp::Reverse(p.query.len()));
    let mut hit: HashMap<String, String> = HashMap::new();
    for p in order {
        let q = p.query.to_ascii_lowercase();
        if q.is_empty() {
            continue;
        }
        if let Some(i) = recs.iter().position(|(argv, _)| {
            let a = argv.to_ascii_lowercase();
            a.contains("hard-recall") && a.contains(&q)
        }) {
            let stdout = recs.remove(i).1;
            hit.insert(p.name.clone(), pack_block_from_stdout(&stdout));
        }
    }
    hit
}

fn assign_recalls(log_delta: &str, probes: &[&FixtureProbe]) -> HashMap<String, bool> {
    let mut lines: Vec<String> = log_delta
        .lines()
        .filter(|l| l.to_ascii_lowercase().contains("hard-recall"))
        .map(|s| s.to_ascii_lowercase())
        .collect();
    let mut order: Vec<&FixtureProbe> = probes.to_vec();
    order.sort_by_key(|p| std::cmp::Reverse(p.query.len()));
    let mut hit: HashMap<String, bool> = probes.iter().map(|p| (p.name.clone(), false)).collect();
    for p in order {
        let q = p.query.to_ascii_lowercase();
        if q.is_empty() {
            continue;
        }
        if let Some(i) = lines.iter().position(|line| line.contains(&q)) {
            lines.remove(i);
            hit.insert(p.name.clone(), true);
        }
    }
    hit
}

fn score_probe(
    s: &CodingScenario,
    p: &FixtureProbe,
    parsed: &HashMap<String, HostProbe>,
    recalled_log: bool,
    pack_block: &str,
) -> SystemProbeRow {
    let keys = gold_by_key(s);
    let host = parsed.get(&p.name);
    let preview = host.map(|h| h.pack_preview.as_str()).unwrap_or("");
    // Full vegapunk pack.block when the shim captured stdout; else host preview
    // (preview-era dumps / tests). Do not retune gold to hide a miss.
    let pack = if !pack_block.trim().is_empty() {
        pack_block
    } else {
        preview
    };
    let answer = host.map(|h| h.answer.as_str()).unwrap_or("");
    let blob = format!("{pack}\n{answer}");
    // Host `recalled: true` is self-report; discipline is the shim log only.
    let host_recalled = host.map(|h| h.recalled).unwrap_or(false);
    let recall_skip = !recalled_log;

    let mut missing = Vec::new();
    for k in &p.expect_keys_in_top {
        let Some(text) = keys.get(k.as_str()).copied() else {
            missing.push(k.clone());
            continue;
        };
        if !blob_has(&blob, text) {
            missing.push(k.clone());
        }
    }
    let fact_hit = missing.is_empty();

    let expect_texts: Vec<&str> = p
        .expect_keys_in_top
        .iter()
        .filter_map(|k| keys.get(k.as_str()).copied())
        .collect();
    let mut leak = false;
    for k in &p.must_not_include_keys {
        if let Some(text) = keys.get(k.as_str()).copied() {
            // Plane must_not is key-based. Leak is the forbidden *sentence*
            // (or a substring of it), not fact_hit's 50% bag-of-words — a long
            // pack.block shares Nomiso/eval/coding tokens with scope gold.
            // Overlap with the *current* expect gold (e.g. rustfmt in v2) is not a leak.
            if leak_has(&blob, text) && !expect_texts.iter().any(|e| text_overlap(text, e)) {
                leak = true;
            }
        }
    }

    let abstain_host = host.map(|h| h.abstain).unwrap_or(false);
    let abstain_ok = if p.expect_abstain {
        Some(
            recalled_log
                && (abstain_host
                    || pack.trim().is_empty()
                    || answer.to_ascii_uppercase().contains("ABSTAIN")),
        )
    } else {
        None
    };

    let passed = if p.expect_abstain {
        abstain_ok == Some(true) && !leak && !recall_skip
    } else {
        fact_hit && !leak && !recall_skip
    };

    SystemProbeRow {
        scenario: s.id.clone(),
        probe: p.name.clone(),
        tag: tag_name(p),
        skipped: None,
        recall_skip,
        fact_hit,
        leak,
        abstain_ok,
        passed,
        detail: if p.expect_abstain {
            format!(
                "abstain_ok={abstain_ok:?} leak={leak} recall_skip={recall_skip} host_recalled={host_recalled}"
            )
        } else {
            format!(
                "missing={missing:?} leak={leak} recall_skip={recall_skip} host_recalled={host_recalled}"
            )
        },
    }
}

fn summarize(host: &str, embedder: &str, rows: Vec<SystemProbeRow>) -> CodingSystemReport {
    let n_skip = rows.iter().filter(|r| r.skipped.is_some()).count() as u32;
    let scored: Vec<&SystemProbeRow> = rows.iter().filter(|r| r.skipped.is_none()).collect();
    let n_scored = scored.len() as u32;
    let recall_skip_n = scored.iter().filter(|r| r.recall_skip).count() as u32;
    let fact_hit_n = scored.iter().filter(|r| r.fact_hit).count() as u32;
    let leak_n = scored.iter().filter(|r| r.leak).count() as u32;
    CodingSystemReport {
        track: "system".into(),
        host: host.into(),
        embedder: embedder.into(),
        skipped: None,
        rows,
        passed: true,
        n_scored,
        n_skip_graph_or_time: n_skip,
        recall_skip_n,
        fact_hit_n,
        leak_n,
    }
}

fn read_log(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

fn which(bin: &str) -> Option<PathBuf> {
    if bin.contains('/') {
        let p = PathBuf::from(bin);
        return p.is_file().then_some(p);
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(bin);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use nomiso_eval::{CodingScenario, FixtureMemory, FixtureProbe, ProbeTag};

    fn probe(name: &str, tag: ProbeTag) -> FixtureProbe {
        FixtureProbe {
            name: name.into(),
            query: "vegapunk-cli Rust AXI".into(),
            scope: "org/eval/coding/nomiso".into(),
            expect_keys_in_top: vec!["p01_rust_cli".into()],
            tag: Some(tag),
            ..FixtureProbe::default()
        }
    }

    fn scenario() -> CodingScenario {
        CodingScenario {
            id: "p01_toolchain_cli_lang".into(),
            pattern: "p01_toolchain".into(),
            scope: "org/eval/coding/nomiso".into(),
            transcript: vec![],
            gold_ops: vec![],
            gold_memories: vec![FixtureMemory {
                key: "p01_rust_cli".into(),
                scope: "org/eval/coding/nomiso".into(),
                text:
                    "vegapunk-cli is implemented in Rust. Do not rewrite the AXI CLI in TypeScript."
                        .into(),
                category: Some("semantic".into()),
                with_embedding: true,
                ..FixtureMemory::default()
            }],
            probes: vec![],
            notes: None,
            origin: None,
        }
    }

    #[tokio::test]
    async fn coding_system_skips_without_live_flag() {
        std::env::remove_var("VEGAPUNK_SYSTEM_LIVE");
        let r = run_coding_system().await.expect("skip");
        assert!(r.skipped.unwrap().contains("VEGAPUNK_SYSTEM_LIVE"));
        assert!(r.passed);
    }

    #[test]
    fn system_skips_temporal_and_graph() {
        let mut p = probe("t", ProbeTag::Temporal);
        p.as_of = Some("2026-01-01T00:00:00Z".into());
        assert!(system_skip_reason(&p).unwrap().contains("temporal"));
        let p = probe("g", ProbeTag::MultiHop);
        assert!(system_skip_reason(&p).unwrap().contains("graph"));
    }

    #[test]
    fn score_detects_recall_skip_and_fact_hit() {
        let s = scenario();
        let p = probe("p01_exact_rust_cli", ProbeTag::Exact);
        let mut parsed = HashMap::new();
        parsed.insert(
            p.name.clone(),
            HostProbe {
                probe: p.name.clone(),
                recalled: false,
                abstain: false,
                pack_preview:
                    "vegapunk-cli is implemented in Rust. Do not rewrite the AXI CLI in TypeScript."
                        .into(),
                answer: "Rust.".into(),
            },
        );
        let row = score_probe(&s, &p, &parsed, false, "");
        assert!(row.fact_hit);
        assert!(row.recall_skip);
        assert!(!row.passed);

        parsed.get_mut(&p.name).unwrap().recalled = true;
        let row = score_probe(&s, &p, &parsed, false, "");
        assert!(
            row.recall_skip,
            "host recalled=true must not clear skip without a shim log"
        );
        assert!(!row.passed);

        let row = score_probe(&s, &p, &parsed, true, "");
        assert!(!row.recall_skip);
        assert!(row.passed);
    }

    #[test]
    fn score_abstain_ok_on_empty_pack() {
        let s = CodingScenario {
            id: "p11".into(),
            pattern: "p11_abstain".into(),
            scope: "org/eval/coding/nobody".into(),
            transcript: vec![],
            gold_ops: vec![],
            gold_memories: vec![],
            probes: vec![],
            notes: None,
            origin: None,
        };
        let p = FixtureProbe {
            name: "p11_abstain_never_ingested".into(),
            query: "ZQXY_NEVER_INGESTED_TOKEN_99".into(),
            scope: "org/eval/coding/nobody".into(),
            expect_abstain: true,
            tag: Some(ProbeTag::Abstain),
            ..FixtureProbe::default()
        };
        let mut parsed = HashMap::new();
        parsed.insert(
            p.name.clone(),
            HostProbe {
                probe: p.name.clone(),
                recalled: true,
                abstain: true,
                pack_preview: String::new(),
                answer: "ABSTAIN".into(),
            },
        );
        let row = score_probe(&s, &p, &parsed, true, "");
        assert_eq!(row.abstain_ok, Some(true));
        assert!(row.passed);

        let row = score_probe(&s, &p, &parsed, false, "");
        assert_eq!(row.abstain_ok, Some(false));
        assert!(row.recall_skip);
        assert!(!row.passed);
    }

    #[test]
    fn sibling_queries_do_not_share_one_log_line() {
        let a = FixtureProbe {
            name: "p07_no_token_leak".into(),
            query: "rot13-not-real staging API token".into(),
            scope: "org/eval/coding/nomiso".into(),
            tag: Some(ProbeTag::ScopeIsolation),
            ..FixtureProbe::default()
        };
        let b = FixtureProbe {
            name: "p07_sidecar_has_token".into(),
            query: "staging API token rot13-not-real".into(),
            scope: "org/eval/coding/sidecar".into(),
            tag: Some(ProbeTag::ScopeIsolation),
            ..FixtureProbe::default()
        };
        let probes = [&a, &b];
        let one = assign_recalls(
            "hard-recall --query rot13-not-real staging API token --pack\n",
            &probes,
        );
        assert_eq!(one.get("p07_no_token_leak"), Some(&true));
        assert_eq!(
            one.get("p07_sidecar_has_token"),
            Some(&false),
            "token-overlap anagram must not steal the sibling's line"
        );
        let both = assign_recalls(
            "hard-recall --query rot13-not-real staging API token --pack\n\
             hard-recall --query staging API token rot13-not-real --pack\n",
            &probes,
        );
        assert_eq!(both.get("p07_no_token_leak"), Some(&true));
        assert_eq!(both.get("p07_sidecar_has_token"), Some(&true));
    }

    #[test]
    fn score_fact_hit_from_full_pack_block_not_preview() {
        let s = CodingScenario {
            id: "p17_near_dup".into(),
            pattern: "p17_near_dup".into(),
            scope: "org/eval/coding/nomiso".into(),
            transcript: vec![],
            gold_ops: vec![],
            gold_memories: vec![
                FixtureMemory {
                    key: "p17_no_autodump".into(),
                    scope: "org/eval/coding/nomiso".into(),
                    text: "Never auto-dump Nomiso memory into the prompt.".into(),
                    category: Some("semantic".into()),
                    with_embedding: true,
                    ..FixtureMemory::default()
                },
                FixtureMemory {
                    key: "p17_pack_only".into(),
                    scope: "org/eval/coding/nomiso".into(),
                    text: "Soft-inject stays off. The host injects Vegapunk pack.block only when it is non-empty.".into(),
                    category: Some("semantic".into()),
                    with_embedding: true,
                    ..FixtureMemory::default()
                },
            ],
            probes: vec![],
            notes: None,
            origin: None,
        };
        let p = FixtureProbe {
            name: "p17_hybrid_inject_doctrine".into(),
            query: "auto-dump pack.block soft-inject".into(),
            scope: "org/eval/coding/nomiso".into(),
            expect_keys_in_top: vec!["p17_no_autodump".into(), "p17_pack_only".into()],
            tag: Some(ProbeTag::Hybrid),
            ..FixtureProbe::default()
        };
        let mut parsed = HashMap::new();
        parsed.insert(
            p.name.clone(),
            HostProbe {
                probe: p.name.clone(),
                recalled: true,
                abstain: false,
                pack_preview: "Soft-inject stays off. The host injects Vegapunk pack.block only when it is non-empty.".into(),
                answer: "Host injects pack.block only.".into(),
            },
        );
        let preview_only = score_probe(&s, &p, &parsed, true, "");
        assert!(
            !preview_only.fact_hit,
            "truncated preview must still miss autodump"
        );

        let mut full = String::new();
        full.push_str("Soft-inject stays off. The host injects Vegapunk pack.block only when it is non-empty.\n");
        full.push_str(&"x".repeat(500));
        full.push('\n');
        full.push_str("Never auto-dump Nomiso memory into the prompt.");
        let row = score_probe(&s, &p, &parsed, true, &full);
        assert!(
            row.fact_hit,
            "full pack.block below the 500-char cut must count: {}",
            row.detail
        );
        assert!(row.passed);
    }

    #[test]
    fn score_does_not_hide_miss_when_full_pack_lacks_gold() {
        let s = CodingScenario {
            id: "p17_near_dup".into(),
            pattern: "p17_near_dup".into(),
            scope: "org/eval/coding/nomiso".into(),
            transcript: vec![],
            gold_ops: vec![],
            gold_memories: vec![
                FixtureMemory {
                    key: "p17_no_autodump".into(),
                    scope: "org/eval/coding/nomiso".into(),
                    text: "Never auto-dump Nomiso memory into the prompt.".into(),
                    category: Some("semantic".into()),
                    with_embedding: true,
                    ..FixtureMemory::default()
                },
                FixtureMemory {
                    key: "p17_pack_only".into(),
                    scope: "org/eval/coding/nomiso".into(),
                    text: "Soft-inject stays off. The host injects Vegapunk pack.block only when it is non-empty.".into(),
                    category: Some("semantic".into()),
                    with_embedding: true,
                    ..FixtureMemory::default()
                },
            ],
            probes: vec![],
            notes: None,
            origin: None,
        };
        let p = FixtureProbe {
            name: "p17_hybrid_inject_doctrine".into(),
            query: "auto-dump pack.block soft-inject".into(),
            scope: "org/eval/coding/nomiso".into(),
            expect_keys_in_top: vec!["p17_no_autodump".into(), "p17_pack_only".into()],
            tag: Some(ProbeTag::Hybrid),
            ..FixtureProbe::default()
        };
        let mut parsed = HashMap::new();
        parsed.insert(
            p.name.clone(),
            HostProbe {
                probe: p.name.clone(),
                recalled: true,
                abstain: false,
                pack_preview: "Soft-inject stays off.".into(),
                answer: "Host injects pack.block only.".into(),
            },
        );
        let only_pack_only = "Soft-inject stays off. The host injects Vegapunk pack.block only when it is non-empty.";
        let row = score_probe(&s, &p, &parsed, true, only_pack_only);
        assert!(
            !row.fact_hit,
            "do not retune: missing autodump stays a miss"
        );
        assert!(row.detail.contains("p17_no_autodump"));
    }

    #[test]
    fn assign_pack_blocks_uses_vegapunk_stdout_not_preview() {
        let p = FixtureProbe {
            name: "p17_hybrid_inject_doctrine".into(),
            query: "auto-dump pack.block soft-inject".into(),
            scope: "org/eval/coding/nomiso".into(),
            tag: Some(ProbeTag::Hybrid),
            ..FixtureProbe::default()
        };
        let stdout = serde_json::json!({
            "pack": {
                "block": "Never auto-dump Nomiso memory into the prompt. Soft-inject stays off."
            }
        })
        .to_string();
        let log = format!(
            "{PACK_ARGV}\n--no-help --format json hard-recall --query auto-dump pack.block soft-inject --pack\n{PACK_STDOUT}\n{stdout}\n{PACK_END}\n"
        );
        let packs = assign_pack_blocks(&log, &[&p]);
        let block = packs.get("p17_hybrid_inject_doctrine").expect("assigned");
        assert!(block.contains("Never auto-dump"));
        assert!(block.contains("Soft-inject stays off"));
    }

    #[test]
    fn leak_requires_forbidden_sentence_not_token_overlap() {
        let s = CodingScenario {
            id: "p07b_scope_tact".into(),
            pattern: "p07_scope_isolation".into(),
            scope: "org/eval/coding/tact".into(),
            transcript: vec![],
            gold_ops: vec![],
            gold_memories: vec![FixtureMemory {
                key: "p07b_tact_secrets".into(),
                scope: "org/eval/coding/tact".into(),
                text: "Tact-on-Nomiso TACT_ROOT_ONLY_SLOT secrets live under org/eval/coding/tact."
                    .into(),
                category: Some("semantic".into()),
                with_embedding: true,
                ..FixtureMemory::default()
            }],
            probes: vec![],
            notes: None,
            origin: None,
        };
        let p = FixtureProbe {
            name: "p07b_no_tact_leak".into(),
            query: "TACT_ROOT_ONLY_SLOT".into(),
            scope: "org/eval/coding/nomiso".into(),
            must_not_include_keys: vec!["p07b_tact_secrets".into()],
            tag: Some(ProbeTag::ScopeIsolation),
            ..FixtureProbe::default()
        };
        let mut parsed = HashMap::new();
        parsed.insert(
            p.name.clone(),
            HostProbe {
                probe: p.name.clone(),
                recalled: true,
                abstain: true,
                pack_preview: "Nomiso eval coding secrets live under org/eval/coding/nomiso."
                    .into(),
                answer: "ABSTAIN".into(),
            },
        );
        let long_pack = "Format gate is rustfmt plus cargo clippy -D warnings. \
             Artifact bytes live in FsBlobStore. Nomiso eval default_scope is \
             org/eval/coding/nomiso. Do not read sidecar secrets.";
        let row = score_probe(&s, &p, &parsed, true, long_pack);
        assert!(
            !row.leak,
            "generic Nomiso tokens must not count as a tact leak"
        );
        assert!(row.passed);

        let leaked = format!(
            "{long_pack}\nTact-on-Nomiso TACT_ROOT_ONLY_SLOT secrets live under org/eval/coding/tact."
        );
        let row = score_probe(&s, &p, &parsed, true, &leaked);
        assert!(row.leak, "full forbidden sentence in pack.block is a leak");
        assert!(!row.passed);
    }

    #[test]
    fn grok_prompt_does_not_say_bash() {
        let s = scenario();
        let p = probe("p01_exact_rust_cli", ProbeTag::Exact);
        let (system, user) = grok_prompts(&s, &[&p]);
        let blob = format!("{system}\n{user}").to_ascii_lowercase();
        assert!(
            !blob.contains("bash"),
            "prompt must not mention bash: {blob}"
        );
        assert!(user.contains("terminal tool"));
        assert!(ISOLATE_UNSET.contains(&"VEGAPUNK_ENDPOINT"));
        assert!(ISOLATE_UNSET.contains(&"VEGAPUNK_EMBED_URL"));
        assert!(ISOLATE_UNSET.contains(&"VEGAPUNK_EMBED_DIM"));
        assert!(ISOLATE_UNSET.contains(&"VEGAPUNK_PROFILE"));
        assert!(ISOLATE_UNSET.contains(&"VEGAPUNK_SCOPE"));
    }
}
