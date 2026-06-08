# Phase 02 - Crate Map and Dependency DAG Spec

Status: Draft
Date: 2026-06-09
Owner: agent-core workspace integration

## Scope

This phase changes the workspace crate map and internal dependency graph. It
removes misleading `ports` crates, folds `eos-runtime` into `eos-agent-core`,
folds generic config, agent definitions, and audit into their real owners,
normalizes singular crate names, and makes the target ownership boundaries
visible in `Cargo.toml`.

This phase may move files and update imports, but it should avoid changing
runtime behavior beyond what is required for the new crate boundaries to build.

## Local Architecture

Target crate topology:

```text
agent-core/crates/
├── eos-agent-core/
├── eos-agent-run/
├── eos-engine/
├── eos-tool/
├── eos-workflow/
├── eos-types/
├── eos-db/
├── eos-llm-client/
├── eos-sandbox-port/
└── eos-testkit/
```

## Crate Diff Table

| Current | Target | Action |
| --- | --- | --- |
| `eos-runtime` | `eos-agent-core/src/runtime/` | fold request runtime wiring into the external facade crate |
| `eos-agent-ports` | owners | split DTOs/contracts into `eos-agent-core`, `eos-agent-run`, `eos-engine`, and `eos-types` |
| `eos-tool-ports` | `eos-tool` | fold model, registry, executor trait, hooks, and tool runtime resources into `eos-tool` |
| `eos-agent-message-records` | `eos-engine` | fold record writer/reader into engine-owned records internals |
| `eos-tools` | `eos-tool` | rename and collapse plural concrete tool crate |
| `eos-agent-runner` | `eos-agent-run` | rename lifecycle crate |
| `eos-skills` | `eos-tool` | fold skill registry and skill package loading into tool ownership |
| `eos-plugin-catalog` | `eos-tool` or `eos-agent-core/runtime/plugins.rs` | fold plugin package catalog into the owner that actually consumes it |
| `eos-agent-def` | `eos-agent-core/src/agents.rs` plus `eos-types` if shared | fold agent definition loading into the facade/runtime owner |
| `eos-config` | owning crates | provider config to `eos-llm-client`, workflow config to `eos-workflow`, DB config to `eos-db`, agent config to `eos-agent-core` |
| `eos-audit` | `eos-agent-core/src/runtime/audit.rs` | fold runtime audit sink into the facade runtime |

## Target Dependency DAG

```text
eos-types
eos-db                -> eos-types
eos-llm-client        -> eos-types
eos-sandbox-port      -> eos-types
eos-tool              -> eos-types, eos-sandbox-port
eos-engine            -> eos-types, eos-tool, eos-llm-client, eos-sandbox-port
eos-workflow          -> eos-types
eos-agent-run         -> eos-types, eos-engine
eos-agent-core        -> eos-db, eos-engine, eos-workflow, eos-agent-run,
                         eos-tool, eos-sandbox-port, eos-types,
                         eos-llm-client
eos-testkit           -> dev-only test dependencies
```

No target crate depends on a retired crate.

## Ownership Rules

- `eos-agent-core` is the external-project facade and owns private request
  runtime wiring.
- `eos-agent-run` owns run lifecycle and depends on engine services, not engine
  internals.
- `eos-engine` owns execution and depends on `eos-tool` for tool framework
  contracts.
- `eos-tool` owns tool registry, executor trait, hooks, concrete tool behavior,
  skill loading, and tool runtime resources.
- `eos-workflow` owns workflow lifecycle and sibling-facing workflow services.
- `eos-llm-client` owns outbound provider clients and uses `client` /
  `providers`, not `services`.
- Config lives with the behavior owner. There is no generic final
  `eos-config` crate.
- Agent definitions live in `eos-agent-core/src/agents.rs`; only passive shared
  DTOs may move to `eos-types`.
- `eos-types` owns passive DTOs and store traits only.
- `eos-sandbox-port` remains the only port-named crate.

## Resulting Workspace Manifest Shape

```toml
[workspace]
members = [
  "crates/eos-types",
  "crates/eos-db",
  "crates/eos-llm-client",
  "crates/eos-sandbox-port",
  "crates/eos-tool",
  "crates/eos-engine",
  "crates/eos-workflow",
  "crates/eos-agent-run",
  "crates/eos-agent-core",
  "crates/eos-testkit",
  "workspace-guard",
]
```

## Resulting File Structure

```text
agent-core/crates/
├── eos-agent-core/
├── eos-agent-run/
├── eos-engine/
├── eos-tool/
├── eos-workflow/
├── eos-types/
├── eos-db/
├── eos-llm-client/
├── eos-sandbox-port/
└── eos-testkit/
```

## Progress Tracker

| Item | Status |
| --- | --- |
| Add target crate names to workspace guard | Not started |
| Fold `eos-runtime` into `eos-agent-core/src/runtime/` | Not started |
| Rename `eos-tools` to `eos-tool` | Not started |
| Rename `eos-agent-runner` to `eos-agent-run` | Not started |
| Fold `eos-tool-ports` into `eos-tool` | Not started |
| Split/fold `eos-agent-ports` into owners | Not started |
| Fold `eos-agent-message-records` into engine | Not started |
| Fold `eos-skills` into `eos-tool` | Not started |
| Fold `eos-agent-def` into `eos-agent-core/src/agents.rs` | Not started |
| Fold `eos-config` into owning crates | Not started |
| Fold `eos-audit` into `eos-agent-core/src/runtime/audit.rs` | Not started |
| Update workspace dependencies | Not started |
| Update internal imports | Not started |
| Update dependency DAG guard | Not started |

## Acceptance Criteria

- `agent-core/Cargo.toml` contains no retired crate members.
- No target crate imports `eos-runtime`, `eos-agent-ports`,
  `eos-tool-ports`, or `eos-agent-message-records`.
- No target crate imports `eos-config`, `eos-agent-def`, or `eos-audit`.
- The internal DAG guard passes with the target edge set.
- `cargo check --workspace --all-targets` reaches type checking for the new
  crate map.
- Module count is at or below the staged phase-2 budget of 220.
- Plugin catalog ownership is resolved without a standalone `eos-plugin-catalog`
  crate.
