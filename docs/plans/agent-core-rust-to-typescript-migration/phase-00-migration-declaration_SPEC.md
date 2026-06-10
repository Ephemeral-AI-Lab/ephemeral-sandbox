# EOS Agent Core Rust to TypeScript Migration - Phase 00 Declaration

Status: Proposed
Date: 2026-06-10
Owner: eos-agent-core
Migration direction: Rust -> TypeScript
Project path: `/Users/yifanxu/machine_learning/LoVC/EphemeralOS/eos-agent-core`

## 1. Intent

This spec declares a migration program for moving the current Rust `agent-core/`
implementation into the TypeScript project at
`/Users/yifanxu/machine_learning/LoVC/EphemeralOS/eos-agent-core`.

Phase 00 is a planning declaration only. It creates the spec boundary and names
the migration direction. It does not authorize source moves, runtime rewrites,
Cargo removal, API shape changes, or test rewrites by itself.

## 2. Scope

In scope:

- `agent-core/` runtime entry and request orchestration
- workflow lifecycle and attempt scheduling
- engine/query loop and tool-call execution
- model/provider client integration
- sandbox host API boundary used by agent-core
- config, audit, skills, plugin catalog, and persistence contracts owned by
  agent-core

Out of scope for this declaration:

- `sandbox/` daemon, wire protocol, command sessions, LayerStack, OCC, overlay,
  isolated workspace, plugin PPC, and terminal-pair implementation
- deleting Rust crates or Cargo workspace files
- introducing a TypeScript package layout before a phase spec defines it
- changing user-facing API contracts without compatibility and verification
  criteria

## 3. Migration Invariants

1. The live Rust implementation remains authoritative until replaced by a
   phase-specific TypeScript implementation with verification.
2. Each phase must name the Rust source surface being retired and the TypeScript
   target surface under `eos-agent-core/` replacing it.
3. Contract surfaces must be migrated with typed DTOs or schemas, golden tests,
   or compatibility checks where JSON, database, daemon, or audit behavior is
   externally visible.
4. EOS agent core and sandbox ownership must remain separate. Sandbox remains the
   Rust substrate unless a separate sandbox migration spec says otherwise.
5. No phase may use broad compatibility shims as a substitute for naming the
   actual ownership boundary being moved.

## 4. Recommended TypeScript Target Stack

Phase 00 records the initial stack recommendation for the TypeScript target. A
later phase must still introduce the package layout and dependency manifests
before any runtime code changes.

| Concern | Recommended library or primitive | Role in the migration |
| --- | --- | --- |
| Workspace layout | `pnpm` workspaces | Own future TypeScript package boundaries and shared dependency versions. |
| Runtime and language | Node.js TypeScript with strict compiler settings | Target implementation language and runtime for migrated eos-agent-core behavior. |
| Durable state | `better-sqlite3` + `Kysely` | Store agent requests, runs, attempts, events, heartbeats, terminal states, and migrations through typed SQL. |
| Runtime validation | Zod 4 | Validate request DTOs, tool inputs, provider payloads, persisted JSON blobs, and compatibility fixtures. |
| Test runner | Vitest | Cover TypeScript unit tests, state transition tests, migration fixtures, and focused integration tests. |
| Observability | OpenTelemetry JavaScript packages | Emit spans and events for request admission, agent runs, attempts, provider calls, tool calls, cancellation, and sandbox-host operations. |
| Structured logs | `pino` or the repo-selected structured logging wrapper | Provide JSON logs once the TypeScript runtime package exists. |
| Cancellation | native `AbortController` | Own active run cancellation trees, child task cancellation, and shutdown propagation. |
| Request context | native `AsyncLocalStorage` | Carry request, run, attempt, and actor context across async agent execution. |
| Bounded concurrency | `p-queue` or a small local limiter | Optional; use only where a phase defines request admission, provider, tool, or agent-run concurrency limits. |

The persistence model must keep durable run state separate from in-process
control handles:

| State class | Owner | Examples |
| --- | --- | --- |
| Durable SQL state | TypeScript persistence package | request rows, run rows, attempt rows, event rows, heartbeats, terminal states, final output metadata |
| In-process active state | TypeScript agent-run supervisor | abort controllers, task handles, stream waiters, live output subscriptions, wakeups |
| Reconciliation state | TypeScript supervisor or runtime entry | stale-running detection, owner-instance checks, cancellation finalization, crash recovery |

Terminal run updates must be compare-and-swap style transitions so only one path
can move a run from active to terminal. This is the critical invariant for
knowing that an agent loop exited when many requests and agent runs are active.

Phase 00 explicitly does not adopt BullMQ, Temporal, Redux, Zustand, XState,
Prisma, Drizzle, TypeORM, or Redis as defaults. Any later phase that introduces
one of those libraries must name the runtime requirement it satisfies and the
eos-agent-core boundary it owns.

## 5. Initial Phase Shape

Later phase specs should be added in this directory and should include:

- current Rust surface and owning crate/module,
- target TypeScript package/module,
- preserved public contracts,
- migration steps,
- rollback or coexistence rules,
- focused verification commands,
- and an index update requirement.

## 6. Acceptance Criteria

This declaration is accepted when:

- the migration spec folder exists under `docs/plans/`,
- the folder clearly declares `eos-agent-core/` as the TypeScript project path
  for the Rust `agent-core/` migration,
- the declaration records the recommended TypeScript third-party stack and
  lifecycle primitives,
- current live Rust behavior is explicitly unchanged by Phase 00,
- and this folder's `index.md` lists the declaration spec.
