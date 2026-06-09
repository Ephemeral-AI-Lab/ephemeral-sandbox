# Phase 01 - Workspace Guardrails Spec

Status: Implemented
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
    ├── crate_inventory.rs
    ├── crate_layout.rs
    ├── naming_rules.rs
    ├── service_boundaries.rs
    ├── public_surface.rs
    └── module_budget.rs
```

## Rule Set

| Rule file | Responsibility | Gate |
| --- | --- | --- |
| `crate_inventory.rs` | exact allowed crate set and retired crate names | hard |
| `crate_layout.rs` | thin crate roots, banned folders, source-test placement, no `mod.rs` maze or duplicate module shape | hard |
| `naming_rules.rs` | `api`, `service`, `port`, forbidden vocabulary | hard |
| `service_boundaries.rs` | every service has a sibling-crate reference | hard |
| `public_surface.rs` | accidental `pub mod` and public export drift | hard |
| `dependency_dag.rs` | target internal dependency graph | hard |
| `module_budget.rs` | reports total/per-crate module counts, max folder depth, root LOC, and the 170 ceiling | advisory |

Only seven rule files exist. `module_budget.rs` is **advisory**: it reports
counts and flags the strict 170 ceiling but never gates a merge, because file
count is a coarse proxy and cohesion outranks it (Phase 6). There is no
`profiles.rs` rule; do not add an undocumented guard test.

## Guard Command

Run from `agent-core`:

```bash
cargo test -p workspace-guard
```

No repository CI workflow is present in this checkout, so Phase 01 wires the
guard command into docs. A future CI workflow should run the same command before
crate-local checks.

## Staging Behavior

The guard starts from the current legacy workspace so Phase 01 can pass before
destructive crate moves begin. Once the target 10-crate map is present, final
checks activate for retired crate names, dependency DAG, naming rules, service
boundaries, layout, and public surface drift.

The staged mode still blocks undocumented new crates, keeps the live dependency
DAG and public surface from drifting silently, validates the guard file set, and
reports the advisory module budget.

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

### Crate Layout

Hard layout checks:

| Check | Rule |
| --- | --- |
| rule file inventory | `workspace-guard/tests` contains only the seven documented rule files |
| crate roots | `lib.rs`, `main.rs`, and root `mod.rs` stay under 200 nonblank lines |
| metadata library roots | each Cargo metadata library target stays under 200 nonblank lines |
| source tests | test modules live under the crate `tests/` tree, not `src/**/tests.rs` or `src/**/tests/` |
| duplicate module shape | final target crates may not contain both `foo.rs` and `foo/mod.rs` |
| `mod.rs` maze | final target crates may not use nested `mod.rs` routing |
| vague buckets | final target crates may not use folders named `common`, `helpers`, `shared`, or `utils` |
| architecture-smell folders | final target crates may not use folders named `api`, `services`, `ports`, `composition`, `deps`, or `runtime_services` |

The folder bans match exact folder names. They do not ban owner-specific names
such as `tool_api` when a phase spec explicitly keeps that contract. The staged
legacy workspace may still contain old module trees, but once the target
10-crate map is present these checks become hard.

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
5. report one canonical replacement word.

Suggested replacements:

| Bad use | Canonical replacement |
| --- | --- |
| facade-local object graph or executable wiring | `Runtime` |
| private executor resource bag | `Handles` |
| per-call immutable facts | `Context` |
| outbound external provider | `Client` |
| record writer/reader internals | `Records` |

The guard intentionally suggests only `Runtime`, `Handles`, `Context`,
`Client`, or `Records`. It is not a general naming dictionary. Domain-specific
names such as `registry.rs` or `printer.rs` may still appear where a phase spec
assigns that ownership, but they are not fallback replacements for private
`Service` names.

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

The advisory report also prints max source-folder depth and root file nonblank
LOC so reviewers can spot layout drift without turning file count into a merge
gate.

## Resulting File Structure

```text
agent-core/workspace-guard/tests/
├── dependency_dag.rs
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
| Add target crate inventory test | Done |
| Add retired crate name failures | Done |
| Add `port` naming guard | Done |
| Add crate/module `api` naming guard | Done |
| Add strict service sibling-use guard | Done |
| Add forbidden `composition` / `deps` / `runtime_services` guard | Done |
| Add `eos-agent-core/src/runtime` allowance | Done |
| Add thin crate root guard | Done |
| Add source-test placement guard | Done |
| Add duplicate module shape guard | Done |
| Add vague bucket folder guard | Done |
| Add module budget guard | Done |
| Add advisory folder-depth/root-LOC reporting | Done |
| Add public surface drift guard | Done |
| Wire guard command into CI/docs | Done (docs; no CI workflow present) |
| Update `index.md` Progress Tracker with Phase 01 result and exit artifact | Done |

## Acceptance Criteria

- `cargo test -p workspace-guard` runs from `agent-core`.
- Guard failures explain the exact path, symbol, and rule violated.
- The guard can distinguish same-crate references from sibling-crate references.
- The guard bans `eos-runtime`, `eos-agent-ports`, `eos-tool-ports`,
  `eos-agent-message-records`, `eos-tools`, `eos-agent-runner`, `eos-skills`,
  `eos-agent-def`, `eos-config`, and `eos-audit` after the crate collapse
  phase.
- The guard bans `composition`, `deps`, and `runtime_services`.
- The layout guard bans vague bucket folders, exact architecture-smell folders,
  duplicate `foo.rs` plus `foo/mod.rs` module shapes, and source-local test
  modules in the final target.
- Service failure messages suggest only `Runtime`, `Handles`, `Context`,
  `Client`, or `Records`.
- The guard supports temporary staged budgets but has a documented final budget
  of 150-170 modules.
- No production crate behavior changes in this phase.
