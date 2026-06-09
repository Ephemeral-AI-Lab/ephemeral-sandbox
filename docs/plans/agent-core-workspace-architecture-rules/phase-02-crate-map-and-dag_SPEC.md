# Phase 02 - Crate Map and Dependency DAG Spec

Status: Draft (revised 2026-06-09 — contract-floor, loader, and binary placements added)
Date: 2026-06-09
Owner: agent-core workspace integration

## Scope

This phase changes the workspace crate map and internal dependency graph. It:

- removes the misleading `*-ports` crates and folds their contracts into the
  crates that own the behavior, or into the shared `eos-types` floor,
- folds `eos-runtime` request wiring into `eos-agent-core`,
- folds generic config, agent definitions, audit, skills, plugin catalog, and
  message records into their real owners,
- sinks every cross-crate contract (trait ports, neutral LLM DTOs,
  agent-definition DTOs, the markdown frontmatter parser) into `eos-types` so the
  target DAG stays acyclic,
- splits the dissolved `eos-config` into owner-local structs plus a single home
  for the shared loader machinery,
- relocates the process/binary entry point out of `agent-core`,
- normalizes singular crate names, and makes the target ownership boundaries
  visible in `Cargo.toml`.

Most folds and renames move files and update imports. Three target edges are
**not** pure renames and are called out under
[Edges that require migration, not a move](#edges-that-require-migration-not-a-move):
the `eos-tool` LLM-DTO sink, the `eos-agent-ports` contract split, and the
`eos-workflow` tool-rendering edge. Their behavior-preserving inversions land in
Phases 3–5; this phase establishes the crate boundaries and the contract
placements those inversions require.

## Lock reconciliation (read first)

Phase 00 (the accepted lock) freezes the 10-crate map, vocabulary, ownership, and
parallel lanes, but originally did **not** contain a per-crate dependency DAG.
This spec is the authoritative DAG and has been ratified back into Phase 00
before the destructive lanes start.

Two prior elaborations disagreed: `index.md`'s mermaid drew
`Workflow --> AgentRun`, while this DAG has no such edge.
**Resolution: `eos-workflow` does not depend on `eos-agent-run`.** Workflow spawns
runs only through an injected `AgentRunApi` port defined in `eos-types`; the
concrete run lifecycle is wired at the `eos-agent-core` composition root. The
`index.md` mermaid now records `Workflow --> Tool` and `Workflow --> Types`
instead.

Phase 00's retired-crate list previously named `eos-agent-api`, which never
existed; the real retired crate is `eos-agent-ports`, and the lock plus guard now
use only the real crate name.

## Local Architecture

Target crate topology (10 crates):

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
| `eos-runtime` (lib half) | `eos-agent-core/src/runtime/` | fold request runtime wiring into the facade; rename `runtime_services/` → `runtime/` (banned vocab) |
| `eos-runtime` (bin half) | `backend-server` (external) | `main.rs`, `entry.rs`, `observability.rs` leave `agent-core`; the facade stays a lib |
| `eos-agent-ports` | split (see contract floor) | `AgentRunApi` + spawn/outcome DTOs → `eos-types`; metadata/state contracts → `eos-types`; nothing lands in `agent-run`/`agent-core` that a lower crate consumes |
| `eos-tool-ports` | `eos-tool` + `eos-types` | model/registry/executor/hooks → `eos-tool`; the `AgentRunApi`-style and `WorkflowApi` contracts it re-exported → `eos-types` |
| `eos-agent-message-records` | `eos-engine/src/records.rs` | fold record writer/reader into engine-owned records internals |
| `eos-tools` | `eos-tool` | rename; concrete tool collapse executed in Phase 03 |
| `eos-agent-runner` | `eos-agent-run` | rename lifecycle crate; implements `eos-types::AgentRunApi` |
| `eos-skills` | `eos-tool/src/tools/skills.rs` | fold skill registry and skill package loading into tool ownership |
| `eos-plugin-catalog` | `eos-agent-core/src/runtime/plugins.rs` | fold plugin package catalog into the composition root that consumes it (decision committed; no longer "or eos-tool") |
| `eos-agent-def` | DTOs → `eos-types/src/agent.rs`; loader → `eos-agent-core/src/agents.rs` | passive definitions are shared, so they sink to types; only filesystem loading/validation stays in the facade |
| `eos-config` | structs → owners; loader → split | see [Config and loader disposition](#config-and-loader-disposition) |
| `eos-audit` | `eos-agent-core/src/runtime/audit.rs` | file sink impl in the facade; `AuditSink` trait + audit DTOs → `eos-types` if any engine/run code emits audit |

## Target Dependency DAG

```text
eos-types            (contract floor; no internal upstream edge)
eos-sandbox-port  -> eos-types
eos-llm-client    -> eos-types
eos-db            -> eos-types
eos-tool          -> eos-types, eos-sandbox-port
eos-engine        -> eos-types, eos-tool, eos-llm-client, eos-sandbox-port
eos-workflow      -> eos-types, eos-tool
eos-agent-run     -> eos-types, eos-engine
eos-agent-core    -> eos-types, eos-db, eos-llm-client, eos-sandbox-port,
                     eos-tool, eos-engine, eos-workflow, eos-agent-run
eos-testkit       -> eos-types, eos-engine, eos-agent-run, eos-llm-client,
                     eos-sandbox-port, eos-tool   (dev-only)
```

No target crate depends on a retired crate. No cycles: every cross-crate contract
is consumed from `eos-types`, which has no internal upstream edge.

Changes vs the prior draft DAG:

- `eos-tool`: prior draft said `-> types, sandbox-port` while the code imports
  `eos-llm-client` (`Message`, `ContentBlock`, `MessageRole`, `ToolSpec`). The
  edge is now honest because those neutral DTOs sink to `eos-types`; there is no
  `tool -> llm-client` edge.
- `eos-workflow`: prior draft said `-> types` only; the code renders tool
  instructions via `eos_tool::render_tool_instruction` in `context`. The DAG now
  records `-> types, tool`. Phase 05 may invert this to types-only by injecting a
  renderer port.
- `eos-agent-run`: `-> types, engine` (unchanged). `eos-workflow` does not depend
  on it.

## Contract floor — what sinks into `eos-types`

`eos-types` is the only crate with no internal upstream edge, so every contract
shared across sibling crates must live here or a cycle forms. Acyclicity, not
preference, forces these placements:

| Contract | From | Why it must be in `eos-types` |
| --- | --- | --- |
| `AgentRunApi` + spawn/outcome/status/error DTOs | `eos-agent-ports` | `eos-engine` background manager consumes `dyn AgentRunApi`; engine cannot depend on `agent-run`/`agent-core` |
| `WorkflowApi` (was `workflow_api.rs`) | `eos-types` (rename) | consumed by tool + engine; implemented by workflow |
| persistence store traits (`AgentRunStore`, …) | `eos-types/ports/` → `stores.rs` | drops banned `port` vocab |
| neutral LLM DTOs: `Message`, `ContentBlock`, `MessageRole`, `ToolSpec` | `eos-llm-client` | consumed by tool, engine, records, testkit; sinking them severs `tool -> llm-client` and `records -> llm-client` |
| agent DTOs: `AgentName`, `AgentDefinition`, `AgentType`, read-only `AgentRegistry` + in-memory builder | `eos-agent-def` | consumed by workflow + tool + agent-run; none can reach the facade |
| `parse_markdown_frontmatter` (pure parser) | `eos-config/markdown.rs` | shared by tool (skills) and the agent-def/plugin loaders; pure, no I/O |
| `AuditSink` trait + audit event/node DTOs | `eos-audit` | only if engine/run code emits audit; the file sink impl stays in the facade |

`eos-types` stays behavior-free: no `load()`, no filesystem registry builder, no
provider encoders, no I/O. A passive in-memory `AgentRegistryBuilder` is allowed
only to assemble already-loaded `AgentDefinition` values. The `*Api` trait type
names are tolerated as external-contract language; only the *module* names
`workflow_api.rs` / `agent_run_api.rs` are banned and are replaced by
`contracts.rs`.

### Agent type launch classes

`AgentType` is the only launch/dispatch axis on the agent profile. There is no
separate `AgentRole`: a run's workflow role is the `TaskRole` on its lineage row
(`root`, `planner`, `generator`, `reducer`), and a parented run's launch class is
the `ParentedRunKind` on its run row (`subagent`, `advisor`); neither is a field
on the profile. The target `AgentType` values are:

| `AgentType` | Launcher | Required runner rule |
| --- | --- | --- |
| `agent` | root and workflow launches | task-owned runs (root/planner/generator/reducer) require `agent` |
| `subagent` | `run_subagent` | subagent runs require `subagent` |
| `advisor` | `ask_advisor` | advisor runs require `advisor` |

There is no generic standalone `agent` launch: every top-level run of a request
is a root task run, so `agent` covers root and workflow roles only.

The advisor profile is therefore `agent_type: advisor` with no role field. This
removes the current profile-name convention where advisor is encoded as a plain
`agent`, and lets `eos-agent-run` validate advisor launches the same way it
validates subagent launches.

The subagent class is the generic read-only worker launch class, not an
explorer-specific class. The bundled profile is named `subagent`, and its
terminal is `submit_subagent_result`; focused exploration is only one prompt a
caller may give that general subagent.

Behavior that previously keyed off `AgentDefinition.role` re-anchors on the
task's `TaskRole` and the profile's `AgentType`: the planner's
generator-capability check and the context-builder/context-recipe selection read
the `TaskRole` the agent is admitted to, not a workflow role pinned on the
profile. An `agent`-type profile is therefore not bound to a single workflow
role.

## Edges that require migration, not a move

These three are flagged so the integration lane does not treat them as renames:

1. **`eos-tool` → no `eos-llm-client`.** Requires the neutral LLM DTO sink above.
   Until it lands, `eos-tool` will not build against the target DAG.
2. **`eos-agent-ports` split.** Acyclicity dictates per-symbol homes: anything
   `eos-engine`, `eos-tool`, or `eos-workflow` consumes goes to `eos-types`;
   only facade-private wiring lands in `eos-agent-core`. There is no symbol that
   may land in `eos-agent-run` while a lower crate still consumes it.
3. **`eos-workflow` tool rendering.** `context` calls a concrete
   `eos-tool` function. This phase records the honest `workflow -> tool` edge;
   the optional inversion to a types-level renderer port is Phase 05.

## Config and loader disposition

Dissolving `eos-config` must place the shared loader machinery, not only the
section structs. The structs scatter to owners; the machinery splits by nature:

| Item | Target | Note |
| --- | --- | --- |
| `DatabaseConfig`, `DatabaseUrl`, `ModelsConfig`, `ModelRegistrationConfig` | `eos-db/src/config.rs` | db already deserializes these |
| `ProvidersConfig`, `RetryConfig`, provider api configs | `eos-llm-client/src/config.rs` | |
| `WorkflowConfig` | `eos-workflow` | |
| `RuntimeConfig`, `AttemptConfig` | `eos-agent-core/src/runtime/config.rs` | |
| passive shared config DTO (only if unavoidable) | `eos-types` | |
| `parse_markdown_frontmatter` (pure) | `eos-types/src/frontmatter.rs` | shared by tool/skills and the agent-def/plugin loaders with no mid-DAG config edge |
| `load()` / `load_with_override()` / `ConfigDocument` (file merge, I/O) | `eos-agent-core/src/runtime/config.rs` | startup composition; the facade reads files and hands typed sections to each crate |

There is no generic final `eos-config` crate and no replacement loader crate.

## Binary entry point

`eos-runtime` is the current binary crate. Its process concerns leave
`agent-core`:

- `main.rs`, `entry.rs` (process bootstrap), `observability.rs` (tracing init),
  and HTTP routing belong to the external `backend-server`, which depends on
  `eos-agent-core` as a library.
- `entry.rs`'s `run_request` / `RequestOutcome` become the public facade API on
  `eos-agent-core::lib`.
- `eos-agent-core` ships as a library only; no `main.rs` under `agent-core`.

## Ownership Rules

- `eos-agent-core` is the external-project facade and owns private request
  runtime wiring, the audit file sink, the plugin catalog, the agent-definition
  loader, and the config file-merge loader.
- `eos-agent-run` owns run lifecycle and implements `eos-types::AgentRunApi`; it
  validates `AgentType` launch classes against the requested record kind and
  depends on engine services, not engine internals.
- `eos-engine` owns execution and depends on `eos-tool` for tool framework
  contracts; it consumes `dyn AgentRunApi` / `dyn WorkflowApi` from `eos-types`,
  never the concrete run/workflow crates.
- `eos-tool` owns tool registry, executor trait, hooks, concrete tool behavior,
  skill loading, and tool runtime resources; LLM DTOs and tool contracts it
  shares come from `eos-types`.
- `eos-workflow` owns workflow lifecycle and sibling-facing workflow services;
  it renders tool instructions via `eos-tool` and spawns runs via an injected
  `AgentRunApi` port.
- `eos-llm-client` owns outbound provider clients and provider config; it uses
  `client` / `providers` / `stream`, not `services`, and no longer owns the
  neutral transcript DTOs.
- `eos-types` owns passive DTOs, store traits, cross-cutting trait ports, the
  neutral LLM DTOs, the agent-definition DTOs (including `AgentType::Advisor`),
  and the pure frontmatter parser. It holds no behavior or I/O.
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

Crate-level structure plus the modules this phase establishes. File-level
collapses of the concrete tool tree, the engine internals, and the deep
`eos-types/state` tree are executed in Phases 3–5; their target homes are shown
here. Legend: `new`, `from <retired crate>`, `renamed`, `out` (moved to another
crate).

```text
agent-core/crates/
├── eos-types/                       # contract floor (~16 modules; see budget note)
│   └── src/
│       ├── lib.rs · error.rs · ids.rs · json.rs · time.rs
│       ├── frontmatter.rs           # new   from eos-config::parse_markdown_frontmatter (pure)
│       ├── llm.rs                   # new   from eos-llm-client: Message/ContentBlock/MessageRole/ToolSpec
│       ├── agent.rs                 # new   from eos-agent-def: AgentName/Definition/Type + read-only AgentRegistry
│       ├── stores.rs                # renamed from ports/ persistence traits
│       ├── contracts.rs             # AgentRunApi (from eos-agent-ports) + WorkflowApi (was workflow_api.rs)
│       ├── state.rs
│       └── state/{engine,runtime,workflow,tools,model_registry}.rs
├── eos-sandbox-port/                # unchanged (only allowed port crate)
│   └── src/{lib,error,gateway,ops,provision,timeouts,transport,command_service}.rs
│       └── models/… · tool_api/…
├── eos-llm-client/                  # pure provider leaf
│   └── src/
│       ├── lib.rs · error.rs
│       ├── config.rs                # new   from eos-config provider sections
│       ├── model.rs                 # provider-wire DTOs; neutral DTOs moved out to eos-types
│       ├── client.rs                # client + auth + retry
│       ├── stream.rs                # sse + events
│       ├── providers.rs
│       └── providers/{anthropic,openai}.rs
├── eos-db/
│   └── src/
│       ├── lib.rs · error.rs · pool.rs · json_col.rs · rows.rs · model_registry.rs
│       ├── config.rs                # new   from eos-config db/model sections
│       ├── database.rs              # renamed from composition.rs (banned vocab)
│       ├── repositories.rs
│       └── repositories/{agent_run,attempt,iteration,request_task,workflow}.rs
├── eos-tool/                        # from eos-tools + eos-tool-ports + eos-skills (Phase 03 collapse)
│   └── src/
│       ├── lib.rs · error.rs · model.rs · registry.rs · hooks.rs · tools.rs
│       └── tools/{sandbox,command,isolated_workspace,subagent,workflow,submission,advisor,skills,terminal}.rs
├── eos-engine/                      # execution only; from eos-engine + eos-agent-message-records (Phase 04)
│   └── src/
│       ├── lib.rs · error.rs · model.rs · events.rs · printer.rs · services.rs · query.rs
│       ├── records.rs               # from eos-agent-message-records
│       ├── agent_loop.rs
│       ├── agent_loop/{executor,state,turn}.rs
│       ├── background.rs
│       └── background/{command_sessions,subagent_sessions,workflow_sessions,notification}.rs
├── eos-workflow/
│   └── src/
│       └── {lib,error,model,services,attempts,planning,iterations,context}.rs
├── eos-agent-run/                   # renamed from eos-agent-runner; implements AgentRunApi
│   └── src/
│       └── {lib,error,model,services,request,active_runs,persistence,completion}.rs
├── eos-agent-core/                  # facade + hidden runtime; from eos-runtime(lib)+audit+agent-def(loader)+plugin-catalog+config(loader)
│   └── src/
│       ├── lib.rs                   # public facade API (was eos-runtime/entry.rs)
│       ├── error.rs · model.rs · request.rs · state.rs · cancellation.rs
│       ├── facade.rs                # renamed from agent_core.rs (crate-name stutter)
│       ├── agents.rs                # from eos-agent-def loader/validation (DTOs are in eos-types)
│       ├── runtime.rs
│       └── runtime/
│           ├── builder.rs · database.rs · engine.rs · sandbox.rs   # renamed from runtime_services/
│           ├── audit.rs             # from eos-audit (sink impl; AuditSink trait is in eos-types)
│           ├── plugins.rs           # from eos-plugin-catalog
│           └── config.rs            # from eos-config loader + RuntimeConfig/AttemptConfig
└── eos-testkit/                     # dev-only; edges retargeted to types/engine/agent-run/llm-client/sandbox-port/tool
```

The process binary (`main.rs`, `observability.rs`, routing) lives in the external
`backend-server`, outside this workspace.

## Module Budget Note

Sinking the contract floor into `eos-types` raises it above the prior `<= 12`
sub-budget; revise the `eos-types` ceiling to `<= 16` (offset by the genuine
collapses in tool/engine/workflow). The `<= 220` staged total for this phase is
reached only via the in-fold merges named above (for example
`eos-runtime` 21 modules → `runtime/` 6 modules) — folds move files into target
crates where they are re-counted, so a pure move would land near 270. Treat the
listed folds as merge-and-move, not move-only; otherwise the staged budget is not
met. Wide-but-passive `eos-types` is acceptable because it is behavior-free; the
budget guard must confirm no logic lands there.

## Progress Tracker

| Item | Status |
| --- | --- |
| Ratify this DAG + contract floor into Phase 00 lock | Done (2026-06-09) |
| Add target crate names to workspace guard | Done (2026-06-09; stale `eos-agent-api` retired alias removed) |
| Sink LLM/agent/contract DTOs + frontmatter parser into `eos-types` | Done (2026-06-09; `AgentType::Advisor`, neutral LLM DTOs, `AgentRunApi` contracts, `WorkflowApi`, and pure frontmatter parser now live in `eos-types`) |
| Remove agent-profile role axis (`AgentRole`, `AgentDefinition.role`, and `role:` frontmatter) | Done (2026-06-09; workflow lineage now uses `TaskRole`, profiles expose `AgentType` only) |
| Generalize explorer-specific subagent naming | Done (2026-06-09; profile/tool naming is `subagent` + `submit_subagent_result`, with advisor kept as a sibling `AgentType`) |
| Fold `eos-runtime` lib into `eos-agent-core/src/runtime/`; relocate bin to backend-server | Not started |
| Rename `eos-tools` → `eos-tool`; rename `eos-agent-runner` → `eos-agent-run` | Done (2026-06-09; both crates/packages/imports renamed to the locked singular names) |
| Fold `eos-tool-ports` into `eos-tool` (+ contracts to `eos-types`) | Done (2026-06-09; executable tool framework/registry types live in `eos-tool`, shared cancellation/agent/workflow contracts live in `eos-types`, engine notifications are engine-local, and the crate was removed from the active workspace) |
| Split `eos-agent-ports` per the contract floor | Done (2026-06-09; agent-run lifecycle DTOs and `AgentState` live in `eos-types`, agent-loop launcher/outcome contracts live in `eos-engine`, runtime keeps the concrete execution-metadata adapter, and the crate was removed from the active workspace) |
| Fold `eos-agent-message-records` into `eos-engine/src/records.rs` | Done (2026-06-09; crate removed from workspace, implementation/test moved under `eos-engine::records`, runner imports through `eos-engine`) |
| Fold `eos-skills` into `eos-tool/src/tools/skills.rs` | Done (2026-06-09; folded into `eos-tool::tools::skills`) |
| Fold `eos-plugin-catalog` into `eos-agent-core/src/runtime/plugins.rs` | Done (2026-06-09; folded into current `eos-runtime::plugins` pending the separate `eos-runtime` → `eos-agent-core` fold) |
| Fold `eos-agent-def`: DTOs → `eos-types`, loader → `eos-agent-core/src/agents.rs` | Done (2026-06-09; DTOs/passive registry live in `eos-types`, loader/validation moved into the current `eos-runtime::agents` facade staging module, bundled-profile loader coverage moved with it, and the standalone crate was removed from the active workspace) |
| Dissolve `eos-config`: structs to owners, parser → types, loader → facade | In progress (2026-06-09; pure parser moved, config structs/loader still local) |
| Fold `eos-audit`: sink → facade, `AuditSink` trait → `eos-types` | Not started |
| Update workspace dependencies and internal imports | In progress (2026-06-09; staged imports updated for completed sinks, DTO direct-import cleanup, message-record fold, skills fold, plugin-catalog fold, agent-definition loader fold, `eos-tool` rename, `eos-agent-run` rename, and full `eos-tool-ports`/`eos-agent-ports` retirement from the active workspace) |
| Update dependency DAG guard to the target edge set | Done (2026-06-09; staged legacy graph remains active until the final crate map is present) |
| Update `index.md` Progress Tracker with Phase 02 result and exit artifact | Not started |

## Acceptance Criteria

- `agent-core/Cargo.toml` contains no retired crate members.
- No target crate imports `eos-runtime`, `eos-agent-ports`, `eos-tool-ports`, or
  `eos-agent-message-records`.
- No target crate imports `eos-config`, `eos-agent-def`, `eos-audit`,
  `eos-skills`, or `eos-plugin-catalog`.
- `eos-tool` has no `eos-llm-client` dependency; `eos-llm-client` no longer
  exports the neutral transcript DTOs (they resolve from `eos-types`).
- `eos-workflow` depends on `eos-types` and `eos-tool` only; it has no
  `eos-agent-run` edge.
- `eos-engine` consumes `AgentRunApi` and `WorkflowApi` from `eos-types`, with no
  edge to `eos-agent-run` or `eos-workflow`.
- The shared frontmatter parser resolves from `eos-types`; the config file loader
  resolves from `eos-agent-core/src/runtime/config.rs`.
- Agent profiles and `AgentDefinition` expose `AgentType` only. There is no
  `AgentRole` enum, no `AgentDefinition.role`, and no `role:` frontmatter field;
  planner/generator/reducer/root are `TaskRole` lineage coordinates.
- Active subagent profile/tool names are generic, not explorer-specific:
  `subagent` + `submit_subagent_result`; advisor remains a separate sibling
  `AgentType`, not a subagent specialization.
- `eos-agent-core` ships as a library with no `main.rs`; the process binary lives
  in `backend-server`.
- The internal DAG guard passes with the target edge set; `crate_inventory` and
  `dependency_dag` guards pass.
- `cargo check --workspace --all-targets` compiles for the new crate map.
- Module count is at or below the staged phase-2 budget of 220, with
  `eos-types` at or below its revised ceiling of 16.
- Plugin catalog ownership is resolved at `eos-agent-core/src/runtime/plugins.rs`
  without a standalone `eos-plugin-catalog` crate.
