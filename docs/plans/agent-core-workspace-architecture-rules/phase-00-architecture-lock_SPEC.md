# Phase 00 - Architecture Lock Spec

Status: Accepted
Date: 2026-06-09
Owner: agent-core architecture

## Scope

This phase freezes the target architecture before implementation starts. No file
moves, crate renames, or code edits should begin until this spec is accepted.

Phase 0 exists because the cleanup is intentionally destructive. The target
crate map, naming vocabulary, service rule, module budget, and parallel work
lanes must be stable before agents start editing disjoint crates.

Approval record: accepted by user instruction on 2026-06-09. Any later change to
the target crate map, retired crate list, vocabulary rules, service rule,
module budget, or parallel work lanes must reopen Phase 0 before implementation
continues.

Amendment record: service replacement vocabulary and Rust folder-structure
guardrails tightened by user instruction on 2026-06-09. Phase 02's dependency
DAG and contract-floor placements were ratified back into this lock on
2026-06-09 before destructive crate moves began.

## Locked Decisions

| Decision | Target |
| --- | --- |
| external facade crate | `eos-agent-core` |
| HTTP/path router | outside `agent-core`; belongs in `backend-server` |
| removed runtime crate | `eos-runtime` folds into `eos-agent-core/src/runtime/` |
| removed generic config crate | config structs live with their owning crate; file loading lives in `eos-agent-core/src/runtime/config.rs` |
| removed agent definition crate | passive DTOs live in `eos-types`; filesystem loading/validation lives in `eos-agent-core/src/agents.rs` |
| removed audit crate | audit sink lives in `eos-agent-core/src/runtime/audit.rs`; shared audit contracts live in `eos-types` only when lower crates emit audit |
| shared contract floor | cross-crate trait ports, neutral LLM DTOs, agent DTOs, store traits, and the pure frontmatter parser live in `eos-types` |
| only allowed port crate | `eos-sandbox-port` |
| service meaning | sibling-crate consumed callable surface |
| service replacement vocabulary | `runtime`, `handles`, `context`, `client`, `records` |
| runtime wiring vocabulary | `runtime` and `handles`; no standalone handle file unless it earns its size |
| folder structure guardrails | thin roots, source tests under `tests/`, no vague buckets, no duplicate module shape |
| forbidden vocabulary | `composition`, `deps`, `runtime_services` |
| final crate count | 10 |
| final module count | 150-170 |

## Final Crate Map

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

Retired crates:

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

## Locked Dependency DAG

Phase 02 is the authoritative crate dependency DAG. It is ratified here so the
destructive lanes share one acyclic target:

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

`eos-workflow` has no crate edge to `eos-agent-run`; it starts runs through the
`AgentRunApi` contract from `eos-types`, and the concrete run lifecycle is wired
only at the `eos-agent-core` composition root.

## Boundary Rules

### eos-agent-core

Owns the user-facing Rust facade and hidden request runtime wiring.

```text
eos-agent-core/src/
├── lib.rs
├── error.rs
├── model.rs
├── agent_core.rs
├── request.rs
├── state.rs
├── cancellation.rs
├── agents.rs
├── runtime.rs
└── runtime/
    ├── builder.rs
    ├── database.rs
    ├── engine.rs
    ├── sandbox.rs
    ├── audit.rs
    └── plugins.rs
```

Does not own HTTP routing. Does not define domain logic owned by engine, tool,
workflow, run lifecycle, DB, or sandbox crates.

### eos-agent-run

Owns agent-run lifecycle: spawn, wait, poll, cancel, active runs, persistence,
completion handoff.

### eos-engine

Owns execution only: loop, turns, stream events, message records, midflight
printing, background accounting, and sibling-facing engine services.

### eos-tool

Owns the tool framework, concrete model-callable tools, hooks, registry, skill
loading, and runtime resources passed into tool execution.

Target source shape:

```text
eos-tool/src/
├── lib.rs
├── error.rs
├── model.rs
├── registry.rs
├── hooks.rs
├── tools.rs
├── tools/
│   ├── sandbox.rs
│   ├── command.rs
│   ├── workflow.rs
│   ├── subagent.rs
│   ├── submission.rs
│   ├── skills.rs
│   └── terminal.rs
```

`tools/` is concrete model-callable behavior. `hooks.rs` is tool pre/post
policy. `registry.rs` owns default tool registration, the executor trait, and
`ToolRuntime`; there is no first-target `catalog.rs`, `executor.rs`, or
`handles.rs`.

### eos-workflow

Owns workflow lifecycle and sibling-facing workflow services. It renders tool
instructions through `eos-tool` and starts runs through an injected
`eos-types::AgentRunApi`; it does not depend on the concrete `eos-agent-run`
crate.

### Config, Agent Definitions, and Audit

There is no standalone generic crate for these concerns in the final target.

| Concern | Owner |
| --- | --- |
| provider config | `eos-llm-client` |
| agent definition DTOs | `eos-types/src/agent.rs`; `AgentType` is the launch class `agent` / `subagent` / `advisor` |
| agent profile and definition loading | `eos-agent-core/src/agents.rs` |
| workflow config | `eos-workflow` |
| DB config | `eos-db` |
| passive shared config DTO, only if unavoidable | `eos-types` |
| pure markdown frontmatter parser | `eos-types/src/frontmatter.rs` |
| file-merge config loader | `eos-agent-core/src/runtime/config.rs` |
| runtime audit sink | `eos-agent-core/src/runtime/audit.rs` |

### eos-llm-client

Owns outbound provider clients and provider-wire DTOs. It uses `client.rs`,
`providers.rs`, and `stream.rs`, not `services.rs`; neutral transcript DTOs
shared with tool, engine, records, and testkit live in `eos-types`.

### eos-types

Owns passive shared contracts: typed IDs, state DTOs, store traits,
`AgentRunApi`, `WorkflowApi`, neutral LLM DTOs, agent-definition DTOs, audit
contracts when needed, and the pure markdown frontmatter parser. It has no
runtime, I/O, provider, DB, or service logic.

`AgentType` is the only profile launch/dispatch axis: `agent` is the normal
root/workflow class, `subagent` is launchable only through `run_subagent`, and
`advisor` is launchable only through `ask_advisor`. There is no `AgentRole` on
the profile; a run's workflow role is the `TaskRole` on its lineage row. There
is no generic standalone `agent` run. The advisor profile uses
`agent_type: advisor`.

## Vocabulary Rules

| Word | Status | Rule |
| --- | --- | --- |
| `api` | restricted | external contract language only; not the facade crate name |
| `router` | banned in agent-core | HTTP/path routing belongs in backend-server |
| `service` | restricted | only sibling-crate consumed callable surfaces |
| `context` | allowed | per-call immutable facts |
| `records` | allowed | persisted record surfaces |
| `runtime` | allowed | private request-running wiring inside `eos-agent-core` |
| `handles` | allowed | grouped concrete resource handles; avoid extra handle modules by default |
| `catalog` | restricted | loaded/static definitions with lifecycle, not default tool specs |
| `sink` | allowed | write-only event/audit output |
| `client` | allowed | outbound external clients |
| `port` | restricted | only `eos-sandbox-port` |
| `composition` | banned | too vague and visually noisy |
| `deps` | banned | implementation leakage |
| `runtime_services` | banned | old mixed naming |

## Parallel Work Lanes

| Lane | Scope | Can run after |
| --- | --- | --- |
| Guardrails | `workspace-guard` tests | Phase 0 accepted |
| Tool | `eos-tool` and folded tool/skill crates | Phase 0 accepted |
| Engine/run | `eos-engine`, `eos-agent-run`, message records | Phase 0 accepted |
| Agent core/workflow/types | `eos-agent-core`, `eos-workflow`, `eos-types` | Phase 0 accepted |
| Integration | root `Cargo.toml`, dependency DAG, public exports | after lane contracts are drafted |

Only the integration lane should edit root `Cargo.toml`, shared workspace
dependencies, or cross-crate public re-export surfaces during the destructive
move.

## Progress Tracker

| Item | Status |
| --- | --- |
| Approve `eos-agent-core` over `eos-agent-api` / router | Approved |
| Approve final 10-crate map | Approved |
| Approve owner-local config / agent definition / audit folds | Approved |
| Approve retired crate list | Approved |
| Ratify Phase 02 dependency DAG and contract floor | Approved |
| Approve vocabulary rules | Approved |
| Approve service sibling-use rule | Approved |
| Approve canonical service replacement vocabulary | Approved |
| Approve Rust folder-structure guardrails | Approved |
| Approve module budget | Approved |
| Approve parallel work lanes | Approved |
| Approve verification ladder | Approved |
| Update `index.md` Progress Tracker with Phase 00 result and exit artifact | Approved |

## Acceptance Criteria

- The target facade crate is `eos-agent-core`.
- No target crate or module is named router.
- `eos-runtime` is not a target crate.
- `eos-workflow` has no target dependency edge to `eos-agent-run`; run spawning
  crosses `eos-types::AgentRunApi`.
- Shared cross-crate contracts resolve from `eos-types`, which has no internal
  upstream dependency edge.
- Agent profiles and `AgentDefinition` expose `AgentType` only; `AgentRole`,
  `AgentDefinition.role`, and `role:` agent-profile frontmatter are removed.
- `composition`, `deps`, and `runtime_services` are rejected vocabulary.
- The final crate map contains exactly 10 crates.
- Every target `services.rs` has a named sibling-crate behavior consumer.
- Private `Service` names are replaced only with the canonical vocabulary:
  `Runtime`, `Handles`, `Context`, `Client`, or `Records`.
- Final target crates keep thin roots, keep source tests under crate `tests/`,
  avoid vague bucket folders, and do not mix `foo.rs` with `foo/mod.rs`.
- `eos-tool` does not use `services.rs` or `handles.rs`; it uses `ToolRuntime`
  in `registry.rs`.
- No standalone `eos-config`, `eos-agent-def`, or `eos-audit` crate exists in
  the final target.
- The implementation phases may begin without reopening naming or ownership
  decisions.
