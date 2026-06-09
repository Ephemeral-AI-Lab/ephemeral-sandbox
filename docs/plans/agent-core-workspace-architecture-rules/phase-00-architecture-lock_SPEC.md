# Phase 00 - Architecture Lock Spec

Status: Accepted - amended for backend-facing server boundary
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

Amendment record: Phase 05 replaced the original `eos-agent-core` facade target
with the backend-facing `eos-agent-core-server` crate. Backend-server is now the
production composition root for concrete stores, sandbox host, and engine
launcher wiring. Phase 06 also adds an executable final-layout budget gate while
keeping the default guard run advisory during staged cleanup.

## Locked Decisions

| Decision | Target |
| --- | --- |
| external facade crate | `eos-agent-core-server` |
| HTTP/path router | outside `agent-core`; belongs in `backend-server` |
| removed runtime crate | `eos-runtime` is retired; request lifecycle lives in `eos-agent-core-server`, loop lifecycle in `eos-agent-run` / `eos-engine`, and concrete wiring in `backend-server` |
| removed generic config crate | config structs live with their owning crate; file loading is backend composition-owned |
| removed agent definition crate | passive DTOs live in `eos-types`; filesystem loading/validation is backend composition-owned |
| removed audit crate | backend audit/observability contracts and persistence live in `eos-backend-audit`; `agent-core` has no standalone audit crate |
| shared contract floor | cross-crate trait ports, neutral LLM DTOs, agent DTOs, store traits, and the pure frontmatter parser live in `eos-types` |
| only allowed port crate | `eos-sandbox-port` |
| service meaning | sibling-crate or backend-composition consumed callable surface |
| service replacement vocabulary | `runtime`, `handles`, `context`, `client`, `records` |
| runtime wiring vocabulary | `runtime` and `handles`; no standalone handle file unless it earns its size |
| folder structure guardrails | thin roots, source tests under `tests/`, no vague buckets, no duplicate module shape |
| forbidden vocabulary | `composition`, `deps`, `runtime_services` |
| final crate count | 10 |
| final module count | 150-170 |

## Final Crate Map

```text
agent-core/crates/
├── eos-agent-core-server/
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
eos-agent-core
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
eos-agent-run     -> eos-types
eos-agent-core-server -> eos-types, eos-agent-run, eos-sandbox-port
eos-testkit       -> eos-types, eos-engine, eos-llm-client,
                     eos-sandbox-port, eos-tool   (dev-only)
```

`eos-workflow` has no crate edge to `eos-agent-run`; it starts runs through the
`AgentRunApi` contract from `eos-types`, and the concrete run lifecycle is wired
only at the backend composition root.

## Boundary Rules

### eos-agent-core-server

Owns the backend-facing request lifecycle service. Backend-server owns concrete
composition and HTTP routing.

```text
eos-agent-core-server/src/
├── lib.rs
├── dto.rs
├── error.rs
├── service.rs
├── user_request.rs
└── user_request/
    ├── create.rs
    ├── cancel.rs
    ├── finalizer.rs
    └── query.rs
```

Does not own HTTP routing, concrete store construction, agent/profile file
loading, audit persistence, or domain logic owned by engine, tool, workflow, run
lifecycle, DB, or sandbox crates.

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
| agent profile and definition loading | backend composition |
| workflow config | `eos-workflow` |
| DB config | `eos-db` |
| passive shared config DTO, only if unavoidable | `eos-types` |
| pure markdown frontmatter parser | `eos-types/src/frontmatter.rs` |
| file-merge config loader | backend composition |
| audit and observability contracts/persistence | `eos-backend-audit` |

### eos-llm-client

Owns outbound provider clients and provider-wire DTOs. It uses `client.rs`,
`providers.rs`, and `stream.rs`, not `services.rs`; neutral transcript DTOs
shared with tool, engine, records, and testkit live in `eos-types`.

### eos-types

Owns passive shared contracts: typed IDs, state DTOs, store traits,
`AgentRunApi`, `WorkflowApi`, neutral LLM DTOs, agent-definition DTOs, and the
pure markdown frontmatter parser. It has no
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
| `service` | restricted | only sibling-crate or backend-composition consumed callable surfaces |
| `context` | allowed | per-call immutable facts |
| `records` | allowed | persisted record surfaces |
| `runtime` | restricted | request-running wiring belongs to backend composition or owner-local runtime types |
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
| Agent core/server/workflow/types | `eos-agent-core-server`, `eos-workflow`, `eos-types` | Phase 0 accepted |
| Integration | root `Cargo.toml`, dependency DAG, public exports | after lane contracts are drafted |

Only the integration lane should edit root `Cargo.toml`, shared workspace
dependencies, or cross-crate public re-export surfaces during the destructive
move.

## Progress Tracker

| Item | Status |
| --- | --- |
| Approve backend-facing `eos-agent-core-server` over router/API crate names | Approved |
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

- The target facade crate is `eos-agent-core-server`.
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
