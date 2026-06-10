# Claude Code Tech Stack Notes

Status: Observed
Date: 2026-06-10
Source path: `/Users/yifanxu/machine_learning/LoVC/c c/src`
Migration context: `eos-agent-core/` TypeScript planning for the Rust
`agent-core/` migration

## Evidence Limits

The inspected Claude Code source tree appears to be extracted or transformed
TypeScript source, not a normal package checkout. No `package.json`, lockfile,
`tsconfig.json`, Vite config, or Vitest config was found within three directory
levels of `/Users/yifanxu/machine_learning/LoVC/c c`.

The notes below are source observations, not an authoritative dependency
manifest. Import counts are useful directional signals, but the source contains
generated or transformed code and some import-like strings in comments or prompt
text.

## Observed Stack

| Concern | Observed Claude Code pattern | Source signal |
| --- | --- | --- |
| Language and UI shell | TypeScript and TSX with React | `react` is the dominant bare import; many files are `.tsx`. |
| Transformed output | React compiler runtime | Many files import `react/compiler-runtime`. |
| Build/runtime feature gates | Bun bundle feature helper | `bun:bundle` is imported for feature checks. |
| State management | Custom store plus React `useSyncExternalStore` | `state/store.ts` defines `createStore`; `state/AppState.tsx` exposes app-state hooks. |
| Task model | Custom task types and statuses | `Task.ts` defines task types and `pending`, `running`, `completed`, `failed`, `killed`. |
| Task runtime | Custom registration, polling, output-offset tracking, notifications, and eviction | `utils/task/framework.ts` owns task registration and polling. |
| Cancellation | Native `AbortController` with helper wrappers | `utils/abortController.ts` configures listener limits and child abort propagation. |
| Async context | `AsyncLocalStorage` for in-process teammate isolation | In-process teammate comments and runtime code describe same-process agent isolation. |
| Tool validation and contracts | Zod 4 types | `Tool.ts` imports `zod/v4`. |
| Provider SDK | Anthropic SDK | Multiple imports from `@anthropic-ai/sdk`. |
| MCP integration | Model Context Protocol SDK | Multiple imports from `@modelcontextprotocol/sdk`. |
| Observability | OpenTelemetry packages and local tracing helpers | Imports include `@opentelemetry/*`; agent code also references Perfetto tracing helpers. |
| Utility libraries | `lodash-es`, `axios`, `chalk`, `figures`, `diff`, `execa`, `ws`, `chokidar`, `lru-cache`, `p-map` | These appear in the import survey. |

## Not Observed

No real imports were found for these candidate libraries in
`/Users/yifanxu/machine_learning/LoVC/c c/src`:

- `better-sqlite3`
- `Kysely`
- SQLite client libraries
- Drizzle
- Prisma
- TypeORM
- Knex
- BullMQ
- Temporal
- XState
- Zustand
- Redux
- `p-queue`
- `p-limit`

This does not mean Claude Code has no persistence or queueing elsewhere. It only
means those libraries were not observed in the inspected local source tree.

## Lifecycle Lessons

Claude Code's source points toward a small custom lifecycle framework rather
than a general-purpose state machine library:

1. App state is held in a small custom store with `getState`, `setState`, and
   `subscribe`.
2. React consumers subscribe to selected slices through `useSyncExternalStore`.
3. Tasks have explicit typed statuses and are stored in an app-state task map.
4. Task registration emits a start event, while each task type owns its own
   completion behavior.
5. Running task output is tracked through output offsets and disk-backed deltas.
6. Offset and eviction updates are merged against fresh state after async reads
   to avoid clobbering concurrent terminal transitions.
7. Runtime-only handles such as `AbortController`, cleanup callbacks, and
   per-turn controllers are kept separate from plain persisted or UI state.
8. In-process agents use async context for identity isolation instead of passing
   every field through every function.
9. UI mirrors are capped to avoid keeping duplicate full conversations in memory.

## EOS Agent Core Migration Takeaways

Use the Claude Code source as an implementation-shape reference, not as a
dependency list to copy.

For `eos-agent-core`, the useful patterns are:

- keep an explicit active-run supervisor instead of adopting Redux, Zustand, or
  XState for core agent state;
- use native `AbortController` trees for cancellation and shutdown propagation;
- use `AsyncLocalStorage` for request, run, attempt, actor, and provider context;
- keep UI or live-stream mirrors capped and separate from authoritative history;
- record events at lifecycle boundaries: request accepted, run spawned, attempt
  started, tool call started, provider call completed, cancellation requested,
  and terminal state committed;
- merge async output/progress updates against fresh state to avoid resurrecting
  stale running states.

The important difference for EphemeralOS is durability. Claude Code's inspected
source leans heavily on in-memory app state, runtime handles, and file-backed
output/transcript sidecars. `eos-agent-core` should make SQL state authoritative
for requests, runs, attempts, heartbeats, events, and terminal transitions. The
in-process supervisor should own only live handles such as abort controllers,
task promises, waiters, and stream subscriptions.

## Recommended EOS Shape

| State class | EOS owner | Reason |
| --- | --- | --- |
| Durable state | TypeScript persistence package using `better-sqlite3` + `Kysely` | Recoverable source of truth for requests, runs, attempts, events, heartbeats, terminal states, and final output metadata. |
| Active process state | TypeScript agent-run supervisor | Owns live cancellation, task handles, waiters, output streams, and wakeups that cannot be serialized. |
| Request context | `AsyncLocalStorage` context wrapper | Carries request/run/attempt/actor identity through provider, tool, and sandbox-host calls. |
| Cancellation | Native `AbortController` hierarchy | Propagates cancellation from user request to agent loop, tool calls, sandbox operations, and child agents. |
| Reconciliation | Runtime entry or supervisor loop | Detects stale running rows, missing owners, and cancelled-but-live runs after process restarts or crashes. |

Terminal run updates should be compare-and-swap style transitions. Only one path
may move a run from non-terminal to terminal, even if provider completion,
manual cancellation, process shutdown, and reconciliation race with each other.

## Source Anchors

- Custom store: `/Users/yifanxu/machine_learning/LoVC/c c/src/state/store.ts:1`
- App state hook: `/Users/yifanxu/machine_learning/LoVC/c c/src/state/AppState.tsx:126`
- Task types and statuses: `/Users/yifanxu/machine_learning/LoVC/c c/src/Task.ts:6`
- Task registration and polling: `/Users/yifanxu/machine_learning/LoVC/c c/src/utils/task/framework.ts:74`
- Fresh-state output offset merge: `/Users/yifanxu/machine_learning/LoVC/c c/src/utils/task/framework.ts:208`
- Abort controller helper: `/Users/yifanxu/machine_learning/LoVC/c c/src/utils/abortController.ts:16`
- In-process teammate isolation note: `/Users/yifanxu/machine_learning/LoVC/c c/src/tasks/InProcessTeammateTask/InProcessTeammateTask.tsx:1`
- Runtime-only teammate state fields: `/Users/yifanxu/machine_learning/LoVC/c c/src/tasks/InProcessTeammateTask/types.ts:22`
- UI message cap rationale: `/Users/yifanxu/machine_learning/LoVC/c c/src/tasks/InProcessTeammateTask/types.ts:89`
- In-process teammate spawn: `/Users/yifanxu/machine_learning/LoVC/c c/src/utils/swarm/spawnInProcess.ts:104`
- Tool context and Zod import: `/Users/yifanxu/machine_learning/LoVC/c c/src/Tool.ts:1`
- Agent runner: `/Users/yifanxu/machine_learning/LoVC/c c/src/tools/AgentTool/runAgent.ts:248`
