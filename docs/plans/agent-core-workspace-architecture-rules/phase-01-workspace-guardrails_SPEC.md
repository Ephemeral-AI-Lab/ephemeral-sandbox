# Phase 01 - Workspace Guardrails Spec

Status: Draft
Date: 2026-06-09
Owner: workspace-guard

## Scope

This phase creates executable architecture rules before destructive refactors
start. The rules live in `agent-core/workspace-guard/tests` and fail CI when a
future change reintroduces the patterns this cleanup removes.

No product behavior changes are allowed in this phase.

## Local Architecture

`workspace-guard` remains a test-only crate. The crate has no production library
logic; integration tests parse Cargo metadata and source paths.

```text
agent-core/workspace-guard/
├── Cargo.toml
├── src/
│   └── lib.rs
└── tests/
    ├── dependency_dag.rs
    ├── profiles.rs
    ├── crate_inventory.rs
    ├── crate_layout.rs
    ├── naming_rules.rs
    ├── service_boundaries.rs
    ├── public_surface.rs
    └── module_budget.rs
```

## Rule Set

| Rule file | Responsibility |
| --- | --- |
| `crate_inventory.rs` | exact allowed crate set and retired crate names |
| `crate_layout.rs` | banned folders, thin `lib.rs`, no `mod.rs` maze |
| `naming_rules.rs` | `api`, `service`, `port`, forbidden vocabulary |
| `service_boundaries.rs` | every service has a sibling-crate reference |
| `public_surface.rs` | accidental `pub mod` and public export drift |
| `module_budget.rs` | total and per-crate module ceilings |
| `dependency_dag.rs` | target internal dependency graph |

## Target Checks

### Crate Inventory

Allowed target crates:

```text
eos-agent-core
eos-agent-run
eos-engine
eos-tool
eos-workflow
eos-types
eos-db
eos-llm-client
eos-sandbox-port
eos-testkit
```

Retired names:

```text
eos-runtime
eos-agent-ports
eos-tool-ports
eos-agent-message-records
eos-tools
eos-agent-runner
eos-skills
eos-plugin-catalog
eos-agent-def
eos-config
eos-audit
```

`eos-plugin-catalog` may be temporarily allowlisted during migration only if
Phase 0 records the reason and the final fold target.

### Naming

```text
port:
  allowed crate: eos-sandbox-port
  banned elsewhere: crate names, module names, public type names

api:
  banned as a crate/module suffix unless Phase 0 explicitly allows an external transport adapter
  allowed in prose for external contract descriptions only

service:
  allowed only when a different workspace crate imports or calls it
  banned for private resource bags and agent-core-local runtime wiring

runtime:
  allowed as hidden wiring under eos-agent-core/src/runtime.rs and runtime/

client:
  allowed for outbound external clients such as eos-llm-client

composition:
  banned

deps:
  banned

runtime_services:
  banned
```

### Service Boundary

For every `service.rs`, `services.rs`, `services/`, `*Service`, or `*Services`:

1. find the owning crate,
2. find the public export path,
3. scan other workspace crates for references,
4. fail if there is no sibling-crate consumer,
5. report the suggested replacement word.

Suggested replacements:

| Bad use | Replacement |
| --- | --- |
| private executor resource bag | `Handles` |
| per-call immutable facts | `Context` or `Metadata` |
| facade-local runtime object graph | `Runtime` |
| record writer/reader internals | `Records` |
| event callback/printing internals | `Printer` or `Sink` |
| registry definitions | `Registry`; use `Catalog` only for loaded definitions with lifecycle |
| outbound provider implementation | `Client` |
| default tool specs | `ToolRegistry` or `tools.rs`, not `Catalog` |

### Module Budget

Initial budget gates:

| Gate | Limit |
| --- | ---: |
| total modules after phase 2 | <= 220 |
| total modules after phase 4 | <= 190 |
| final total modules | <= 170 |
| `eos-agent-core` final modules | <= 22 |
| `eos-tool` final modules | <= 16 |
| `eos-engine` final modules | <= 22 |
| `eos-workflow` final modules | <= 10 |
| `eos-types` final modules | <= 12 |

The final goal is 150-170 modules. The guard may use staged allowlists while
the refactor is in progress, but the final check must not allow 291 modules or
a final count above 170.

## Resulting File Structure

```text
agent-core/workspace-guard/tests/
├── dependency_dag.rs
├── profiles.rs
├── crate_inventory.rs
├── crate_layout.rs
├── naming_rules.rs
├── service_boundaries.rs
├── public_surface.rs
└── module_budget.rs
```

## Progress Tracker

| Item | Status |
| --- | --- |
| Add target crate inventory test | Not started |
| Add retired crate name failures | Not started |
| Add `port` naming guard | Not started |
| Add crate/module `api` naming guard | Not started |
| Add strict service sibling-use guard | Not started |
| Add forbidden `composition` / `deps` / `runtime_services` guard | Not started |
| Add `eos-agent-core/src/runtime` allowance | Not started |
| Add module budget guard | Not started |
| Add public surface drift guard | Not started |
| Wire guard command into CI/docs | Not started |

## Acceptance Criteria

- `cargo test -p workspace-guard` runs from `agent-core`.
- Guard failures explain the exact path, symbol, and rule violated.
- The guard can distinguish same-crate references from sibling-crate references.
- The guard bans `eos-runtime`, `eos-agent-ports`, `eos-tool-ports`,
  `eos-agent-message-records`, `eos-tools`, `eos-agent-runner`, `eos-skills`,
  `eos-agent-def`, `eos-config`, and `eos-audit` after the crate collapse
  phase.
- The guard bans `composition`, `deps`, and `runtime_services`.
- The guard supports temporary staged budgets but has a documented final budget
  of 150-170 modules.
- No production crate behavior changes in this phase.
