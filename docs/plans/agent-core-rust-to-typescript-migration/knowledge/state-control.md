# Claude Code State Control

Status: Observed
Date: 2026-06-10
Source path: `/Users/yifanxu/machine_learning/LoVC/c c/src`
Migration context: `eos-agent-core/` TypeScript migration reference

## Store Core

A ~35-line custom store, no Redux/Zustand/XState
(`state/store.ts:1`):

```typescript
type Store<T> = {
  getState: () => T
  setState: (updater: (prev: T) => T) => void
  subscribe: (listener: Listener) => () => void
}
```

- `setState` takes an updater over fresh state; `Object.is(next, prev)`
  no-ops identical references; listeners notified synchronously; an optional
  `onChange({ newState, oldState })` callback fires per mutation.
- React reads through `useAppState(selector)` →
  `useSyncExternalStore(store.subscribe, get, get)`
  (`state/AppState.tsx:142-163`); selectors must return existing references
  so identity comparison suppresses re-renders. `useSetAppState()` returns a
  stable setter, so write-only components never re-render.
- Cross-cutting reactions (permission-mode change notifications, model
  override persistence) live in one `onChangeAppState` diff handler
  (`state/onChangeAppState.ts:65-112`), not scattered in components.

## State Layers

| Layer | Where | Mutability / discipline | Persistence |
| --- | --- | --- | --- |
| React-local UI state | components (`useState`) | local only | none |
| AppState store | `state/AppStateStore.ts:89-452` | immutable updates via `setState(updater)` | selected fields flow to transcript/settings |
| Module singletons | `bootstrap/state.ts` (sessionId), `history.ts` (prompt history buffer), `context.ts` (memoized system context), `utils/messageQueueManager.ts` (command queue) | set-once or owned-buffer with explicit reset functions | settings file, `history.jsonl` |
| Engine instance | `QueryEngine.ts:184-207`: `mutableMessages`, root `abortController`, `totalUsage`, `discoveredSkillNames` | mutable, single owner | transcript via `recordTranscript()` |
| Per-iteration loop state | `query.ts:204` `State` object | rebuilt each `continue` | none |
| Per-turn context | `Tool.ts:158` `ToolUseContext` | passed by reference into tools; tools return context *modifiers* rather than mutating | none |

`AppState` holds both DeepImmutable config-ish fields (settings, model,
permission context, UI panel state) and mutable runtime maps —
notably `tasks: Record<string, TaskState>`, `todos: { [agentId]: TodoList }`,
`mcp` (clients/tools/commands), `notifications`, `teamContext`, and
`speculation` (which deliberately uses mutable refs for message arrays to
avoid per-chunk spreading, `AppStateStore.ts:58-77`).

## ToolUseContext — the Per-Turn Capability Object

`ToolUseContext` (`Tool.ts:158-300`) is the single object threaded from the
loop into every tool. Key groups:

- `options`: model, tools list, MCP clients, agent definitions, query
  source, thinking config — read-only configuration.
- live handles: `abortController`, `readFileState` (file-read dedup LRU),
  `getAppState`/`setAppState` bridges into the store.
- UI callbacks: `setToolJSX`, `addNotification`, `setStreamMode`,
  `setResponseLength` — injected effects rather than imports.
- identity: `agentId`/`agentType` set only for subagents; main thread uses
  the session id.
- dedup/tracking sets: `loadedNestedMemoryPaths`, `discoveredSkillNames`,
  `setInProgressToolUseIDs`, `toolDecisions` (permission memo),
  `queryTracking` (chain id/depth across subagent spawns).

Tools never mutate the context directly; `runToolUse` yields
`contextModifier` functions that orchestration applies — immediately on the
serial path, post-batch on the concurrent path — keeping parallel tools on a
stable snapshot.

## Permission / Mode State

`AppState.toolPermissionContext` (`Tool.ts:123-148`) is a typed contract:
`mode: PermissionMode` (`default | plan | acceptEdits | bypassPermissions |
dontAsk | auto`), allow/deny/ask rule sets by source, working-directory
grants, and `prePlanMode` for restoring the previous mode on plan exit.
Mode changes go through normal `setAppState`, and the `onChange` diff
handler propagates them outward (remote session metadata, hooks).

## Conversation State and Persistence

- Canonical in-session history is `QueryEngine.mutableMessages`; the query
  loop receives a copy in `State.messages` and the engine appends what the
  loop yields.
- Persistence is append-mostly: `recordTranscript()`
  (`utils/sessionStorage.ts:1408`) dedups against the on-disk session
  message set and inserts only new messages as a parent-linked chain —
  retries/rewinds cannot double-write.
- Prompt history (`history.ts`) buffers entries in memory
  (`pendingEntries`) and flushes async to `~/.claude/history.jsonl`.
- Async update merging: long-running async computations return patches that
  are re-applied against fresh state (see task offset/eviction merging in
  `utils/task/framework.ts:158`), the repo-wide answer to lost-update races
  in a single-threaded but interleaved runtime.

## EOS Migration Takeaways

- A 35-line store with updater-based `setState`, sync listeners, and an
  `onChange` diff hook covers everything a UI-adjacent agent runtime needs;
  resist bigger state libraries.
- Put cross-cutting side effects in one old/new diff handler keyed on field
  comparisons.
- Thread one typed per-turn context object into tools; let tools return
  context modifiers instead of mutating shared state.
- Keep four layers explicit (durable, store, engine-instance, per-turn) and
  never let per-turn recovery state leak into module scope.
- For EphemeralOS specifically, the durable layer should be SQL-backed and
  authoritative (see `claude-code-tech-stack.md`); Claude Code's
  in-memory-first design is the main thing NOT to copy.

## Source Anchors

- Store: `/Users/yifanxu/machine_learning/LoVC/c c/src/state/store.ts:1`
- React hooks: `/Users/yifanxu/machine_learning/LoVC/c c/src/state/AppState.tsx:142`
- AppState shape: `/Users/yifanxu/machine_learning/LoVC/c c/src/state/AppStateStore.ts:89`
- Diff side effects: `/Users/yifanxu/machine_learning/LoVC/c c/src/state/onChangeAppState.ts:65`
- ToolUseContext: `/Users/yifanxu/machine_learning/LoVC/c c/src/Tool.ts:158`
- Permission context: `/Users/yifanxu/machine_learning/LoVC/c c/src/Tool.ts:123`
- Engine instance state: `/Users/yifanxu/machine_learning/LoVC/c c/src/QueryEngine.ts:184`
- Loop State: `/Users/yifanxu/machine_learning/LoVC/c c/src/query.ts:204`
- Transcript persistence: `/Users/yifanxu/machine_learning/LoVC/c c/src/utils/sessionStorage.ts:1408`
