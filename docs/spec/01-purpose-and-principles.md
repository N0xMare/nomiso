# Purpose, principles, and architectural rationale

Status: normative target. Current implementation status is owned by [14](14-current-state-and-transition.md).

## Product purpose

Nomiso is a Rust toolkit for building memory systems.
Its foundation provides durable storage and trustworthy operations; its higher-level toolkit supplies reusable memory-processing mechanisms.
Vegapunk is the opinionated, tested composition offered to users who want a working memory system rather than an assembly project.

The goal is to make expensive experience reusable without allowing irrelevant, outdated, or unsupported information to degrade future decisions.
The system increases effective agent capability through evidence access, continuity, procedure reuse, and relationship discovery.
It does not claim to increase a model's intrinsic reasoning ability or implement model-weight learning through database operations.

## Intended users

| User | Required experience |
| --- | --- |
| Rust application author | Embed the foundation or full memory composition without running a server |
| Agent harness author | Supply allowed events and task context, receive bounded context proposals, record outcomes |
| Memory researcher | Replace processing strategies without rebuilding persistence and evidence handling |
| Shell/MCP agent user | Use a small, predictable set of tools with durable state and actionable errors |
| Operator | Understand storage ownership, provider dependencies, upgrades, and recovery |

## Representative workloads

1. Persist an explicit project constraint and retrieve its current version in a later session.
2. Preserve evidence from a difficult diagnosis and derive a procedure that can be verified on a different task.
3. Discover a dependency or related incident not directly similar to the query text.
4. Explain which observation supports a memory and whether that observation has changed.
5. Resume task state without treating an unfinished plan as an established fact.
6. Compare two retention or retrieval policies against the same captured evidence and tasks.
7. Operate without models when callers provide structured memories and lexical queries.

## Core principles

**PUR-001 — Reusable experience.** Reuse MUST preserve the distinction between observed outcome and inferred explanation. A successful action is not proof that every proposed explanation or generalization is correct.

**PUR-002 — Separate decisions.** Retaining evidence, endorsing a reusable memory, retrieving a candidate, and inserting context MUST be separate decisions. Persisted does not mean true, current, permitted for every caller, or useful now.

**PUR-003 — Evidence before derived authority.** Derived content MUST retain resolvable support or explicitly identify absent support. Model confidence, repeated retrieval, and repeated copying cannot upgrade evidence quality by themselves.

**PUR-004 — Mechanisms below product choices.** Reusable controllers, policies, and algorithms belong in the Nomiso toolkit. Vegapunk selects, configures, and tests a composition. Higher abstraction does not imply product-specific ownership.

**PUR-005 — Model-free foundation.** Foundation correctness MUST NOT require a generative model, embedding service, network model provider, or vendor CLI. Optional model-assisted components live above foundation storage semantics.

**PUR-006 — Explicit context authority.** Neither foundation nor toolkit may silently mutate an arbitrary harness prompt. The controller returns a proposal; the authorized harness decides actual insertion and reports it.

**PUR-007 — Conservative default, open experiments.** Baseline behavior MUST remain usable without graph extraction, learned policy, automatic background reasoning, or proactive context selection. Alternative policies MAY be evaluated without bypassing invariants.

**PUR-008 — Evaluate utility, not complexity.** A new module MUST NOT become a supported default solely because it implements a fashionable architecture, improves one tuned fixture, or produces plausible reasoning. Promotion follows document 13.

## Non-goals

- Replacing the harness's agent loop, tools, permissions, or action approval.
- A second structured database backend during this transition.
- A multi-tenant SaaS administration/control plane.
- A universal ontology, workflow language, or dynamic code plugin marketplace.
- Mandatory archival of raw conversations or sensitive data.
- Unbounded per-turn memory dumps.
- Automatic promotion of model-inferred relationships to verified facts.
- A universal optimality claim across all models, tasks, and budgets.
- Neural-memory architectures inside the foundation.

These are scope boundaries, not statements that the excluded research directions lack value.
A future scope change requires a design revision, not an undocumented flag.

## Why a toolkit instead of only a database wrapper

Database features are necessary mechanisms, not finished memory capabilities.
Graph storage does not supply entity resolution or a useful traversal policy.
Vector similarity does not decide applicability or truth.
A transaction does not validate an LLM's interpretation.

The toolkit contributes revision/evidence contracts, bounded processing, composable retrieval, reliable lifecycle operations, and reproducible decisions.
A filesystem of notes and ordinary search remains a legitimate baseline for small workloads.
Nomiso must justify its added complexity through reliability, reuse, or measured task benefit.

## Why SurrealDB remains the substrate

SurrealDB provides structured records, relations, lexical/vector indexes, and transactions in a single engine family.
Nomiso uses these through a Rust boundary rather than exposing database-specific values to every component.
Physical query execution may evolve without changing memory semantics.

Hybrid means multiple retrieval signals, not multiple databases.
Temporal memory fields are intervals and audit clocks, not an assertion that the project implements a time-series database.
CAS stores artifact bytes; it is not a second source of structured belief truth.

No claim about production throughput, filtered HNSW completeness, remote behavior, or cross-platform durability is inferred from the choice of database.
Those properties require tests on supported deployments.

## Research lessons and limits

The following sources informed the target. They are research references, not normative dependencies or a comparable leaderboard.

| Source | Useful lesson | Boundary or caution |
| --- | --- | --- |
| [ReasoningBank](https://research.google/blog/reasoningbank-enabling-agents-to-learn-from-experience/) | Learn transferable procedures from successful and failed experiences | A model's self-assessment needs evidence and downstream validation |
| [ACE](https://arxiv.org/html/2510.04618) | Incremental, itemized playbook updates preserve useful detail | More detailed context and more concise context can each win on different tasks |
| [Hindsight](https://arxiv.org/html/2512.12818v1) | Distinguish evidence, experiences, observations, and beliefs; combine retrieval strategies | Vendor benchmark scores do not establish this implementation's quality |
| [Graphiti](https://help.getzep.com/graphiti/getting-started/overview.mdx) | Evolving entities and temporally valid, sourced relationships | Entity resolution errors can connect unrelated information |
| [HippoRAG 2](https://arxiv.org/html/2502.14802v2) | Combine passage evidence with associative graph retrieval | A graph must not be the only surviving lossy representation |
| [GraphRAG query methods](https://microsoft.github.io/graphrag/query/overview/) | Hierarchical summaries support collection-wide questions | Global summarization need not run for simple fact lookup |
| [A-Mem](https://arxiv.org/html/2502.12110v11) | Evolving linked notes provide flexible organization | Updating an interpretation must not overwrite original evidence |
| [SimpleMem](https://arxiv.org/abs/2601.02553v3) | Structured compression and intent-aware retrieval can improve efficiency | Semantic summaries are not a universal losslessness guarantee |
| [Letta sleep-time processing](https://www.letta.com/blog/sleep-time-compute/) | Background memory processing can reduce foreground work | Background compute must be included in total cost |
| [Memory-R1](https://arxiv.org/abs/2508.19828) | Retention and utilization may be learned policies | Reliable outcomes and strong baselines precede training |
| [Context engineering](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents) | Combine bounded upfront context with just-in-time exploration | No single injection schedule is optimal for every harness |
| [Recursive Language Models](https://alexzhang13.github.io/blog/2025/rlm/) | Externalized context can be explored programmatically | Inference-time exploration is not durable memory lifecycle management |
| [Titans/MIRAS](https://research.google/blog/titans-miras-helping-ai-have-long-term-memory/) | Model-level memory is another research axis | An external store does not implement internal neural memory |

Earlier Spectron comparisons contributed the distinction among valid, known, and system time.
The specification does not depend on unverified claims about Spectron's current product capabilities.
The Tact-shaped consumer demonstrates that restrictive, lexical, explicitly accessed memory is a valid alternative composition; see document 09.

## Interpreting efficiency

Efficiency includes foreground latency, tool calls, prompt/output tokens, background processing, indexing, and operational complexity.
Moving computation into a worker is a latency change, not automatically a total-cost reduction.
Preserving a cached, detailed procedure can sometimes be cheaper than reconstructing a terse summary repeatedly.
Selection should optimize useful information under a declared budget, not minimize text length regardless of task success.

## First supported scope

The initial supported product target is trusted local or single-principal agent memory.
Shared deployments still require an explicit permission boundary.
Scopes are filters and organizational partitions, not authentication credentials.

The project first proves a dependable conservative baseline, then promotes capabilities individually.
This allows publication without pretending that every research module in this specification is already necessary or ready.
