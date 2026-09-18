# Architecture, dependencies, and composition

Status: normative target. [14](14-current-state-and-transition.md) maps existing code to this design.

## Logical layers

| Layer | Owns | Must not own |
| --- | --- | --- |
| Nomiso foundation | Persistence, validation, revisions, evidence, relationships, query primitives, migrations, snapshots | Model interpretation, coding defaults, prompt insertion |
| Nomiso memory toolkit | Reusable retention/extraction, retrieval planning, graph expansion, ranking, context assembly, orchestration, workers | A mandatory Vegapunk profile or vendor model |
| Vegapunk library | Supported composition, profiles, default choices, convenient workflows, capability declarations | Duplicate storage/query physics or private copies of reusable algorithms |
| Harness integration | Allowed events, task context, permission grants, actual insertion, execution outcomes | Unvalidated mutation bypasses |
| Transport adapters | Serialization, protocol lifecycle, authentication at the boundary, config input, presentation | Independent retention/ranking/revision policy |

A reusable policy implementation may live in the toolkit.
Choosing that policy and configuring it as a product default is Vegapunk's responsibility.

## Dependency rules

**ARCH-001 — Rust first.** Every supported domain capability, including controller and background-job execution, MUST be usable through Rust libraries without launching a CLI, HTTP server, or MCP server.

**ARCH-002 — Acyclic ownership.** Foundation MUST NOT depend on the toolkit or Vegapunk. Toolkit MUST NOT depend on Vegapunk. A library MUST NOT depend on a binary crate. Adapters MUST call the owning library for behavior.

**ARCH-003 — Independent foundation.** Structured writes, reads, lexical queries, evidence operations, and migrations MUST work without model credentials. An optional provider failure cannot block an unrelated metadata operation.

**ARCH-004 — Reusable composition.** The controller MUST accept replaceable components through typed Rust interfaces. At least one non-Vegapunk test composition MUST exercise these interfaces without adopting Vegapunk defaults.

**ARCH-005 — Optional dependency closure.** Model providers, experimental strategies, transports, and evaluation runners MUST NOT become unavoidable dependencies of minimal foundation use. Feature combinations must be tested in isolation, not only under workspace feature unification.

**ARCH-006 — Library-owned invariants.** Limits, mutation validation, scope checks, and supplied permission restrictions MUST be enforced beneath transport adapters. A Rust caller receives the same protection as a remote caller using the same granted access.

**ARCH-007 — No ambient runtime in constructors.** Constructors MUST NOT spawn background jobs, scan a user's files, load credentials from arbitrary files, or perform model inference implicitly. Connecting storage and starting workers are explicit lifecycle operations.

**ARCH-008 — Narrow modules before crate proliferation.** Reusable high-level work SHOULD begin in one `nomiso-memory` crate with coherent modules. A new crate requires a dependency, reuse, feature, or release boundary, not merely a new algorithm name.

## Target crate direction

```text
Permitted direct dependencies (consumer -> dependency):
nomiso-schema  -> nomiso-core
nomiso-store   -> nomiso-core, nomiso-schema
nomiso-service -> nomiso-core, nomiso-store
nomiso-embed   -> nomiso-core, nomiso-service
nomiso-memory  -> required foundation/provider crates
vegapunk       -> nomiso-memory and required foundation interfaces
adapters       -> the owning foundation or product library
```

The CAS library may remain independent of the service. The diagram lists architectural edges, not an instruction to add unused dependencies.
`nomiso` remains a convenience facade. If it re-exports toolkit components behind a feature, `nomiso-memory` must depend on lower crates directly, not on that facade.
Evaluation support (`nomiso-eval`) may depend on foundation and optional toolkit APIs, but runtime foundation/toolkit/product libraries must not require evaluation runners in their default dependency closure. Product system runners belong in integration tests or optional maintainer tools that consume Vegapunk; their dependency direction must not force a Vegapunk/eval cycle. The facade may re-export selected lower layers, but those layers never depend back on the facade.
The implementation may use focused internal traits rather than forcing every component to implement the entire existing `MemoryStore` trait.

## Workspace organization

| Location | Role |
| --- | --- |
| `crates/nomiso-core` | Pure operation types, identifiers, errors, validation, capability traits |
| `crates/nomiso-schema` | Packaged migrations and compatibility descriptors |
| `crates/nomiso-store` | Surreal execution, persistence mapping, query and transaction implementation |
| `crates/nomiso-service` | Foundation client and optional embedding orchestration |
| `crates/nomiso-embed` | Hashing and optional HTTP embedding implementations |
| `crates/nomiso-blob` | Content-addressed bytes and backend adapters |
| `crates/nomiso-memory` | Reusable memory toolkit (writer/recall/pack/sleep mechanics) behind a `MemoryPolicy` seam |
| `crates/vegapunk` | Product builder: profiles/policy (`ProfilePolicy` → `MemoryPolicy`), workflow facade, eval runners, CLI-LLM backends |
| `crates/nomiso-http`, `crates/nomiso-mcp` | Foundation transport adapters |
| `crates/nomiso` | Public facade and feature re-exports |
| `crates/nomiso-eval`, `evals/` | Evaluation support and fixtures; not mandatory product runtime |
| `bins/nomisod`, `bins/vegapunk` | Configuration adapters, commands, process lifecycle |
| `examples/` | Alternative consumers, reference harness integration, skills, demonstrations |
| `docs/spec/` | Canonical specification only |

Package READMEs remain discoverability surfaces outside `docs/`.
New agent-tool configuration uses the repository's configured conventions; documentation consolidation does not install hooks or services.

## Toolkit modules and seams

The following names describe target responsibilities, not already exported APIs.

| Component | Input | Output | Side effects |
| --- | --- | --- | --- |
| Retention policy | Allowed event, restrictions, prior context | Retention decision and reasons | None |
| Extractor | Admitted evidence, relevant priors, extraction configuration | Typed candidate memories and evidence references | Optional bounded provider call |
| Proposal validator | Candidates, access grant, current revisions | Validated proposal or typed rejection | Reads; optional explicit support assessor; no writes |
| Retrieval planner | Task/query, context inventory, budget | Bounded retrieval plan | Optional bounded provider call |
| Candidate source | Subquery, filters, budget | Candidates with provenance and completeness | Reads only |
| Expander | Seeds, relationship filters, budget | Additional candidates and evidence paths | Reads only |
| Ranker/selector | Candidates, task, inventory, budget | Ordered selected/excluded items and reasons | Optional bounded provider call |
| Context assembler | Selected revision-bound items, renderer/tokenizer | Context proposal and manifest | No prompt mutation |
| Consolidator | Evidence and current derived state | Mutation proposal and validation plan | Reads/provider calls; no direct write |
| Controller | Explicit workflow request and configured components | Workflow result | Calls the foundation commit boundary |
| Worker | Durable job and configured controller | Checkpointed completion/failure | Leased, idempotent workflow execution |

The commit boundary is shared by manual, model-assisted, and worker-originated proposals.
Components receive bounded data and a restricted client capability, not an unrestricted database connection.
A plugin is ordinary compiled Rust code in this phase; dynamically executing untrusted plugins is out of scope.

## Runtime composition

A builder validates configuration before serving requests.
It resolves policy versions, provider identities, resource budgets, and capability availability.
The resolved configuration is inspectable without revealing secrets.

A composition has an immutable identity for a run:

- Component implementation versions.
- Policy configuration digest.
- Embedding/index generation identities.
- Supported operation/contract versions.
- Effective access restrictions.

Changing a component or configuration creates a new composition identity.
No process silently changes the active strategy because a package update introduced a new default.

## Deployment patterns

### Embedded

A Rust application owns the client and optional worker lifecycle.
An embedded Rocks engine is owned by one process at a time.
Multiple handles inside that process can share a client under the engine's concurrency contract.

### Shared service

One service owns the local embedded store and exposes the same library workflows to multiple clients.
Alternatively, multiple application processes use a supported remote Surreal deployment with tested concurrency behavior.
CLI calls must not race a long-lived local MCP/server process for the same embedded database files.

### Harness-controlled processing

A host may use only foundation operations, only selected toolkit components, or the Vegapunk facade.
Using a memory module does not require replacing the host's agent loop or model client.

## Incremental extraction from current code

1. Correct known behavior before moving it into a shared toolkit.
2. Extract a coherent responsibility without changing its external behavior in the same refactor.
3. Keep product defaults and `ProfilePolicy` translation in Vegapunk.
4. Preserve temporary wrappers where they aid compatibility.
5. Add an independent composition test before declaring the seam reusable.
6. Remove evaluation-only runtime dependencies without changing historical fixture semantics.

## Acceptance

See the `ARCH` acceptance family in [13](13-verification-and-evaluation.md).
The decisive tests are independent library use, dependency/feature isolation, shared adapter behavior, and alternative composition reuse.
