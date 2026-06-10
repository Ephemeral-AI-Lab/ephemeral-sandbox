# EOS Agent Core Rust to TypeScript Migration - Phase 04.5 Agent Runtime

Status: Proposed
Date: 2026-06-10
Owner: eos-agent-core
Migration direction: Rust -> TypeScript
Project path: `/Users/yifanxu/machine_learning/LoVC/EphemeralOS/eos-agent-core`
Rust source boundary: `agent-core/crates/eos-agent-run` (run lifecycle,
launcher), `agent-core/crates/eos-engine/src/background` (per-run session
runtime ownership)
Depends on: Phase 04 (`@eos/tool`, engine seams), Phase 03 (`@eos/engine`),
Phase 02 (`@eos/contracts`, `@eos/llm-client`)

## 1. Intent

Phase 04.5 introduces `@eos/agent-runtime`: the composition root where
process-level dependencies and per-run engine objects meet. It owns:

- `AgentRuntime.startRun()` - the per-run assembly: notification inbox,
  background supervisor (both engine classes, constructed per run),
  tool executor, engine `startAgentRun` - in one wiring order, in one file;
  this is the start method for main, subagent, and advisor runs,
- the run registry (typed map of active runs; mints `AgentRunId`),
- the agent tool runtime calls: `run_subagent` and `ask_advisor` call
  `startRun`, while `read_agent_run_transcript` reads the JSONL path
  recorded for a run,
- the per-run JSONL transcript writer - the artifact hooks read
  (`transcript_path`) and `read_agent_run_transcript` serves,
- the event broadcaster over the engine's single-consumer stream,
- hook config loading (`.eos-agents/hooks.json`).

Phase 04.5 does not add provider backends, external execution backends, or
other orchestration backends. The runtime accepts the already-built tool
definitions it is given, adds the agent-run tools it owns, and runs the
engine against fakes in its integration suite.

This phase is additive (one stub rename). The Rust implementation remains
live; nothing under `agent-core/` changes.

## 2. Design Decisions

1. **One pair per run.** The notification inbox and supervisor (both
   engine classes, Phase 04 §2.12) are constructed here per agent run,
   never shared: notifications target exactly one conversation,
   `liveCount()` backs exactly one run's submission guard, and disposal
   must not touch a sibling run's sessions. Subagent and advisor runs get
   their own pair via the same factory; the caller's subagent
   `SessionHandle` just watches that run's outcome.
2. **The wiring order is the spec.** inbox -> supervisor -> run state ->
   runtime-owned tool definitions -> `buildToolExecutor` -> engine start
   -> registry-settle subscription. Each arrow is a real dependency; the
   order lives in one function so neither `@eos/engine` nor `@eos/tool`
   ever learns process topology. Session teardown is engine-owned (Phase
   04 §2.17): this root wires none of it.
3. **Two lifetimes, one boundary.** Process-level dependencies (LLM client,
   agent profile store, caller-provided base tool definitions, hook config)
   are bound at `createAgentRuntime`; everything per-run is built in
   `startRun`. The runtime is the only layer that holds both.
4. **The transcript JSONL is the one cross-cutting artifact.** Hooks read
   it (`transcript_path`), `read_agent_run_transcript` serves it by byte
   offset, and Phase 04's notification design assumes it exists. It is
   written by the runtime's own event subscriber - not by the engine, not
   by tools.
5. **Event broadcasting is a runtime adapter, not an engine change.**
   Phase 03's event stream is deliberately single-consumer; the runtime is
   that single consumer and re-broadcasts to its subscribers (transcript
   writer always; caller subscribers optionally). Backpressure remains a
   server-phase concern.
6. **Subagent and advisor execution are just `startRun`.** `startRun` is
   `agent_name` driven, not a separate spawn helper and not
   `AgentKind`-parameterized. `run_subagent` passes its requested
   `agent_name` to `startRun(...)`, registers `handle.outcome.then(...)`
   with the background supervisor, and returns immediately. `ask_advisor`
   calls `startRun(...)` with `agent_name: "advisor"`, awaits
   `handle.outcome`, and returns the advisor submission as the tool
   result. No second execution path exists.
7. **The public runtime vocabulary is agent name.** `AgentKind` stays as
   profile data (`agent_kind: main | planner | worker | subagent |
   advisor`) and as a derived run fact, but callers do not pass it to
   `startRun`.
8. **Profiles resolve before engine start.** `startRun` loads the
   named agent profile from the runtime's `AgentProfileRegistry` before
   constructing the engine input. Profile data contributes the agent kind,
   LLM client id, system prompt, max turns, allowed tools, and explicit
   terminal tool. The runtime resolves `llm_client_id` through
   `.eos-agents/llm_clients.json` to get the configured client, auth
   source, model id, and reasoning effort. Tool selection is assembled by
   the runtime from that resolved profile and available tool definitions before
   `startAgentRun`; the engine receives only a resolved `systemPrompt`,
   LLM client, model id, reasoning effort, turn limit, and `ToolExecutor`.
9. **Initial messages are ordered user messages.** `initialMessages` is a
   non-empty list because some call sites need separable user messages
   (for example transcript-as-evidence, then an instruction about that
   transcript). The system prompt stays a request field, never a message.
   The runtime accepts `Message` values, not raw prompt strings, so callers
   make message boundaries explicit.
10. **`signal` is lifecycle input, not prompt data.** The optional
    `AbortSignal` belongs on `startRun` because cancellation is owned by
    the caller's lifecycle: a UI stop button, server request abort,
    caller-run disposal, or an `ask_advisor` tool cancellation can all
    terminate the run without changing its prompts, profile, or tools. The
    runtime passes this signal to `startAgentRun`; while the process is
    alive, the handle resolves a `cancelled` `AgentRunOutcome`.

## 3. Scope

In scope:

- keep the package at `packages/agent-runtime`
  (`@eos/agent-runtime`; the stub is package.json-only at phase start),
- `AgentRuntime` (`createAgentRuntime`, `startRun`), run registry,
- `AgentProfileRegistry` + profile loader for `.eos-agents/profile/*.md`,
- `LlmClientRegistry` + config loader for `.eos-agents/llm_clients.json`,
- agent tool runtime calls, transcript writer + reader, event broadcaster,
- hook config loading with Zod validation,
- disposal and caller-run cancellation,
- the §13 integration suite over `MockLlmClient` + in-process fakes.

Out of scope (named seams in §10):

- external execution backends and backend-specific tools,
- persistence beyond the transcript JSONL (`@eos/db` records, resume),
- server transports, observability wiring, run-level authn/quotas,
- compaction, scheduling/admission control.

## 4. Composition Root (`runtime.ts`)

```ts
interface AgentRuntimeDependencies {
  agentProfilesDir?: string;               // default: .eos-agents/profile
  llmClientsPath?: string;                 // default: .eos-agents/llm_clients.json
  llmClients?: LlmClientRegistry;          // optional in-memory test override
  baseTools?: readonly ToolDefinition[];   // optional process-level tools
  hookConfigPath?: string;                 // default: .eos-agents/hooks.json
  dataDir: string;                         // transcript root
}

type AgentName = string;
type UserMessage = Message & { role: "user" };

interface StartRunParams {
  agent_name: AgentName;
  initialMessages: readonly [UserMessage, ...UserMessage[]];
  signal?: AbortSignal;                    // caller cancellation scope
}

interface StartedRun {
  run_id: AgentRunId;
  handle: AgentRunHandle;                  // steer / interrupt / outcome
  subscribe(): AsyncIterable<AgentEvent>;  // event-broadcaster tap (§6)
  transcript_path: string;
}
```

`createAgentRuntime` loads agent profiles at startup:

```ts
function createAgentRuntime(dependencies: AgentRuntimeDependencies): AgentRuntime {
  const agent_profile_registry = loadAgentProfileRegistry(
    dependencies.agentProfilesDir ?? ".eos-agents/profile",
  );
  const llm_client_registry =
    dependencies.llmClients ??
    loadLlmClientRegistry(
      dependencies.llmClientsPath ?? ".eos-agents/llm_clients.json",
    );
  const hookEngine = buildHookEngine(loadHookConfig(dependencies.hookConfigPath));
  return createRuntime({
    ...dependencies,
    agent_profile_registry,
    llm_client_registry,
    hookEngine,
  });
}
```

`agent-profile-loader.ts` parses one Markdown file into frontmatter plus
body. `agent-profile-registry.ts` loads the directory once, validates every
profile with Zod, rejects duplicate `name` values, and exposes lookup by
`agent_name` only:

```ts
interface AgentProfile {
  name: AgentName;
  description: string;
  llm_client_id: string;
  max_turns: number;
  agent_kind: AgentKind;
  allowed_tools: readonly ToolName[];
  terminal_tool: ToolName;
  systemPrompt: string;                    // Markdown body after frontmatter
  source_path: string;                     // diagnostics only, never API input
}

interface AgentProfileRegistry {
  require(agent_name: AgentName): AgentProfile;
  list(): readonly AgentProfile[];
}
```

Profile file format:

```md
---
name: worker
description: Worker
llm_client_id: codex_coding_plan
max_turns: 100
agent_kind: worker
allowed_tools:
  - read
  - multi_read
  - write
  - edit
  - exec_command
  - command_stdin
  - read_command_transcript
  - list_background_sessions
  - cancel_background_session
  - ask_advisor
terminal_tool: submit_worker_outcome
---

You are the worker for one assigned work item.

Complete only the `<work_item>` in your context. Treat `<needs>` as fixed
direct dependency outcomes. If delegated workflow tools are available and a
subtask needs decomposition, you may delegate it, then inspect or cancel all
outstanding workflow handles before your terminal submission.

Before terminal submission, call `ask_advisor` with
`tool_name="submit_worker_outcome"` and the exact payload you intend to
send.
```

`allowed_tools` names ordinary non-terminal tools to expose. `terminal_tool`
names exactly one terminal tool to expose, separate from the allowlist. The
runtime validates that every `allowed_tools` entry resolves to a non-terminal
definition, `terminal_tool` resolves to exactly one terminal definition, and
the terminal name is not also listed under `allowed_tools`. The loader does
not infer ordinary tools from prose: if the profile body tells the agent to
call `ask_advisor`, `allowed_tools` must include `ask_advisor`.
`terminalToolDefinitions(supervisor)` is a name-keyed inventory of terminal
definitions; it is not keyed by `AgentKind`, and the profile selects the
terminal by `terminal_tool`.

`llm-client-registry.ts` loads `.eos-agents/llm_clients.json`, validates it
with Zod, and exposes lookup by `llm_client_id`:

```ts
interface LlmClientBinding {
  id: string;
  provider: ProviderConnection["provider"];
  model_id: string;
  reasoning_effort: ReasoningEffort;
  client: LlmClient;
}

interface LlmClientRegistry {
  require(llm_client_id: string): LlmClientBinding;
  list(): readonly LlmClientBinding[];
}
```

Config file format:

```json
{
  "clients": [
    {
      "id": "codex_coding_plan",
      "provider": "codex_coding_plan",
      "model_id": "gpt-5.5",
      "reasoning_effort": "medium",
      "base_url": "https://chatgpt.com/backend-api/codex",
      "auth": {
        "kind": "codex_cli_auth_file",
        "path": "/Users/yifanxu/.codex/auth.json"
      }
    }
  ]
}
```

The Codex loader mirrors
`packages/llm-client/e2e/support/codex-auth.ts`: read
`tokens.access_token` from the configured auth file, validate the JWT has
the ChatGPT account claim and is not expired, wrap it in `SecretString`,
then call `createLlmClient({ provider: "codex_coding_plan", base_url,
access_token })`. The token is never written back to
`.eos-agents/llm_clients.json`.

`startRun` implementation sketch:

```ts
interface StartRunContext {
  caller_run_id?: AgentRunId;              // internal only, never public input
}

function startRun(params: StartRunParams, context: StartRunContext = {}): StartedRun {
  const profile = agent_profile_registry.require(params.agent_name);
  const llm = llm_client_registry.require(profile.llm_client_id);
  if (profile.agent_kind === "main" && context.caller_run_id !== undefined) {
    throw new TypeError("main profiles can only be started externally");
  }

  const run_id = mintAgentRunId();
  const inbox = new NotificationInbox();
  const supervisor = new BackgroundSupervisor(inbox);
  const transcript_path = transcriptPathFor(dependencies.dataDir, run_id);

  const runState = createAgentRunState({
    run_id,
    kind: profile.agent_kind,
    caller_run_id: context.caller_run_id,
    agent_name: profile.name,
    transcript_path,
  });
  registry.add(runState);

  const availableDefinitions = [
    ...(dependencies.baseTools ?? []),
    ...agentTools(
      {
        startRun: (next) => startRun(next, { caller_run_id: run_id }),
        resolveTranscriptPath: (target) => registry.transcriptPath(target),
        readTranscriptFile,
      },
      supervisor,
    ),
    ...backgroundTools(supervisor),
    ...terminalToolDefinitions(supervisor),
  ];
  const definitions = selectProfileDefinitions(profile, availableDefinitions);

  const tools = buildToolExecutor({
    runState,
    definitions,
    inbox,
    hookEngine,
  });

  const handle = startAgentRun({
    llmClient: llm.client,
    tools,
    notifications: inbox,
    background: supervisor,
    model: llm.model_id,
    reasoningEffort: llm.reasoning_effort,
    systemPrompt: profile.systemPrompt,
    maxTurns: profile.max_turns,
    signal: params.signal,
    initialMessages: [...params.initialMessages],
  });

  const broadcaster = createEventBroadcaster(handle.events);
  const transcriptWriter = new TranscriptWriter(transcript_path);
  broadcaster.subscribe((event) => transcriptWriter.append(event));
  registry.attach(run_id, handle);
  handle.outcome.finally(() => {
    void transcriptWriter.flush().finally(() => registry.settle(run_id));
  });

  return {
    run_id,
    handle,
    subscribe: () => broadcaster.subscribe(),
    transcript_path,
  };
}
```

`startRun` wiring order (decision 2):

```
1. profile = agent_profile_registry.require(agent_name)
2. llm = llm_client_registry.require(profile.llm_client_id)
3. run_id = mintAgentRunId()
4. inbox = new NotificationInbox()               // engine class
5. supervisor = new BackgroundSupervisor(inbox)  // engine class; self-
                                                 // subscribes for delivery
6. transcript_path = runs/<run_id>/transcript.jsonl
7. runState = createAgentRunState({ run_id,
     kind: profile.agent_kind,
     caller_run_id: internal caller_run_id,
     agent_name: profile.name, transcript_path }) // Phase 04 §2.19
   registry.add(runState)                        // facts stored once
8. availableDefinitions = [
     ...(dependencies.baseTools ?? []),
     ...agentTools({ startRun, resolveTranscriptPath, readTranscriptFile }, supervisor),
     ...backgroundTools(supervisor),
     ...terminalToolDefinitions(supervisor),
   ]
   definitions = selectProfileDefinitions(profile, availableDefinitions)
   tools = buildToolExecutor({ runState, definitions, inbox, hookEngine })
9. handle = startAgentRun({ llmClient: llm.client, tools, notifications: inbox,
     background: supervisor, systemPrompt: profile.systemPrompt,
     model: llm.model_id, reasoningEffort: llm.reasoning_effort,
     maxTurns: profile.max_turns,
     initialMessages, ... })
10. broadcaster = createEventBroadcaster(handle.events) // sole consumer
   broadcaster.subscribe(transcriptWriter)
11. handle.outcome.finally(() => registry.settle(run_id))
```

Session teardown needs no wiring here: the engine loop triggers
`supervisor.dispose(reason)` on every finish (Phase 04 §2.17), cancelling
stragglers through each start site's `SessionHandle`. Step 11 is pure
registry bookkeeping.

## 5. Run Registry and Agent Tool Runtime Calls (`registry.ts`)

The registry is one typed map: `Map<AgentRunId, { state: AgentRunState,
handle, status }>` - the run facts live exactly once, in the state record
(Phase 04 §2.19); the registry adds only what the record must not hold
(the live handle, the registry-level status). Terminal runs stay listed
until the caller-run session (if any) has observed their settlement;
transcript reads against finished runs must keep working.

Agent-family tools receive narrow bound functions, not a service object:

- `run_subagent` receives a start function that can call
  `startRun({ agent_name, initialMessages })`; the runtime registry
  resolves the name to a profile before engine start. It registers
  `handle.outcome.then(mapSubagentOutcome)` as the `SessionHandle.settled`
  promise and returns the session/run reference immediately.
- `ask_advisor` receives a start function that can call
  `startRun({ agent_name: "advisor", initialMessages, signal })`; it
  awaits the
  returned handle's outcome and maps the advisor submission to its tool
  result.
- `read_agent_run_transcript` resolves `run_id -> transcript_path` through
  a bound runtime lookup, then calls `readTranscriptFile(path, offset)`.
  The model never supplies a raw filesystem path.

Tool flow:

```ts
// main: external caller starts the primary run.
const main = runtime.startRun({
  agent_name: "root",
  initialMessages: [fromUserText(prompt)],
  signal,
});
return main;

// ask_advisor: synchronous tool result.
const advisor = startRun({
  agent_name: "advisor",
  initialMessages: [
    fromUserText(callerTranscript),
    fromUserText(
      "Read the transcript and verify if the caller submitted the payload correctly.",
    ),
  ],
  signal,
});
const advisorOutcome = await advisor.handle.outcome;
return mapAdvisorOutcome(advisorOutcome);

// run_subagent: background session.
const subagent = startRun({
  agent_name,
  initialMessages: [fromUserText(prompt)],
});
supervisor.register(
  { type: "subagent", id: subagent.run_id },
  toolUseId,
  {
    settled: subagent.handle.outcome.then(mapSubagentOutcome),
    cancel: async (reason) => {
      subagent.handle.interrupt(reason);
      await subagent.handle.outcome;
    },
  },
);
return { run_id: subagent.run_id };
```

## 6. Transcript Writer and Event Broadcaster (`transcript.ts`, `event-broadcaster.ts`)

`createEventBroadcaster(events)` consumes the engine stream once and
re-emits to N subscribers (push, per-subscriber buffer; a slow caller tap
never blocks the transcript writer). `subscribe()` after `run_finished`
replays nothing and completes immediately - `outcome` is the completion
surface, parity with Phase 03 §8.

`TranscriptWriter` appends one JSON line per conversation-shaping event to
`<dataDir>/runs/<run_id>/transcript.jsonl`:

```ts
type TranscriptLine =
  | { seq, ts, kind: 'user' | 'assistant'; message: Message }
  | { seq, ts, kind: 'tool_result'; result: ToolCallResult }
  | { seq, ts, kind: 'notification'; text: string }
  | { seq, ts, kind: 'run_finished'; outcome_status: string;
      submission?: JsonValue };
```

Writes go through one append queue per run (ordered, awaited before
`readTranscript` returns, flushed on `run_finished`). This file is the
`transcript_path` in every Phase 04 `HookPayload` and `ToolCallMeta` - the
hook-state story depends on it existing for every run, including
subagent/advisor runs.

## 7. Hook Config Loading (`hook-config.ts`)

`loadHookConfig(path)`: read `hookConfigPath` (default
`.eos-agents/hooks.json`), `safeParse` against the Phase 04
`HookConfigEntry[]` schema. Missing file -> `[]` (no hooks). Malformed file
-> startup error naming the Zod issues - config errors fail loudly at
`createAgentRuntime`, never silently mid-run. One `HookEngine` is built per
runtime and shared by all runs (hook commands are stateless processes; the
per-call payload carries all identity).

## 8. Disposal and Cancellation

| Trigger | Effect |
| --- | --- |
| run finishes (any status) | the ENGINE triggers `supervisor.dispose` (Phase 04 §2.17); the runtime only marks the registry terminal |
| caller run disposed with live subagent | the subagent `SessionHandle.cancel` -> `handle.interrupt('caller disposed')` -> that run's own engine dispose cascades |
| caller `signal` aborts | engine cancels (Phase 03 semantics) and disposes on finish |
| `cancel_background_session` on a subagent | same subagent interrupt path, model-initiated |

The cascade is depth-first through session handles; no global kill switch
exists - each run only ever touches sessions it registered.

## 9. Public API (`index.ts`)

```ts
function createAgentRuntime(dependencies: AgentRuntimeDependencies): AgentRuntime;

interface AgentRuntime {
  startRun(params: StartRunParams): StartedRun;
  getRun(runId: AgentRunId): StartedRun | undefined;
  listRuns(): ReadonlyArray<{ run_id, agent_kind, agent_name, status, caller_run_id? }>;
}
```

Identity stamping of events (Phase 03 §8 deferral) stays deferred: a
`StartedRun` tap serves one run; multiplexed envelopes belong to the server
phase.

## 10. Deferred (named seams)

| Deferred behavior | Seam left by this phase |
| --- | --- |
| Backend-specific tool families | `dependencies.baseTools` accepts already-built definitions |
| DB-backed run records / resume | `TranscriptLine` + `AgentRunOutcome` carry what a recorder needs |
| Server transport, multiplexed event envelopes | `StartedRun.subscribe()` per run |
| Hook config hot-reload / per-project layering | `loadHookConfig` is one call site |
| Run admission control / concurrency budget | `startRun` is the single entry |

## 11. Workspace Changes

- `packages/agent-runtime/`; package name `@eos/agent-runtime`
  (`dependencies`: `@eos/contracts`, `@eos/engine`, `@eos/tool`,
  `@eos/llm-client` via `workspace:*`, plus `yaml` for profile
  frontmatter parsing). Phase 03 §11's runtime references resolve to this
  package.
- `packages/testkit/`: gains a scripted `MockLlmClient` scenario helper if
  the engine's double is promoted (second consumer now exists); otherwise
  the runtime suite keeps a local copy.
- `yaml` is the only new third-party dependency in this phase; all parsed
  profile data is still validated through Zod before registration.

Resulting layout:

```
packages/agent-runtime/
├─ src/
│  ├─ runtime.ts          createAgentRuntime() + startRun() §4 wiring
│  ├─ registry.ts         run map, AgentRunId minting, caller links
│  ├─ agent-profile-loader.ts frontmatter/body parser + Zod validation
│  ├─ agent-profile-registry.ts name-indexed registry
│  ├─ llm-client-registry.ts llm_clients.json loader + client factory binding
│  ├─ agent-tools.ts      bound runtime calls for subagent/advisor/transcript
│  ├─ transcript.ts       per-run JSONL writer + offset reader
│  ├─ event-broadcaster.ts single-consumer stream -> N subscribers
│  ├─ hook-config.ts      loadHookConfig()
│  └─ index.ts
├─ tests/                 §13 integration suite
└─ package.json           @eos/agent-runtime; deps: @eos/contracts,
                          @eos/engine, @eos/tool, @eos/llm-client
```

`@eos/agent-runtime` is the only package that depends on everything; the
workspace dependency graph stays acyclic with the composition root on top
(contracts <- engine <- tool <- testkit; agent-runtime consumes all).

## 12. Migration Steps

1. Verify the stub package -> verify: `pnpm install` + workspace resolution
   green.
2. Agent profile loader + registry -> verify: valid frontmatter/body
   profile loads by `agent_name`, duplicate names and malformed profiles
   fail at startup.
3. LLM client registry -> verify: `.eos-agents/llm_clients.json` loads,
   Codex CLI auth-file entries build `codex_coding_plan` clients, and
   missing `llm_client_id` references fail at startup.
4. Transcript writer + event broadcaster -> verify: ordered lines, offset
   reads, slow-tap isolation tests.
5. Registry + agent tool runtime calls (subagent start, advisor await,
   transcript read) -> verify: §13 cases 5-6.
6. Hook config loading -> verify: missing/valid/malformed cases and §13
   case 7.
7. `createAgentRuntime` + `startRun` wiring + disposal -> verify: §13
   cases 3-4, 7-8.
8. Workspace wiring -> verify: `pnpm run check` green.
9. Update the migration `index.md` row for this phase.

## 13. Verification

Integration suite over `MockLlmClient` scripts + in-process fakes; no
network, real files only under a temp `dataDir`.

| # | Case | Asserts |
| --- | --- | --- |
| 1 | Profile loader / registry | the worker-format Markdown profile loads by `agent_name`; duplicate `name`, missing `llm_client_id`, invalid `max_turns`, unknown `allowed_tools`, unknown/non-terminal `terminal_tool`, and `terminal_tool` duplicated under `allowed_tools` fail before any run starts |
| 2 | LLM client registry | `llm_clients.json` loads the Codex coding-plan entry, reads the configured Codex auth file without persisting the token, and rejects missing client ids referenced by profiles |
| 3 | Wiring order | a `startRun` smoke run produces transcript lines, drains a notification, and observes the engine-triggered dispose on finish (spy ordering matches §4) |
| 4 | Submission end-to-end | scripted main run calls `submit_main_outcome`; `outcome.submission` carries the payload; transcript `run_finished` line matches |
| 5 | Subagent round-trip | main starts a subagent run by `agent_name`, idles -> auto-wait, `session_settled` notification arrives, caller reads the subagent transcript via the tool, then submits |
| 6 | Advisor ask | `ask_advisor` blocks, advisor run submits, answer returns in the tool result; caller abort mid-ask cancels the advisor run |
| 7 | Disposal cascade | interrupting the caller cancels the live subagent run; both registries settle |
| 8 | Hook script over transcript | a real spawned node hook denies a call based on `transcript_path` contents (read-before-write style assertion) |
| 9 | Event broadcast isolation | transcript subscriber receives every event while a slow caller subscriber lags or returns early |

Commands:

```bash
cd /Users/yifanxu/machine_learning/LoVC/EphemeralOS/eos-agent-core
pnpm install
pnpm run check
```

- Rust boundary hygiene: `git diff --stat -- agent-core` stays empty.
- Docs hygiene: `git diff --check -- docs/plans/agent-core-rust-to-typescript-migration eos-agent-core`.

## 14. Coexistence and Rollback

- Coexistence: the Rust implementation remains live; `@eos/agent-runtime`
  has no server or CLI consumer yet and is exercised only by its suite.
- Rollback: revert the rename, delete the package contents, drop the index
  row. Phases 02-04 are unaffected.

## 15. Acceptance Criteria

Phase 04.5 is accepted when:

- `@eos/agent-runtime` exposes exactly the §9 API and `startRun` performs
  the §4 wiring in order, with inbox/supervisor pairs strictly per-run,
- subagent and advisor execution is `startRun` recursion (no second path),
  with caller cancellation and the §8 disposal cascade covered by
  tests,
- every run (including subagent/advisor runs) has a readable JSONL
  transcript that hook scripts and `read_agent_run_transcript` consume by
  offset,
- hook config loads fail loudly at startup and absent config means no
  hooks,
- the §13 suite passes under `pnpm run check` with no network I/O,
- the Rust `agent-core/` tree is byte-for-byte unchanged,
- and the migration `index.md` lists Phase 04.5 with status and
  verification.

## 16. Progress Tracker

| Step | Status | Required proof |
| --- | --- | --- |
| Package rename | Done | workspace resolves `@eos/agent-runtime` |
| Agent profile loader + registry | Pending | §13 case 1 |
| LLM client registry | Pending | §13 case 2 |
| Transcript + event broadcaster | Pending | §13 writer/tap tests green |
| Registry + agent tool runtime calls | Pending | §13 cases 5-6 |
| Hook config loading | Pending | missing/valid/malformed cases and §13 case 8 |
| Composition root + disposal | Pending | §13 cases 3-4, 7, 9 |
| Workspace wiring | Pending | `pnpm run check` green; `git diff --stat -- agent-core` empty |
| Index updated | Pending | Phase 04.5 row in `index.md` |
