# Vegapunk / Nomiso skills

Installable playbooks for agents. Product skills use the **Vegapunk AXI CLI** (or lib/serve).

| Skill | Path | Role |
|---|---|---|
| **vegapunk** | `vegapunk/SKILL.md` | Monolith dual-plane (generalist agent) |
| **vegapunk-writer** | `vegapunk-writer/SKILL.md` | Encode / apply-ops only |
| **vegapunk-reader** | `vegapunk-reader/SKILL.md` | Hard-recall + pack only |

Historical plane playbooks (not installable): [`legacy/WRITE.md`](./legacy/WRITE.md), [`legacy/HARD_RECALL.md`](./legacy/HARD_RECALL.md), [`legacy/CHECKPOINT.md`](./legacy/CHECKPOINT.md).

**Product boundary:** skills never require Grok/Codex CLIs. Host models extract ops; Vegapunk commits.

**Also:** `working-state` get/put (coding restore-on); `sleep` dry-run default (`--apply` via prefix-preserving `apply_ops`; `--apply-age-out` needs `--older-than-hours`; host-LLM reflect = extract → apply-ops); after `hard-recall --pack` host-inject `pack.block` then `trace-inject` then `trace-outcome` (same `trace_id`); `--min-score` is a plane-score floor (default 0.0 does not abstain); conflicting facts without a trusted `prior_id` → `category: uncertainty` and `list --category uncertainty`; `candidates` before supersede; `ingest-compaction` takes extracted durable lines (not raw chat); `store-artifact` is CAS then metadata.
