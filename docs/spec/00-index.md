# Nomiso and Vegapunk specification

Status: accepted architectural direction; normative target, not an implementation-completion claim.
Specification revision: 2026-09-16.
Audited implementation baseline: Git commit `3fcb456`.

## Purpose

This directory is the canonical specification for Nomiso and Vegapunk.
It replaces the former collection of MVP sketches, product-surface locks, research comparisons, and session handoffs.
The target is a reliable, composable Rust memory toolkit and an opinionated product built from it.
The target is not a universally optimal retrieval algorithm or a replacement agent runtime.

Nomiso comprises two logical layers:

1. A model-free foundation for durable, attributable, revisable, scoped memory.
2. Reusable memory-system components for processing evidence and preparing useful context.

Vegapunk is a Rust-library-first composition of those components.
Its CLI, HTTP service, MCP server, and future SDKs are adapters, not independent implementations of memory behavior.
Harnesses retain authority over execution, permissions, actual context insertion, and task outcomes.

## Glossary

| Term | Meaning |
| --- | --- |
| Foundation/plane | Model-free storage, evidence, revision, query, and consistency contracts |
| Toolkit | Reusable memory-processing and controller components above the foundation |
| Composition | A configured, versioned selection of components; Vegapunk supplies supported recipes |
| Memory bank | Informal name for a configured collection of scoped memory, not a new authorization or database entity |
| Store instance | A named durable namespace/database instance with an explicit identity and compatibility state |
| Evidence | Retained source observation or artifact; not automatically an endorsed fact |
| Proposal | A typed candidate mutation or context selection awaiting the relevant commit/insertion boundary |
| Frontier | A declared visibility boundary for source/store/index state, not merely a wall-clock approximation |

## Reading guide

| Document | Authoritative subject |
| --- | --- |
| [01 Purpose and principles](01-purpose-and-principles.md) | Goals, non-goals, use cases, research rationale |
| [02 Architecture and dependencies](02-architecture-and-dependencies.md) | Library ownership, modules, dependency rules, composition |
| [03 Data model and evidence](03-data-model-and-evidence.md) | Identity, revisions, time, provenance, memory representations |
| [04 Mutations and consistency](04-mutations-and-consistency.md) | Operations, transactions, idempotency, conflicts, errors |
| [05 Relationships and retrieval](05-relationships-and-retrieval.md) | Graph model, candidate channels, filters, ranking primitives |
| [06 Memory processing](06-memory-processing.md) | Admission, extraction, validation, procedures, consolidation |
| [07 Controller and context](07-controller-and-context.md) | Workflows, selection, budgets, manifests, injection boundary |
| [08 Background processing](08-background-processing.md) | Durable jobs, leases, retries, cancellation, consistency |
| [09 Vegapunk product](09-vegapunk-product.md) | Supported composition, profiles, defaults, alternative consumers |
| [10 Interfaces and integration](10-interfaces-and-integration.md) | Rust/CLI/HTTP/MCP/SDK contracts and current operational recipe |
| [11 Security and operations](11-security-and-operations.md) | Trust, permissions, privacy, providers, backups, observability |
| [12 Compatibility and migrations](12-compatibility-and-migrations.md) | Schema evolution, legacy state, indexes, packaging |
| [13 Verification and evaluation](13-verification-and-evaluation.md) | Conformance, acceptance tests, metrics, experiment protocol |
| [14 Current state and transition](14-current-state-and-transition.md) | Evidence-backed baseline, gaps, migration sequence, source disposition |

Read 01, 02, 03, 04, and 07 before implementing controller behavior.
Read 11 and 12 before modifying a durable store or defining a public transport.
Read 13 before describing a feature as tested or an algorithm as an improvement.
Read 14 before starting work: a target requirement is not evidence that the capability exists.

## Normative language

- **MUST / MUST NOT** specify conformance requirements for the target capability.
- **SHOULD / SHOULD NOT** specify the default engineering choice; exceptions require documented rationale and verification.
- **MAY** identifies a permitted variation, not an implementation promise.
- A paragraph explicitly labeled **Current** describes the audited implementation.
- A paragraph labeled **Example** illustrates a contract; it does not add a new rule or advertise an executable API.
- Tables labeled **Target contract** are normative semantic shapes. They are not generated API documentation.

Requirements have stable identifiers such as `ARCH-001` or `WRITE-003`.
A bold identifier defines a requirement exactly once in its owning document.
Other documents reference that identifier rather than redefine it.
Associated acceptance families are cataloged in document 13.
Requirement identifiers remain stable across prose edits; retired identifiers are not reused for different guarantees.

## Conformance levels

| Level | Meaning |
| --- | --- |
| Foundation | Storage/query contracts usable without Vegapunk or a model |
| Toolkit baseline | Reusable processing/controller machinery with deterministic test implementations |
| Vegapunk baseline | Conservative product composition with tested adapters |
| Experimental capability | Optional module with declared limitations and isolated state |
| Supported capability | Module whose required correctness and workload-specific quality gates have passed |

A release MUST declare which levels and optional capabilities it supports.
An unsupported capability MUST be absent or return an explicit unsupported result; it must not silently degrade into a different operation.
Experiments are not required for baseline use.
A baseline release does not claim graph, learned policies, background extraction, or SDK support merely because these are specified here.

## Interpretation and conflict resolution

**GOV-001 — One source of architectural authority.** This specification is authoritative for the agreed target. Existing code establishes what currently happens; it does not override a target requirement or justify an unsupported completion claim.

**GOV-002 — Named ownership.** Each cross-cutting contract has one owning document. Consumers reference it. Transport-specific details may narrow presentation but cannot weaken foundation invariants.

**GOV-003 — Explicit design changes.** A consequential change to data identity, authorization, deletion, transaction guarantees, context authority, or evaluation protocol requires updating the owning requirement, impact notes, and acceptance cases together.

**GOV-004 — Honest status.** Implementers MUST update the transition register with implementation and verification evidence before changing a capability from planned to supported. Compilation, a successful fixture, and a successful deployment prove different things.

**GOV-005 — Bounded documentation.** All documentation content inside `docs/` MUST be Markdown in `docs/spec/`, recursively, and the aggregate MUST remain at or below 25,000 lines. This is a ceiling, not a length goal. Keep executable fixtures, logs, binaries, and generated API artifacts outside this tree.

Specification approval does not authorize destructive operations on existing stores.
A source-control history of retired documentation is sufficient historical provenance; duplicate legacy docs are not retained beside this specification.
Runtime skills and package READMEs outside `docs/` remain useful entry points, but must reference these contracts rather than establish competing architecture.

## What this specification deliberately does not freeze

The following are controlled implementation or experimental choices:

- Provider/model selection and trained parameters.
- Ranking thresholds, candidate budgets, and graph expansion settings beyond required bounds.
- Default promotion of experimental modules.
- Additional crate splits beyond the required dependency direction.
- A particular performance envelope before measurement.

The following are fixed architectural decisions:

- Rust-library-first implementation of all domain behavior.
- Model-free foundation, optional model-assisted toolkit.
- SurrealDB as the supported structured substrate; CAS for artifact bytes.
- Revision/evidence integrity and scope/permission enforcement.
- Separation of retention, endorsement, retrieval, and actual insertion.
- Vegapunk as a composition rather than a second persistence implementation.
- Correctness-first incremental transition, not a data-reset rewrite.

## Specification verification

Document 13 defines the checks for this tree, including local links, unique requirement definitions, acceptance coverage, source-disposition coverage, and code-example labels.
A documentation-only change MUST NOT be described as passing new runtime acceptance tests unless those tests actually ran.
Historical benchmark rows remain historical under their original scoring protocols.

The initial publication of this specification records the architectural decisions and audited gaps; it does not implement the new toolkit crate, transactions, controller, worker, or graph writer.
