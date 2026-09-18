# Vegapunk product composition

Status: normative target. Current interfaces and limitations are in [10](10-interfaces-and-integration.md) and [14](14-current-state-and-transition.md).

## Definition

Vegapunk is the opinionated, tested memory system composed from Nomiso foundation and toolkit components.
It is a Rust library first, not merely a CLI/server brand.
A small product library can own substantial value through coherent defaults, compatibility, ergonomics, and system-level evidence.

## Product responsibilities

**PROD-001 — Composition, not duplication.** Vegapunk MUST obtain reusable extraction, retention, graph expansion, ranking, context assembly, and worker machinery from Nomiso toolkit components. Product-specific defaults and workflows may configure them but must not fork their invariant enforcement.

**PROD-002 — Simple supported path.** The product MUST provide a convenient builder/facade that validates a complete composition. Normal users must not manually wire every component merely to retain a fact and prepare context.

**PROD-003 — Versioned recipe.** A release MUST identify supported profiles, component/policy versions, capability maturity, provider requirements, and tested deployment modes. Updating a module does not automatically change an existing resolved composition.

## Product builder

Target configuration supplies storage/client, profile, optional providers, hard budgets, permissions, correlation, and optional worker configuration.
The builder validates missing capabilities, incompatible embeddings, ambiguous configuration, and impossible budgets before processing.
The resulting handle can be cloned for concurrent library use under shared store semantics.
It does not spawn background jobs or connect to a vendor CLI implicitly.

The product facade exposes observe/propose/apply, prepare-context, evidence reads, task state, consolidation, and feedback workflows.
Existing `remember`, `recall`, `hard_recall_pack`, and writer-op methods may remain compatibility conveniences.
A convenience method cannot hide partial mutation or unavailable evidence behind a simple success boolean.

## Conservative coding-agent profile

**PROD-004 — Baseline defaults.** The initial coding-agent composition MUST use explicit recall/context proposals, strict scope restrictions, guarded mutations, conservative duplicate handling, and host-controlled insertion. It must remain useful without a generative model.

| Concern | Baseline target |
| --- | --- |
| Durable storage | Explicit durable endpoint for agents; ephemeral mode labeled demo/test |
| Writes | Host-supplied structured proposals; optional model-assisted extraction |
| Retrieval | Strong lexical/vector/hybrid baseline with explicit provider/index identity |
| Graph | Optional bounded capability, not required for ordinary recall |
| Context | Evidence-preserving excerpts and budgeted selection; no blind scope dump |
| Working state | Restore available for coding tasks, separate from endorsed memory |
| Procedures | Applicable, evidence-grounded records; inferred status remains visible |
| Consolidation | Dry-run available; unattended mutation restricted to safe declared operations |
| Hard deletion | Administrative explicit operation, not a routine model discretion |
| Background work | Explicit opt-in with readiness and resource controls |
| Failures | Stable typed results, partiality, and retry/receipt guidance |

An optional real embedding provider improves semantic retrieval but does not establish factual correctness.
Hashing remains a labeled lexical test/demo implementation, not a hidden production semantic model.

## Minimal profile

The minimal profile offers explicit, bounded memory with fewer default capabilities.
It may use lexical retrieval only, a small context budget, and no automatic state restoration or derived processing.
It still obeys the same mutation, evidence, and permission contracts.
Profile differences cannot weaken scope checking or make errors ambiguous.

## Experimental profiles

**PROD-005 — Explicit promotion.** Graph-assisted, procedural-first, learned-retention, background-reflection, and lifecycle-triggered context policies MUST be named and versioned experimental compositions until document 13's promotion gates pass.

A user can choose such a profile without changing the conservative default.
An experiment that loses on quality/cost may remain optional or be removed through a compatibility process.
The product must not describe one score from a narrow suite as universal optimality.

## Harness relationship

**PROD-006 — Host-owned execution.** Vegapunk MUST NOT replace the host's task loop, tool permissions, model client, or action approval. It exposes lifecycle operations that a host can call mechanically, without requiring the main model to perform all bookkeeping.

Session/turn identifiers correlate work; they are not permissions or memory scopes.
The host reports actual insertion and observed outcomes rather than asking the model to guess them.
A memory result recommending an action is evidence/advice, not permission to execute that action.

## Skills

Retain monolith and focused writer/reader skills as alternative host guidance surfaces.
They reference the same contracts and supported commands.
Skills may explain when to recall, how to propose updates, and when to abstain; they must not contain a separate implementation of revision or authorization rules.

Current repository discipline remains:

- Use Vegapunk on Nomiso for durable memory, not unrelated harness memory stores.
- Use only the selected `pack.block` after explicit recall for the current CLI path.
- Report actual use with trace injection/outcomes.
- Do not invent a prior ID.
- Do not retain secrets, raw chat, or unfinished plans as factual conclusions.
- Working-state restore is separate from prompt injection.
- No Pi `before_agent_start` recall/dump hook is introduced here.

## Alternative compositions and the Tact example

**PROD-007 — Independent consumers.** The project MUST retain a small alternative composition test/example that uses Nomiso without depending on Vegapunk's profile or facade. This proves the toolkit boundary without creating a second full product roadmap.

Current `examples/tact-on-nomiso` is a useful foundation-level consumer, not yet proof of reuse of the future toolkit.
Its intentionally different policy includes explicit scan/read/put/delete, BM25-only retrieval, root-only mutations, content/corpus bounds, and probation-based pruning.
These choices are not Vegapunk defaults and are not missing semantic capabilities to be repaired indiscriminately.

Historical example contract, retained for discoverability:

| Concern | Tact-shaped adapter behavior at the baseline |
| --- | --- |
| Scope | Fixed configurable corpus, default `tact/global` |
| Actors | Root mutates; children scan/read |
| Content | Semantic conclusions bounded to 1 KiB |
| Retrieval | Top five lexical candidates with short previews |
| Capacity | Adapter checks 512 rows and 256 KiB content |
| Probation | Seven-day unused-memory pruning policy |
| Replacement | New Nomiso assertion ID, product logical ID retained |
| Telemetry | Scan/use metadata separate from optimistic content updates where implemented |
| Secrets | Best-effort rejection, not a privacy proof |

These are existing adapter policies, not certified concurrent quota guarantees.
Legacy rows lacking use telemetry may become prune-eligible; replacement resets use count/probation in the current implementation.
Consumers must review that behavior before enabling pruning after migration.
See the [example README](../../examples/tact-on-nomiso/README.md) and [source](../../examples/tact-on-nomiso/src/lib.rs).

## Product release claims

**PROD-008 — Claim only verified surfaces.** The product MUST distinguish callable, tested, supported, and experimental capabilities. A Rust module's existence does not establish transport parity, a passing unit gate does not establish live-model quality, and a daemon starting does not establish durable recovery.

HTTP may remain a declared subset while Rust/CLI/MCP integration is hardened.
A future SDK must not advertise unavailable operations merely because their target types are specified.

## Acceptance

See `PROD` in [13](13-verification-and-evaluation.md): composition identity, conservative defaults, independent reuse, and end-to-end product workflows.
