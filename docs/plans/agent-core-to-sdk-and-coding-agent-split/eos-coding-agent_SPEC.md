# eos-coding-agent - Host Application Specification

- **Status:** Draft for review
- **Date:** 2026-06-13
- **Depends on:**
  - `docs/plans/agent-core-to-sdk-and-coding-agent-split/eos-agent-sdk_SPEC.md`
  - `docs/plans/agent-core-rust-to-typescript-migration/phase-05.3-pursuit_leg_attempt_SPEC.md`
- **Scope:** The host application that composes `eos-agent-sdk` into the coding-agent
  product. It owns profiles, config files, all tools, the WorkflowHub, pursuit as the
  first configured workflow, advisor/subagent patterns, hooks, pursuit scripts, and the
  composition root.

## 1. Source-of-truth Alignment

This document is a split spec, not a second migration vocabulary. It must preserve two
source-of-truth decisions:

1. `eos-agent-sdk` is mechanism-only. It knows only agent, run, outcome, tool,
   background task, notification, and hook. It ships zero tools and no workflow concepts.
2. The active orchestration product vocabulary from Phase 05.3 is pursuit, leg, and
   attempt. The old product-facing workflow/iteration/focus/deferred/archive names must
   not return through this host split.

The hub layer uses exactly two nouns, and nothing else:

- A **workflow** is a named, configured `.eos-agents/workflow/<name>.md` file — the thing
  profiles list under `workflows`, the injected system-prompt fragment names, and the
  provider-authored `delegate_<name>` tool acts on. There is no separate "instance" term.
- A **workflow provider** is the compiled implementation of a workflow *type* that the
  hub uses to open configured workflows. There is no "module" term.

"Workflow" is host-infrastructure vocabulary. It does not become a pursuit domain term,
and pursuit's own contract keeps pursuit/leg/attempt names.

| Surface | Active term in this spec | Notes |
| --- | --- | --- |
| SDK dependency | `eos-agent-sdk` root package only | No deep imports past the SDK package's `exports` field. |
| Generic host registry | `WorkflowHub` | Required host-owned registry. Not an SDK concept. |
| Workflow type implementation | `WorkflowProvider` | Named in the SDK spec as a host-side contract (the SDK ships no workflow concepts). Not "module". |
| Configured workflow | `.eos-agents/workflow/<name>.md` (`WorkflowConfig`) | Profile-shaped markdown: frontmatter config + skill-style docs body. Not "instance". |
| First configured workflow | `pursuit` | Concrete product vocabulary follows Phase 05.3. |
| Workflow tools | `delegate_<name>` (+ any others the provider ships) | Per-workflow, provider-authored as a plain `ToolDefinition[]` and named off the configured workflow name; cancellation is the SDK supervisor's job (`cancel_background_task`), not a workflow tool. The factory injects them only for a profile that lists the workflow. |
| Workflow docs surface | system-prompt fragment + `read_workflow_docs` | The injected fragment lists available workflows by `description`; `read_workflow_docs(name)` serves one workflow's full manual. There is no `list_workflows` tool. |
| Context scripts | `.eos-agents/pursuit/scripts/` | Do not use `.eos-agents/workflow/scripts/` as an active script root. |
| Profile context script field | `pursuit_context_script` | Do not use `workflow_context_script`. |
| Context paths | `pursuit_<id>/leg_<id>/.../superseded/` | Do not render `workflow_<id>`, `iteration_<id>`, `focus.md`, `deferred_goal.md`, or `archived/`. |
| SDK background capability | `BackgroundTaskSupervisor` | Host tools are `list_background_tasks` and `cancel_background_task`. |

### 1.1 Phase 05.3 Supersessions

This split deliberately reverses or replaces the following Phase 05.3 decisions. They
are supersessions, not drift; the §14 hygiene scans are this split's completion evidence.

| Phase 05.3 surface | This split | Reason |
| --- | --- | --- |
| Hand-authored `delegate_pursuit` tool | Provider-authored `delegate_pursuit`, named off the configured workflow name; cancellation routes through `cancel_background_task` | Per-workflow tools are the design: the provider authors them as a plain `ToolDefinition[]`, the factory injects them only for profiles that list the workflow, and the delegate tool wires the run's cancel handle into the SDK background supervisor. Pursuit no longer hand-writes a generic adapter, and no `delegate_workflow` exists. |
| Background session type `"pursuit"`, `cancel_background_session`, `list_background_sessions` | SDK background task under a host-chosen `{ type: "pursuit", id: pursuit_<id> }` tag; `cancel_background_task(type, id, reason)`, `list_background_tasks` | The tag is a host-injected addressing handle the supervisor treats as opaque — not a session type with built-in behavior. The old session-type cancel and its "one open pursuit per run" semantics are gone. |
| `AgentLaunchPort` / `LaunchedAgent` / `LaunchSettlement` | Pursuit consumes SDK `Agent` handles directly through the narrow `PursuitAgents` slice (§11) | SDK decision log: launch seam deleted. |
| `PursuitAgentSubmissionBinding` | `onSubmit` in `createAgentOutcomeFn` | Single-mutator submission is now SDK terminal-contract mechanism. |
| Optional diagnostic `leg_goal_mode` on creation input | Dropped; mode derives from payload shape only (§10.1) | Narrower creation contract; one way to say each thing. |
| Profile field `agent_kind` | Deleted; the pursuit provider validates its configured planner/worker profiles at registration (§9) | Profiles carry no role/kind discriminator. |
| `ask_advisor` in profile `allowed_tools`, `isAdvisoryRequired`/`advisorPrompt` tool metadata | Factory-injected `ask_advisor` from the `AgentOutcomeFnWithAdvisory` terminal binding (§6, §7.3) | Advisory is host meta-policy on the terminal binding, not tool metadata and not profile tool selection. |
| `@eos/workflow` → `@eos/pursuit` package rename | Pursuit lives at `packages/workflows/pursuit/` in this host workspace | The SDK no longer ships pursuit; package naming inside the host workspace is host-internal. |

## 2. Boundary Summary

`eos-coding-agent` owns vocabulary and policy. The SDK owns reusable loop mechanics.

```text
eos-coding-agent
  imports only the eos-agent-sdk root package from the SDK
  owns:
    .eos-agents/ profile, llm-client, workflow, hook, and pursuit config
    every model-visible tool
    AgentFactory over host profiles
    WorkflowHub and configured workflows
    pursuit domain state, store, scripts, context projection, and terminal outcomes
    advisor/subagent host patterns
    composition root
```

A second host, for example `eos-research-agent`, should be a sibling application. It may
reuse `eos-agent-sdk` and the WorkflowHub pattern, but it does not inherit
`eos-coding-agent` tools, profiles, prompts, or pursuit policy by default. A second host
that also wants pursuit is the trigger to consider lifting pursuit out of this repository
into a shared package.

## 3. Target Layout

The target host remains a pnpm workspace, but the root package is the application.
Host-private composition, config, agent policy, and workflow-provider wiring live under
root `src/`. `packages/` is reserved for real package boundaries; `packages/app` is
intentionally absent.

```text
eos-coding-agent/
  package.json
  .eos-agents/
    profile/
      operator.md
      planner.md
      worker.md
      advisor.md
      subagent.md                      launchable only when another profile lists it
    llm_clients.json
    workflow/
      pursuit.md                       frontmatter config + skill-style docs body
    hooks.json
    hooks/                             .cjs command hook scripts
    pursuit/
      scripts/
        planner.cjs
        worker.cjs
        variable_reference_map.cjs
      context/                         machine-written pursuit context mirror
      pursuit.sqlite                   or configured store path
  packages/
  src/                                  composition root and host policy
    bootstrap.ts
    config/
      config-root.ts
      config-file.ts
      hook-config.ts
      profile-loader.ts
      workflow-config.ts
    agents/
      agent-factory.ts
      advisor-pass-registry.ts
    tools/
      agent/
        run-subagent.ts
        ask-advisor.ts
      background/
        list-background-tasks.ts
        cancel-background-task.ts
      records/
        read-agent-run.ts
      workflow/
        read-workflow-docs.ts           provider-agnostic over a structural docs view
      pursuit/
        delegate-pursuit.ts             pursuit delegate tool factory
      sandbox/                         exec/file family (§7), migrated as-is
    workflows/
      hub.ts                            WorkflowHub + per-profile view + prompt fragment
      contract.ts                       WorkflowProvider, WorkflowConfig, BuildWorkflowTools
      pursuit-provider.ts               adapter over openPursuitService; imports src/tools/pursuit
      pursuit-context-scripts.ts        script resolution + ComposeLaunchContext composer
  packages/
    workflows/
      pursuit/
        contracts/
        db/                            absorbed former @eos/db (createPursuitDatabase)
        src/
          service.ts
          agent-launcher.ts            launch queue, claims, post-commit guards
          pursuit-tree.ts
          pursuit-context.ts
          pursuit/   leg/   attempt/   plan/   work-item/
            state.ts transition.ts context.ts (per entity; plan has no context)
          context-engine/
            composer.ts                ComposeLaunchContext seam
            input.ts
            projection/
              listing.ts paths.ts resolve.ts mirror.ts
        tests/
    scripts/                           executeJsonCommand subprocess runner
    testkit/                           .eos-agents fixture building
```

Ownership rules for the layout:

- The hub contract lives in `src/workflows`, not in pursuit. Pursuit stays
  caller-agnostic (Phase 05.3 §4) and never imports hub vocabulary; the
  `pursuit-provider.ts` adapter in root `src` wraps `openPursuitService` into a
  `WorkflowProvider`.
- `src/tools` holds model-visible tool implementations. Agent tools that close over the
  concrete `AgentFactory` and advisory policy live in `src/tools/agent/`.
- Provider-authored workflow tools live in `src/tools/<workflow>/` when they are plain
  tool factories such as `(name, service, agents) => ToolDefinition`. The provider in
  `src/workflows/<name>-provider.ts` imports them and supplies `service` + `agents` from
  its `BuildWorkflowTools` builder. `read-workflow-docs.ts` lives under
  `src/tools/workflow/` because it is provider-agnostic over a structural docs view.
- Pursuit is not a standalone product package; lifting it out is gated on a second host
  (§2).

## 4. Composition Root

The application bootstrap builds each composition-root value once and wires only public
SDK values.

```ts
import { createAgentSdk } from "eos-agent-sdk";
import { createAgentOutcomeFnWithAdvisory } from "./agents/agent-factory.js";

import { buildAgentFactory } from "./agents/agent-factory.js";
import { loadEosConfig } from "./config/config-file.js";
import { WorkflowHub } from "./workflows/hub.js";
import { pursuitWorkflowProvider } from "./workflows/pursuit-provider.js";
import {
  pursuitContextScriptComposer,
  resolvePursuitContextScripts,
} from "./workflows/pursuit-context-scripts.js";

const cfg = loadEosConfig(".eos-agents");

const sdk = createAgentSdk({
  llmClients: cfg.llmClients,
  hooks: cfg.hooks,
  recordsDir: cfg.recordsDir,
});

const compose = pursuitContextScriptComposer(
  resolvePursuitContextScripts(cfg.profiles, cfg.workflows),
);

const hub = WorkflowHub.open({
  workflows: cfg.workflows,
  providers: [pursuitWorkflowProvider({ profiles: cfg.profiles, compose })],
});

const agents = buildAgentFactory(sdk, cfg.profiles, cfg.recordsDir, hub);

const mainOutcomeFn = createAgentOutcomeFnWithAdvisory({
  name: "submit_main_outcome",
  description: SUBMIT_MAIN_DESCRIPTION,
  schema: MainOutcome,
  advisoryPrompt: MAIN_ADVISOR_PROMPT,
});

const operator = agents.create("operator", mainOutcomeFn);
```

Rules:

- The composition root imports `eos-agent-sdk` and local host modules only.
- `WorkflowHub.open` completes before `AgentFactory` construction so profiles can bind
  workflow tools to only the workflows they declare.
- `WorkflowHub.open` is fail-fast: an unknown `type`, an args-schema mismatch, or a
  throwing `provider.create` aborts startup with an error naming the workflow. There is
  no degraded "registered with errors" state (§8).
- Advisory prompts are caller-supplied inside the terminal binding:
  `createAgentOutcomeFnWithAdvisory` wraps the SDK's `createAgentOutcomeFn` and stores
  the advisory prompt beside the minted outcome function (`MAIN_ADVISOR_PROMPT` lives
  beside `SUBMIT_MAIN_DESCRIPTION`); there is no advisory prompt registry. Binding the
  advisory shape is also the `ask_advisor` opt-in; no profile lists that tool.
- Pursuit is registered through the hub. Do not wire pursuit directly into the operator
  tool list while bypassing the hub.
- The SDK receives parsed objects and callbacks. File discovery, profile loading,
  subprocess wrapping, and schema validation stay in this host.
- Every configured path (scripts, store, context root) resolves from the directory
  owning `.eos-agents`, never from the process cwd.

## 5. Configuration

`.eos-agents/` is the single config root.

```text
.eos-agents/
  profile/*.md
  llm_clients.json
  workflow/*.md
  hooks.json
  hooks/*.cjs
  pursuit/scripts/*.cjs
  pursuit/context/
```

### 5.1 Profiles

Profiles are host-owned records. The SDK never reads profile files. A profile is a
markdown file: YAML frontmatter plus the system prompt as the body.

| Field | Required | Meaning |
| --- | --- | --- |
| `name` | yes | The agent name other config refers to (becomes `AgentSpec.name`). |
| `llm_client_id` | yes | Resolves against `llm_clients.json`. |
| `description` | no | Human-facing one-liner. |
| `max_turns` | no | Feeds SDK `AgentSpec.maxTurns`; SDK default applies when absent. |
| `allowed_tools` | yes | The profile's ordinary model-visible tool list (`ask_advisor` is never listed; §6). |
| `terminal_tool` | no | Present → terminal-tool mode; absent → SDK text termination mode. |
| `workflows` | no | Workflow names from `.eos-agents/workflow/` this profile may use; a non-empty list injects that workflow's tools and a prompt fragment (§6). |
| `subagents` | no | Profile names this profile may launch through `run_subagent`. |
| `pursuit_context_script` | no | Config-base-relative initial-message script path; required for any profile referenced as a pursuit `planner` or `worker` (§9), and must resolve under `.eos-agents/pursuit/scripts/`. |

```yaml
name: operator
llm_client_id: codex_operator
terminal_tool: submit_main_outcome
workflows:
  - pursuit              # injects delegate_pursuit, read_workflow_docs + the prompt fragment
subagents:
  - subagent
allowed_tools:
  - run_subagent
```

The operator lists no workflow tools: declaring `pursuit` under `workflows` is what makes
them appear (§6).

```yaml
name: planner
llm_client_id: codex_coding_plan
max_turns: 100
terminal_tool: submit_planner_outcome
pursuit_context_script: .eos-agents/pursuit/scripts/planner.cjs
allowed_tools:
  - read
  - write
  - edit
  - exec_command
```

Startup validation:

- Every `workflows` entry must name a configured workflow from `.eos-agents/workflow/`.
- `allowed_tools` contains only ordinary host tools. Startup rejects any factory-injected
  tool there: `ask_advisor`, `read_workflow_docs`, or any configured workflow's
  frontmatter `tools`. The factory injects workflow tools and prompt fragments from
  `profile.workflows`, so two agents can have different visible workflows even though
  they share one host hub.
- Every `subagents` entry must name a known, non-terminal profile — a subagent launch
  supplies no outcome function. A profile that exposes `run_subagent` must declare at
  least one subagent name. The target is a normal profile; there is no role field,
  subagent registry, or kind classifier.
- Reject `agent_kind` and `workflow_context_script` if present; both are dead fields.

`subagent.md` is a normal profile file. It becomes a subagent only from the caller's
allow-list:

```yaml
name: subagent
llm_client_id: codex_subagent
allowed_tools:
  - read_agent_run
```

### 5.2 Workflows (`.eos-agents/workflow/`)

Each configured workflow is one markdown file, profile-shaped: YAML frontmatter for
config, the body as the workflow's skill-style manual. `workflow/pursuit.md`:

```markdown
---
name: pursuit
type: pursuit
description: Delegate a multi-leg coding pursuit.
tools:
  - delegate_pursuit
args:
  planner: planner
  worker: worker
  store: .eos-agents/pursuit/pursuit.sqlite
  context_root: .eos-agents/pursuit/context
  default_max_attempts: 2
---
# pursuit

Long-running goal pursuit: a pursuit owns ordered legs; each leg runs planner then
worker attempts against an attempt budget.

## Operating loop

Call `delegate_pursuit` to start one, watch the background task it registers, read the
`pursuit_<id>/` context paths for progress, and `cancel_background_task` to stop it.

## Tools

### delegate_pursuit
Payload semantics; the returned task id and title both embed `pursuit_<id>`. Cancel a
running pursuit with `cancel_background_task` on that task id.
```

The file basename is the workflow `name`, and must be a valid tool-name fragment
(`snake_case`) because the provider names its tools off it (§8). Frontmatter `type`
selects the provider; `args` is validated by that provider's schema at `WorkflowHub.open`
(§8); `description` is the one-line text the system-prompt fragment shows; `tools` is the
declared list of model-tool names this workflow exposes, reconciled against the provider's
output at assembly (§8.1). The body is the `read_workflow_docs` manual. The `planner` /
`worker` values are agent names; `default_max_attempts` is schema-defaulted and may be
omitted. Each `tools` entry must be a unique valid tool name and must not be
`read_workflow_docs`. Context-script selection is not workflow config: it stays on the
planner/worker profiles (`pursuit_context_script`), resolved by the app (§11).

This placement follows one boundary rule, used consistently across the host: model-facing
prose about a *configured capability* (`description`, the docs body) lives in
`.eos-agents` markdown; prose bound to a *code contract* (a tool's one-line description, a
payload field's `.describe()`, an advisory prompt) stays beside the code. The provider
supplies the tools; the markdown supplies the words around them.

Configuring a workflow here does not expose it to any agent. Exposure happens only when a
profile lists the workflow's `name` in `workflows` (§6).

V1 configures one workflow, `workflow/pursuit.md`. A second pursuit (different store,
different planner/worker pair) is one more file with `type: pursuit` — say
`workflow/pursuit_staging.md`, whose delegate tool is named `delegate_pursuit_staging`
(declared under its `tools`). The provider is reused; no new tool authoring.

## 6. AgentFactory

`AgentFactory` is the only place that turns a host profile into an SDK `AgentSpec`.

```ts
/** Host-owned terminal binding: the SDK outcome contract plus the advisory
 *  prompt that guards its submissions. The host stores the prompt only;
 *  terminal semantics stay inside the opaque SDK value. */
interface AgentOutcomeFnWithAdvisory<T> {
  kind: "with_advisory";
  outcomeFn: AgentOutcomeFn<T>;
  advisoryPrompt: string;
}

/** The single constructor for the host advisory binding. Both the spec helper
 *  below and the pursuit adapter (§11) go through it, so the `kind` discriminator
 *  is stamped in exactly one place. */
export function withAdvisory<T>(
  outcomeFn: AgentOutcomeFn<T>,
  advisoryPrompt: string,
): AgentOutcomeFnWithAdvisory<T> {
  return { kind: "with_advisory", outcomeFn, advisoryPrompt };
}

export function createAgentOutcomeFnWithAdvisory<T>(spec: {
  name: string;
  description?: string;
  schema: z.ZodType<T>;
  onSubmit?: (payload: T, ctx: SubmitCtx) => Promise<{ accept: T } | { reject: string }>;
  advisoryPrompt: string;
}): AgentOutcomeFnWithAdvisory<T> {
  const { advisoryPrompt, ...outcome } = spec;
  return withAdvisory(createAgentOutcomeFn(outcome), advisoryPrompt);
}

interface AgentFactory {
  create<T = string>(
    name: string,
    agentOutcomeFn?: AgentOutcomeFn<T> | AgentOutcomeFnWithAdvisory<T>,
  ): Agent<T>;
}

export function buildAgentFactory(
  sdk: AgentSdk,
  profiles: AgentProfileRegistry,
  recordsDir: string,
  workflowHub: WorkflowHub,
): AgentFactory;
```

Creation rules:

- `profile.allowed_tools` is the source of truth for ordinary tool selection. Two tool
  groups are factory-injected rather than selected: `ask_advisor` (from the advisory
  binding, below) and the workflow tools (from `profile.workflows`, next).
- `AgentFactory` resolves each selected name against the host tool registry (§7); an
  unknown name is a startup error, as is a duplicate name across the assembled set
  (ordinary tools, injected workflow tools, and `ask_advisor`).
- Workflow tools are gated on `profile.workflows`, their single authority (the same
  pattern as `ask_advisor`). When it is non-empty, `AgentFactory` asks the hub for a view
  scoped to those names, then:
  - appends the view's workflow tools (e.g. `delegate_<name>`), built via
    `view.tools(this)` (cancellation is `cancel_background_task`, not a tool);
  - appends one `read_workflow_docs` tool over the same view, with its `name` input
    schema generated from `view.names()`; and
  - prepends `view.promptFragment()` to the system prompt.
  It never passes the whole hub registry to model-visible tools. When `profile.workflows`
  is empty the agent has no workflow surface, and a profile naming `read_workflow_docs` or
  any configured workflow tool in `allowed_tools` is a startup error (§5.1).
- `ask_advisor` is never selected through `allowed_tools`. Profiles do not list it, and
  startup validation rejects a profile that names it; only the factory injects it.
- Binding an `AgentOutcomeFnWithAdvisory` is the advisory opt-in: the factory appends
  `askAdvisor(agents, advisoryPrompt, passes)` built from the stored prompt to the
  resolved toolset and installs the `preToolUse` gate on `profile.terminal_tool`. A bare
  `AgentOutcomeFn`, or no binding, wires no advisor tool and no gate. The factory tells
  the two shapes apart with the host-owned `kind: "with_advisory"` discriminator; the SDK
  value is opaque and exposes no host fields.
  Because the advisory prompt rides the terminal binding, an advisor can never be wired
  without a terminal tool; there is no separate validation rule for that.
  `buildAgentFactory` constructs exactly one `AdvisorPassRegistry` (in-memory, keyed by
  `runId`) and threads that same instance into both `askAdvisor` and the gate, so a pass
  recorded by the tool is visible to the gate guarding the same run (§7.3).
- Advisory stays a host-only concept: the SDK receives an ordinary tool definition and
  an ordinary `preToolUse` hook entry on the `AgentSpec`. No advisory field, prompt, or
  flag crosses the SDK boundary.
- If `profile.terminal_tool` is present, the caller must provide a terminal binding
  whose tool name matches (checked with the SDK's `agentOutcomeToolName`; for
  `AgentOutcomeFnWithAdvisory`, against its `outcomeFn`).
- If `profile.terminal_tool` is absent, the caller must not provide a terminal binding;
  the agent runs in SDK text termination mode.
- `run_subagent` target validation is caller-profile-owned: the tool's input schema
  enumerates that profile's `subagents` list, and startup validation guarantees each
  entry resolves to a known, non-terminal profile.
- Pursuit planner/worker profile validation does not live here; the pursuit provider
  performs it at registration (§9).

`AgentFactory` is a single composition-root value passed by dependency injection, not a
global singleton. There is exactly one instance, but it is threaded explicitly rather than
reached through a module global, for two reasons: the hub↔factory cycle (the factory needs
the hub, the hub's tools need the factory) would make a global temporally coupled —
assigned mid-boot, `undefined` if read early; and pursuit must stay caller-agnostic (§3),
depending on the narrow `PursuitAgents` slice it owns rather than importing a host global
(which would also be an upward package edge). Tools that need to launch agents are tool
factories closed over it, and workflows that launch agents receive it directly when the
factory builds their tools (`view.tools(agents)`). Do not put `AgentFactory` on SDK
`ToolCallContext`.

## 7. Tool Surface

Every model-visible tool is authored in this repository with the SDK `defineTool`
contract. The SDK ships none.

| Family | Tools | Package | Notes |
| --- | --- | --- | --- |
| agent patterns | `run_subagent`, `ask_advisor` | `src/tools/agent/` | Close over `AgentFactory` and host advisory policy. `ask_advisor` is factory-injected via the advisory binding, never profile-selected. |
| workflow | `read_workflow_docs` + per-workflow provider tools, e.g. `delegate_<name>` | `src/tools/workflow/read-workflow-docs.ts`; provider tools under `src/tools/<workflow>/`, wired by `src/workflows/<name>-provider.ts` | Factory-injected from `profile.workflows`, never in `allowed_tools`. Providers author and name their own tools off the configured workflow name. |
| background | `list_background_tasks`, `cancel_background_task` | `src/tools/background/` | Thin projections over `ctx.backgroundTaskSupervisor`. |
| records | `read_agent_run` | `src/tools/records/` | Factory over `recordsDir`. |
| sandbox | `read`, `multi_read`, `write`, `edit`, `exec_command`, `command_stdin`, `read_command_transcript` | `src/tools/sandbox/` | The coding capability, bridged to the sandbox daemon. Migrated as-is; out of this split's redesign scope, but the names must exist in the registry or every current planner/worker profile fails `allowed_tools` validation. |

There is no coding-agent-specific tool context. Tool code receives only the SDK
`ToolCallContext`:

```ts
interface ToolCallContext {
  runId: AgentRunId;
  toolUseId: ToolUseId;
  signal: AbortSignal;
  llmMessages: readonly Message[];
  backgroundTaskSupervisor: BackgroundTaskSupervisor;
  notifier: Notifier;
}
```

Host resource state enters tool definitions inside `AgentFactory` assembly, using the
`recordsDir`, `workflowHub`, and `AgentFactory` values from bootstrap. Do not add
`agents`, `workflow`, or advisory-prompt fields to SDK context.

### 7.1 Background Task Tools

```ts
export const listBackgroundTasks = defineTool({
  name: "list_background_tasks",
  description: "List this run's active background tasks.",
  input: z.object({}),
  execute: async (_input, ctx) => ({
    output: renderBackgroundTaskRows(ctx.backgroundTaskSupervisor.list()),
  }),
});

export const cancelBackgroundTask = defineTool({
  name: "cancel_background_task",
  description: "Cancel one active background task in this run, addressed by its {type, id} tag.",
  input: z.object({
    type: z.string().min(1),
    id: z.string().min(1),
    reason: z.string().min(1),
  }),
  execute: async (input, ctx) => ({
    output: (await ctx.backgroundTaskSupervisor.cancel(
      { type: input.type, id: input.id },
      input.reason,
    ))
      ? "cancelled"
      : "not found",
  }),
});
```

This split intentionally uses SDK background-task vocabulary. Current Phase 05.3
implementation evidence still mentions background session type `"pursuit"` because it
predates the SDK split (§1.1). In this host spec, a workflow's `delegate_<name>` tool
registers its run as a background task under a host-chosen `{ type, id }` tag — for
pursuit, `{ type: "pursuit", id: pursuit_<id> }` — and the model cancels it with
`cancel_background_task(type, id, reason)`. No model-facing background-session API
survives, and cancellation is keyed on the register-time tag, not the supervisor's
internal task id.

**Dependency:** this requires the SDK `BackgroundTaskSupervisor` contract
(`eos-agent-sdk_SPEC.md`) to take a `tag: { type, id }` at `register` (unique among
active tasks), expose it on `list()` rows, pass `reason` to each task's `cancel`
callback, and resolve `cancel(tag, reason)` by that tag.

### 7.2 Subagent Tool

`run_subagent` starts another profile by agent name, but only when the caller profile
lists that target in `subagents`. It supports foreground and background execution
through one tool. Background execution registers exactly one background task, and the
task's `onCompletion` is the only completion publisher.

```ts
export function runSubagent(
  agents: AgentFactory,
  subagents: readonly [string, ...string[]],
): ToolDefinition {
  return defineTool({
    name: "run_subagent",
    description: "Run another configured agent.",
    input: z.object({
      agent_name: z.enum(subagents),
      prompt: z.string().min(1),
      wait: z.boolean().default(true),
    }),
    execute: async (input, ctx) => {
      const child = agents.create(input.agent_name);
      const run = child.start({
        messages: [{ role: "user", content: input.prompt }],
      });
      ctx.signal.addEventListener("abort", () => run.interrupt());

      if (input.wait) {
        return { output: renderAgentOutcome(await run.outcome()) };
      }

      ctx.backgroundTaskSupervisor.register({
        tag: { type: "subagent", id: run.runId },
        title: `${input.agent_name}: ${input.prompt.slice(0, 80)}`,
        cancel: () => run.interrupt(),   // interrupt() carries no reason; the supervisor still records it
        done: run.outcome().then(toBackgroundTaskOutcome),
        onCompletion: (out, { notifier }) => {
          notifier.publish(renderSubagentCompletion(input.agent_name, run.runId, out), {
            key: `subagent:${run.runId}`,
          });
        },
      });

      return { output: `subagent started: ${run.runId}` };
    },
  });
}
```

### 7.3 Advisor Tool and Gate

Advisor is a host pattern, not SDK metadata. Agents opt in at `create` time: binding an
`AgentOutcomeFnWithAdvisory` makes the factory add `ask_advisor` to the toolset and
install the terminal gate. No profile lists `ask_advisor` in `allowed_tools`.

Pass tracking is an in-memory, per-run registry in the app — the gate never reads
transcript records and never starts an advisor itself.

```ts
interface AdvisorSubmission {
  tool_name: string;
  payload: JsonObject;
}

interface AdvisorPassRegistry {
  recordPass(runId: AgentRunId, submission: AdvisorSubmission): void;
  /** Canonical-JSON (sorted keys) deep-equality of { tool_name, payload }. */
  hasPass(runId: AgentRunId, submission: AdvisorSubmission): boolean;
}

const ADVISOR_AGENT_NAME = "advisor";

const AdvisorVerdict = z.object({
  verdict: z.enum(["pass", "fail"]),
  reason: z.string().min(1),
});

const advisorOutcomeFn = createAgentOutcomeFn({
  name: "submit_advisor_outcome",
  description: SUBMIT_ADVISOR_DESCRIPTION,
  schema: AdvisorVerdict,
});

export function askAdvisor(
  agents: AgentFactory,
  advisoryPrompt: string,
  passes: AdvisorPassRegistry,
): ToolDefinition {
  return defineTool({
    name: "ask_advisor",
    description: "Ask the configured advisor to review a terminal submission.",
    input: z.object({
      tool_name: z.string().min(1),
      payload: z.object({}).passthrough(),
    }),
    execute: async (input, ctx) => {
      const advisor = agents.create(ADVISOR_AGENT_NAME, advisorOutcomeFn);
      const run = advisor.start({
        messages: [
          userText(renderCallerMessages(ctx.llmMessages)),
          userText(`${advisoryPrompt} Verify against the exact target below.\n${canonicalJson(input)}`),
        ],
      });
      ctx.signal.addEventListener("abort", () => run.interrupt());
      const outcome = await run.outcome();
      if (outcome.status === "completed" && outcome.outcome.verdict === "pass") {
        passes.recordPass(ctx.runId, {
          tool_name: input.tool_name,
          payload: input.payload,
        });
      }
      return { output: renderAdvisorVerdict(outcome) };
    },
  });
}
```

Because the prompt is bound per launch, an agent's `ask_advisor` always carries the
review standard for its own terminal tool. `input.tool_name` stays in the input because
the gate's exact-match contract needs the model to state the tool and payload it intends
to submit.

The terminal gate is a `preToolUse` hook installed by `AgentFactory` whenever the
terminal binding passed to `create` carries an advisory prompt. The binding is the meta
information the factory checks; profile markdown plays no part in the decision.

```ts
function isAdvisoryBinding<T>(
  binding: AgentOutcomeFn<T> | AgentOutcomeFnWithAdvisory<T>,
): binding is AgentOutcomeFnWithAdvisory<T> {
  return "kind" in binding && binding.kind === "with_advisory";
}

function advisoryHooksFor(
  binding: AgentOutcomeFn<unknown> | AgentOutcomeFnWithAdvisory<unknown> | undefined,
  profile: AgentProfile,
  passes: AdvisorPassRegistry,
): HookEntry[] {
  if (binding === undefined || !isAdvisoryBinding(binding)) {
    return [];
  }
  return [requireAdvisoryPass({ toolName: profile.terminal_tool, passes })];
}
```

The hook denies when `passes.hasPass(facts.runId, { tool_name: facts.toolName,
payload: facts.input })` is false. It never starts an advisor itself. A denial reaches
the live model as a tool error, mutates no pursuit state, and consumes no attempt
budget.

Pre/post hooks receive `ToolCallFacts` only, so terminal submission payloads must be
self-contained enough for advisor review.

## 8. WorkflowHub

WorkflowHub is a host-owned registry. It is deliberately retained because the coding
agent needs one place for workflow discovery, docs, and delegation. It is not part of
the SDK.

```ts
type WorkflowArgs = Record<string, unknown>;

/** One parsed .eos-agents/workflow/<name>.md. The hub holds this for the workflow's
 *  lifetime; nothing below re-declares its fields. */
interface WorkflowConfig {
  name: string;                        // basename; also the tool-name stem
  type: string;                        // frontmatter; selects the provider
  args: unknown;                       // frontmatter; parsed by the provider's args schema
  description: string;                 // frontmatter; the prompt-fragment line
  docs: string;                        // markdown body; the read_workflow_docs manual
  tools: string[];                     // frontmatter; declared model-tool names this workflow exposes
}

interface WorkflowProvider<A extends WorkflowArgs = WorkflowArgs> {
  type: string;
  args: z.ZodType<A>;
  /** Open this configured workflow's service (sync, fail-fast) and return its tool
   *  builder. name/args is all the provider needs; everything else lives on WorkflowConfig. */
  create(name: string, args: A): BuildWorkflowTools;
}

/** Build this workflow's model tools, closed over the per-profile agent factory.
 *  `agents` is the only value that post-dates WorkflowHub.open (the hub<->factory
 *  cycle); a non-agent workflow returns a nullary builder and ignores it. */
type BuildWorkflowTools = (agents: AgentFactory) => ToolDefinition[];

interface WorkflowHubInit {
  workflows: WorkflowConfig[];
  providers: readonly WorkflowProvider[];
}

declare class WorkflowHub {
  /** Fail-fast join of config files and providers (sync). */
  static open(init: WorkflowHubInit): WorkflowHub;
  forProfile(names: readonly [string, ...string[]]): ProfileWorkflowView;
}

interface ProfileWorkflowView {
  /** The profile-visible workflow names, used to scope read_workflow_docs input. */
  names(): readonly [string, ...string[]];
  /** This profile's workflow tools — each workflow's ToolDefinition[], concatenated. */
  tools(agents: AgentFactory): ToolDefinition[];
  /** The system-prompt block listing this profile's workflows and their tool names. */
  promptFragment(): string;
  /** The read_workflow_docs manual for one profile-visible workflow. */
  docs(name: string): string;
}
```

The `open` join, per `workflow/<name>.md` file:

```text
provider = providers.find(p => p.type === cfg.type)     unknown type      -> startup error
args     = provider.args.parse(cfg.args)                schema mismatch   -> startup error
build    = provider.create(cfg.name, args)              create throw      -> startup error
  -> { config: cfg, build }                             cfg owns description / docs / tools
```

Fail-fast is a resolved decision: a workflow that cannot open (for example, pursuit's
store path is unwritable) aborts startup with an error naming the workflow. There are no
readiness or error rows; every workflow a profile view exposes is delegatable. The
startup error message is the diagnosis surface.

### 8.1 Tool Naming and the Profile View

The hub mints no tools at `open`. Per profile, when `AgentFactory` assembles an agent
that lists the workflow (§6), the factory calls `view.tools(agents)`, which concatenates
each listed workflow's `build(agents)` output. Providers name their own tools,
conventionally off the configured `name` (`delegate_${name}`), so one provider serves
`pursuit` and `pursuit_staging` without re-authoring — `delegate_pursuit`,
`delegate_pursuit_staging`, and so on.

`WorkflowConfig.tools` is the declared tool surface. Because tool names are otherwise
unknown until `build(agents)` runs, declaring them in frontmatter lets the hub work from
config alone:

- it lists the callable tool names in `promptFragment()` (so the model sees the surface,
  not just "read the docs");
- the factory collision-checks and rejects an `allowed_tools` entry that names a workflow
  tool or `read_workflow_docs` without instantiating anything; and
- at assembly the hub asserts the provider's produced tool names equal the union of the
  listed workflows' `cfg.tools`, failing fast and naming the workflow on drift.

`ProfileWorkflowView` exposes exactly four things to the factory:

- `names()` — the workflow names visible to this profile; `read_workflow_docs` uses it for
  its enum input schema so an agent cannot request docs for hidden workflows.
- `tools(agents)` — the workflow `ToolDefinition[]`, each closed over `agents`.
- `promptFragment()` — the system-prompt block listing this profile's workflows by
  `description` and their declared `tools`, with the instruction to call
  `read_workflow_docs(<name>)` before first use. This replaces a `list_workflows` tool:
  the list is fixed at assembly, so it is prompt text, not a model round trip.
- `docs(name)` — the `read_workflow_docs` body for one profile-visible workflow.

Progressive discovery — light at the surface, detail one read away:

- **Tool `description` and payload-field `.describe()` text** stay terse, a line each. A
  profile can carry many workflow tools, so the always-visible surface must be cheap.
- **`read_workflow_docs(<name>)`** returns the full manual (the `workflow/<name>.md`
  body): operating loop, payload semantics, mode and successor rules, the context-path
  universe, and settlement vocabulary.
- **`promptFragment()`** is the always-on index: one `description` line per workflow, its
  declared `tools`, and the read-the-docs instruction.

`read_workflow_docs` is the provider-agnostic workflow docs tool; every other workflow
tool is provider-authored. Adding a workflow *type* means writing a provider; configuring one
means adding a `workflow/<name>.md` file that reuses the provider under a new name. Only a
new type adds tool authoring.

The contract assumes nothing about what those tools *are*: a provider returns a plain
`ToolDefinition[]`, and any tool may register a background task (the conventional
`delegate_<name>` does, embedding the delegated domain id in the task title so
cancellation via `cancel_background_task` and context-path correlation keep working) or
run synchronously. `BuildWorkflowTools`'s `agents` parameter is the one value that
post-dates `WorkflowHub.open` (the hub↔factory cycle); cycle-free dependencies (a store,
a scheduler, an HTTP client) are injected into the provider at construction and captured
in `create`, so a non-agent workflow returns a nullary builder and never names
`AgentFactory`.

## 9. Pursuit Registration

Pursuit's provider is a root `src/workflows` adapter over the pursuit service. It holds
the two composition values pursuit cannot own: the profile registry (for registration
validation) and the script composer (so pursuit never spawns subprocesses).

```ts
export function pursuitWorkflowProvider(init: {
  profiles: AgentProfileRegistry;
  compose: ComposeLaunchContext;
}): WorkflowProvider<PursuitWorkflowArgs> {
  return {
    type: "pursuit",
    args: PursuitWorkflowArgsSchema,
    create(name, args) {
      assertPursuitProfiles(init.profiles, args);
      const service = openPursuitService({
        plannerAgentName: args.planner,
        workerAgentName: args.worker,
        storePath: args.store,
        contextRoot: args.context_root,
        defaultMaxAttempts: args.default_max_attempts,
        compose: init.compose,
      });
      return (agents) => [
        delegatePursuit(name, service, pursuitAgentsWithAdvisory(agents)),
      ];
    },
  };
}
```

`assertPursuitProfiles` is the registration-time validation that replaced the deleted
`agent_kind` strictness table:

- `args.planner` and `args.worker` must name known profiles.
- The planner profile's `terminal_tool` must be `submit_planner_outcome`; the worker
  profile's must be `submit_worker_outcome`, so pursuit's plain SDK outcome bindings can
  bind and the host adapter can attach the matching advisory prompt (§11).
- Both profiles must declare `pursuit_context_script`, with the resolved paths inside
  `.eos-agents/pursuit/scripts/`.

Any failure rejects `create`, so `WorkflowHub.open` fails startup naming the workflow.

`delegate_pursuit` is an ordinary tool. It is authored in
`src/tools/pursuit/delegate-pursuit.ts` and imported by `pursuit-provider.ts`,
which supplies `service` and `agents` from its
builder. It calls the pursuit service, which returns a cancel handle, and registers that
handle as a background task. The provider supplies no `description`/`docs`; those live on
the workflow's `WorkflowConfig` (frontmatter `description`, md body). Only the tool's own
one-line `description` is code-bound:

```ts
function delegatePursuit(
  name: string,
  service: PursuitService,
  agents: PursuitAgents,
): ToolDefinition {
  return defineTool({
    name: `delegate_${name}`,            // delegate_pursuit, delegate_pursuit_staging, …
    description: DELEGATE_PURSUIT_DESCRIPTION,
    input: CreatePursuitInputSchema,
    execute: async (input, ctx) => {
      const handle = await service.createPursuit(input, { agents });  // -> { pursuitId, title, cancel, done }
      ctx.backgroundTaskSupervisor.register({
        tag: { type: name, id: handle.pursuitId },   // delegate_pursuit -> { type: "pursuit", id: "pursuit_<id>" }
        title: handle.title,
        cancel: (reason) => handle.cancel(reason),
        done: handle.done,
        onCompletion: (out, { notifier }) =>
          notifier.publish(`${name} ${out.status}: ${out.outcome}`, {
            key: `${name}:${handle.pursuitId}`,
          }),
      });
      return { output: `pursuit delegated: ${handle.pursuitId} — ${handle.title}` };
    },
  });
}
```

Correlation rule: the background task's tag id is the delegated domain id (`pursuit_<id>`),
and `handle.title` embeds it too — `pursuit pursuit_<id>: <goal first line>` — so the
delegating model connects task rows, completion notifications, `pursuit_<id>/` context
paths, and `cancel_background_task("pursuit", "pursuit_<id>", …)` through one id it
already holds. The tool result echoes the id and title for the same reason.

The background task completion message is the single settlement publisher. Pursuit
authors the outcome text in pursuit vocabulary; the tool adapter publishes it. Because
the handle is a background task of the delegating run, SDK run-end disposal cancels a
still-running pursuit when its delegating run terminates — Phase 05.3 tool-adapter
behavior, preserved by mechanism.

## 10. Pursuit Domain Contract

This host split preserves Phase 05.3 behavior except where §1.1 names a supersession.

### 10.1 Creation Input

```ts
type CreatePursuitInput =
  | {
      pursuit_goal: string;
      leg_goals?: undefined;
    }
  | {
      pursuit_goal: string;
      leg_goals: readonly [string, ...string[]];
    };
```

Mode is derived from payload shape: omitting `leg_goals` starts dynamic mode; providing
non-empty `leg_goals` starts predefined mode. The Phase 05.3 optional diagnostic
`leg_goal_mode` field is dropped (§1.1); do not expose it in `delegate_pursuit`.

Dynamic mode:

- The first leg inherits `pursuit_goal`.
- Later legs inherit the previous successful leg's `next_leg_goal`.
- A planner may omit `leg_goal`, submit `leg_goal` to refocus, and submit successor-only
  `next_leg_goal`.

Predefined mode:

- Each leg uses `leg_goals[n]`.
- Planners must not submit `leg_goal` or `next_leg_goal`.
- Non-final successful legs advance to the next predefined leg goal.

### 10.2 Planner Payload

```ts
const PlannerWorkItemSpecSchema = z.strictObject({
  id: z.string().min(1),
  agent_name: z.string().min(1),
  title: z.string().min(1),
  spec: z.string().min(1),
  depends_on: z.array(z.string()).default([]),
});

const PlannerOutcomePayloadSchema = z.strictObject({
  summary: z.string().min(1),
  leg_goal: z.string().min(1).optional(),
  next_leg_goal: z.string().min(1).optional(),
  work_items: z.array(PlannerWorkItemSpecSchema).min(1),
});
```

All Phase 05.3 §10 validation rules carry over unchanged: goal-declaration rules per
mode, no payload shape for clearing `next_leg_goal` without a refocusing `leg_goal`,
work-item id uniqueness across non-superseded attempts of the same leg-goal version,
version-scoped `depends_on` targets, rejection of cross-attempt `depends_on` combined
with a new `leg_goal`, and rejection of old `description`/`work_item_spec`/`needs`
fields.

One rule is new because profile kinds are gone: **work-item `agent_name` must equal the
configured worker name** (a set of one in V1; growing `args.worker` into
`workers: [...]` is the sanctioned extension). Any other value — unknown or known but
not configured as this pursuit worker — is a correctable in-run rejection that
consumes no attempt budget.

### 10.3 Worker Payload

```ts
const WorkerOutcomePayloadSchema = z.strictObject({
  /** One-paragraph result; renders the work item's summary.md. */
  summary: z.string().min(1),
  /** True for delivered work; false for blocked, unsafe, impossible, or incomplete work. */
  is_pass: z.boolean(),
  /** Worker-submitted or system terminal outcome; renders the work item's outcome.md. */
  outcome: z.string().min(1),
});
```

An accepted worker submission settles the work item `Success` when `is_pass` is true and
`Failed` when `is_pass` is false. `summary` renders `summary.md`; `outcome` renders the
work item's `outcome.md` and feeds attempt failure reasons when the work item fails.
Worker death, cancellation, and max-turn failures are observed at `run.outcome()` and
settle the work item `Failed`/`Cancelled` through the existing failure paths (§11).

### 10.4 Dependency and Scheduler Rules

`depends_on` is a hard dependency, not a context hint.

- A work item can launch only when every direct dependency is terminal `Success`.
- A running work item is never converted to `Blocked`.
- Failed or blocked work items propagate `Blocked` only to not-yet-launched dependents.
- Unrelated running or launchable work items continue after a sibling fails.
- An attempt closes `Failed` only after block propagation leaves no work item `Running`
  or `NotStarted`.

`failure_reasons.md` is list-shaped and includes planner/context failures plus failed or
blocked work items.

### 10.5 Context Universe

The rendered path universe is:

```text
pursuit_<id>/
  goal.md
  outcome.md
  leg_<id>/
    leg_goal.md
    next_leg_goal.md
    outcome.md
    attempt_<id>/
      plan_summary.md
      failure_reasons.md
      outcome.md
      work_item_<id>/
        title.md
        spec.md
        summary.md
        outcome.md
    superseded/
      attempt_<id>/
        leg_goal.md
        next_leg_goal.md
        plan_summary.md
        failure_reasons.md
        outcome.md
        work_item_<id>/
          title.md
          spec.md
          summary.md
          outcome.md
```

Rules:

- `Plan` remains DB/launch/submission state and never reappears as a rendered context
  folder.
- `leg_goal.md` exists at leg creation and includes provenance.
- `next_leg_goal.md` appears only when an effective successor exists.
- Search excludes `superseded/` unless explicitly scoped there.
- The mirror root is `.eos-agents/pursuit/context`.

## 11. Pursuit Launch Pipeline

The SDK split removes `AgentLaunchPort` / `LaunchSettlement`. Pursuit consumes SDK
agents directly through the narrow slice it declares; the host provider adapts
`AgentFactory` to that slice. The compose seam is unchanged from the Phase 05.3
implementation: pursuit never spawns a subprocess.

```ts
// declared by pursuit (packages/workflows/pursuit)
interface PursuitAgents {
  create<T>(agentName: string, outcome: AgentOutcomeFn<T>): Agent<T>;
}

type ComposeLaunchContext = (
  agentName: string,
  input: PlannerPursuitContextInput | WorkerPursuitContextInput,
  signal: AbortSignal,
) => Promise<InitialUserMessage[]>;
```

`openPursuitService` deps are `{ plannerAgentName, workerAgentName, storePath,
contextRoot, defaultMaxAttempts, compose }`. There is no `workflowName` or `scriptsRoot`
parameter: workflow naming and script selection are the app adapter's concern. The app
resolves each relevant profile's `pursuit_context_script` at startup into a
per-profile-name map and wraps it with the `executeJsonCommand` runner from
`packages/scripts` (`pursuit-context-scripts.ts`) — hook-parity subprocess semantics,
JSON snapshot on stdin, `initial_messages` JSON on stdout, replace-never-merge. The
provider adapts `AgentFactory` into `PursuitAgents` when it builds the `delegate_<name>`
tool (`view.tools(agents)`), passes that adapter into `service.createPursuit`, and
captures it for the pursuit's lifetime. Advisory wrapping happens in that adapter before
forwarding to `AgentFactory`; pursuit sees only plain SDK `AgentOutcomeFn` values.

```ts
// app-owned adapter, not imported by pursuit
function pursuitAgentsWithAdvisory(agents: AgentFactory): PursuitAgents {
  return {
    create<T>(agentName: string, outcomeFn: AgentOutcomeFn<T>): Agent<T> {
      return agents.create(
        agentName,
        withAdvisory(outcomeFn, pursuitAdvisoryPromptFor(agentOutcomeToolName(outcomeFn))),
      );
    },
  };
}
```

The launch pipeline preserves the Phase 05.3 claim machinery verbatim
(`agent-launcher.ts`); only the port call is replaced:

```text
mutation transaction
  enqueueLaunch(trx, ...)               plan or work_item row -> launch_queue
  claimLaunchable(trx, ...)             entity -> Running, launch_token minted;
                                        work items pass the hard deps gate here
commit

per claim (post-commit launcher)
  verifyClaimLaunchable(db, claim)      stale token/status -> skip silently
  input = script input DTO              planner | worker shape (Phase 05.3 §11)
  msgs  = await compose(agentName, input, signal)
            rejection -> synthesize a context-composition attempt failure
  outcomeFn = plannerOutcome(service, target)     plan claim
            | workerOutcome(service, target)      work_item claim
  run = agents.create(agentName, outcomeFn).start({ messages: msgs })
  stampAgentRunId(db, claim, run.runId)
  pursuit cancel signal -> run.interrupt()
  run.outcome().then((o) => service.reconcileRun(claim, o))
```

`onSubmit` is the only successful-submission writer. Submission targets carry domain
identity only; launch-queue claim data (queue ids, launch tokens) stays inside
`agent-launcher.ts`:

```ts
interface PlannerSubmissionTarget {
  pursuitId: PursuitId;
  attemptId: AttemptId;
  planId: PlanId;
}

export function plannerOutcome(
  service: PursuitService,
  target: PlannerSubmissionTarget,
): AgentOutcomeFn<PlannerOutcomePayload> {
  return createAgentOutcomeFn({
    name: "submit_planner_outcome",
    description: SUBMIT_PLANNER_DESCRIPTION,
    schema: PlannerOutcomePayloadSchema,
    onSubmit: async (payload, ctx) => {
      const result = await service.submitPlannerOutcome({
        target,
        payload,
        runId: ctx.runId,
        submissionId: ctx.submissionId,
      });
      return result.ok ? { accept: payload } : { reject: result.error };
    },
  });
}

export function workerOutcome(
  service: PursuitService,
  target: WorkerSubmissionTarget,
): AgentOutcomeFn<WorkerOutcomePayload> {
  return createAgentOutcomeFn({
    name: "submit_worker_outcome",
    description: SUBMIT_WORKER_DESCRIPTION,
    schema: WorkerOutcomePayloadSchema,
    onSubmit: async (payload, ctx) => {
      const result = await service.submitWorkerOutcome({
        target,
        payload,
        runId: ctx.runId,
        submissionId: ctx.submissionId,
      });
      return result.ok ? { accept: payload } : { reject: result.error };
    },
  });
}
```

Rules:

- Launch claims are made inside the mutation transaction (entity flips to `Running`, a
  fresh `launch_token` is minted); nothing launches before commit.
- The post-commit launcher rechecks every claim (`verifyClaimLaunchable`): a cancel,
  attempt failure, or settlement that reached the row first makes the stale launch a
  silent skip.
- A compose rejection (script start failure, timeout, non-zero exit, invalid output)
  synthesizes a context-composition failure through the existing attempt failure path;
  it appears in `failure_reasons.md`.
- `stampAgentRunId` records the run-to-entity binding immediately after `start`.
- Pursuit cancellation interrupts every live planner/worker run via the captured
  handles; repeated cancel is idempotent.
- `submissionId` is the idempotency key for pursuit transitions.
- Correctable payload errors return `{ reject }` to the live model and consume no
  attempt budget.
- Death, cancellation, and max-turn failures never call `onSubmit`; pursuit observes
  them at `run.outcome()` and synthesizes the appropriate failed or cancelled
  settlement. The observer never mutates pursuit state after a successful `onSubmit` —
  it reconciles and performs death synthesis only.

## 12. Hooks

Host hooks are callback `HookEntry` values passed to `createAgentSdk`.

- `preToolUse` gates terminal submissions with advisor pass checks (§7.3).
- `postToolUse` is available for host policy replacement of ordinary tool results.
- `turnBoundary` hooks may publish reminders or status messages through the notifier.

Hook files stay host config:

```text
.eos-agents/hooks.json
.eos-agents/hooks/*.cjs
```

`hook-config.ts` loads every configured hook event into `cfg.hooks`, wrapping subprocess
hook scripts into callbacks with the `executeJsonCommand` runner from
`packages/scripts`. There is no separate notification-rule file, loader, compiler, or
vocabulary. The SDK never parses hook config and never publishes notification content by
itself.

## 13. Migration Sequencing

Steps 1-2 are substantially complete on disk; they are listed for the record.

1. **SDK flattening (done):** `eos-agent-sdk` is the single flattened package. Finish
   removing any remaining host concepts from its public surface.
2. **Host workspace bootstrap (done):** root `src/` and
   `packages/{workflows/pursuit,scripts,testkit}` exist. `packages/app` is absent.
   `legacy/` and `legacy-tests/` folders and the notification-rules config remain; the
   steps below retire them.
3. **WorkflowHub:** implement `hub.ts` and `contract.ts` in `src/workflows/`;
   wire `pursuitWorkflowProvider`; switch the composition root to `WorkflowHub.open`.
4. **Pursuit launch seam:** replace `AgentLaunchPort` / `LaunchSettlement` /
   `PursuitAgentSubmissionBinding` with `PursuitAgents`, SDK run handles, and the
   `plannerOutcome` / `workerOutcome` factories. Keep the launch-queue machinery
   unchanged. Drop `workflowName` and `scriptsRoot` from service deps; inject `compose`.
   Keep planner/worker advisory prompt content in the host-owned pursuit adapter, bundled
   when plain pursuit outcomes are forwarded to `AgentFactory`; the main prompt lives
   inside the composition root's outcome binding.
5. **Tool port:** move tool families out of `legacy/` per the §7 table. Rename
   `list_background_sessions` → `list_background_tasks` and
   `cancel_background_session` → `cancel_background_task`. Delete the legacy submission
   tool family (`createAgentOutcomeFn` replaces it) and the
   `advisory_prompts`/`description_prompts` folders (the §7.3 pattern replaces them).
   Strip `ask_advisor` from profile `allowed_tools`; it is factory-injected now.
6. **Hooks and notifications:** compile hook config into callbacks. Fold the live
   `TurnCompleted` notification-rule scripts into `.eos-agents/hooks/` as `turnBoundary`
   entries; delete `notification_rules.json`, `.eos-agents/notification-rules/`, and
   `notification-rules-config.ts`. `idle-wake.cjs` is dropped (see open questions).
7. **Vocabulary cleanup:** run the §14 scans against active TypeScript source, profiles,
   and pursuit scripts to prevent old product terms from leaking back in.

## 14. Acceptance Criteria

- `eos-coding-agent` imports only the `eos-agent-sdk` root package from the SDK.
  Host-internal workspace packages keep their own names.
- `WorkflowHub.open` is the only registration path: fail-fast on unknown type, args
  mismatch, or a throwing provider `create`, each error naming the workflow. No
  readiness/error rows exist; every listed workflow is delegatable.
- Removing pursuit requires deleting its provider entry plus one import; profiles that
  still reference the `pursuit` workflow fail startup validation until their `workflows`
  lists are updated.
- Every model-visible tool is defined in this repository per the §7 table; the SDK
  contains no tool implementations. The sandbox family names are present in the tool
  registry so current profiles pass `allowed_tools` validation.
- Each workflow's tools are provider-authored as a plain `ToolDefinition[]`,
  factory-injected only for profiles that list the workflow and never visible to other
  agents; the conventional delegation tool is `delegate_<name>` (e.g. `delegate_pursuit`).
  No generic `delegate_workflow` tool exists, and a workflow's produced tool names equal
  its frontmatter `tools`.
- `read_workflow_docs(<name>)` plus the injected prompt fragment are the only workflow
  discovery surface; no `list_workflows`, `describe_workflow`, or
  `read_workflow_definition` tool exists.
- `.eos-agents/workflow/` (one `<name>.md` per workflow) is the only workflow registry;
  `.eos-agents/pursuit/scripts/` is the only active pursuit script root; configured
  paths resolve from the config base dir, never the process cwd.
- Profiles use `pursuit_context_script`; active runtime wiring rejects
  `workflow_context_script` and any legacy profile-kind field.
- `run_subagent` validates `agent_name` from the caller profile's `subagents` list, and
  every configured subagent target is a known, non-terminal profile.
- Pursuit context paths use `pursuit_<id>/leg_<id>/superseded/` and never render
  `workflow_<id>`, `iteration_<id>`, `focus.md`, `deferred_goal.md`, `archived/`, or
  `/plan_`. `Plan` remains DB/launch/submission state only.
- Planner payloads use `leg_goal`, `next_leg_goal`, and work-item
  `title`/`spec`/`depends_on`; worker payloads use `summary`/`is_pass`/`outcome`;
  work-item `agent_name` must equal the configured worker name.
- The launch pipeline claims inside the mutation transaction with launch tokens,
  rechecks post-commit, stamps run ids, and never launches before commit. Pursuit never
  spawns subprocesses; the app-injected `compose` callback is the only initial-message
  source, and its failure surfaces as a context-composition attempt failure.
- Terminal tool identity is read only through the SDK root export
  `agentOutcomeToolName` (profile `terminal_tool` check, pursuit registration validation);
  the host never imports SDK internals for this, and the advisor gate matches on
  `profile.terminal_tool`.
- `ask_advisor` is factory-injected from the `AgentOutcomeFnWithAdvisory` binding and
  never appears in profile `allowed_tools`; advisory prompts travel inside the binding;
  there is no advisory prompt registry and no advisory metadata on tool definitions.
- The advisor gate consults the in-memory pass registry; it never reads transcript
  records and never starts an advisor. `buildAgentFactory` owns one `AdvisorPassRegistry`
  (keyed by `runId`) shared between every injected `ask_advisor` and its gate. Denial
  mutates no pursuit state and consumes no attempt budget; advisor enforcement runs before
  `onSubmit`.
- Background work uses SDK `BackgroundTaskSupervisor`; host tools are
  `list_background_tasks` and `cancel_background_task(type, id, reason)`, which addresses
  tasks by their register-time `{ type, id }` tag and forwards `reason` to the task's
  cancel callback. Workflow settlement notifications are published exactly once, by the
  delegate tool's `onCompletion` handler.
- The following hygiene checks have no active-source matches outside historical docs or
  explicit migration notes (the legacy planner work-item field `needs` is asserted by
  the planner payload schema tests rather than a repo-wide word grep):

```bash
rg -n "agent[_-]kind|delegate_workflow|\\blist_workflows\\b|\\bdescribe_workflow\\b|delegateWorkflowInputSchema|workflow_context_script|workflow_<id>|iteration_<id>|deferred_goal|archived/|focus\\.md|description\\.md|work_item_spec" eos-coding-agent/packages eos-coding-agent/.eos-agents
rg -n "WorkflowModule|WorkflowInstanceConfig|instanceName|read_workflow_definition|list_background_sessions|cancel_background_session|AgentLaunchPort|LaunchSettlement|PursuitAgentSubmissionBinding" eos-coding-agent/packages
rg -n "@eos/(tool|engine|agent-runtime)\\b|\\.eos-agents/workflow/scripts|\\.eos-agents/workflow\\.json" eos-coding-agent/packages
test ! -f eos-coding-agent/.eos-agents/workflow.json
git diff --check -- docs/plans/agent-core-to-sdk-and-coding-agent-split
```

## 15. Review Change List

These changes address the naming, logic-gap, redundancy, and round-trip review findings.
They do not change the core boundary: the SDK stays mechanism-only, and accepted pursuit
submissions still mutate pursuit state directly through `onSubmit`.

| Area | Change to make | Reason |
| --- | --- | --- |
| Advisory contract | Define the advisor profile validation rule: if any `AgentOutcomeFnWithAdvisory` can be created, startup must require a known `advisor` profile whose terminal tool is `submit_advisor_outcome`. | `askAdvisor` currently hard-codes `ADVISOR_AGENT_NAME = "advisor"` without a matching config invariant. |
| Advisory prompt source | Replace the mixed wording around "no advisory prompt registry" and `pursuitAdvisoryPromptFor(...)` with one explicit app-owned prompt source, such as `advisoryPromptForTerminalTool(toolName)`, and fail startup when a gated terminal tool has no prompt. | The current text rejects registries but still needs a lookup for planner/worker/main prompts. |
| Workflow tool names | Remove `WorkflowConfig.tools` as a markdown-authored source of tool names, or make it a generated/provider-owned declaration such as `provider.toolNames(name, args)`. | The current frontmatter `tools` duplicates provider-authored names and creates rename drift. |
| Workflow docs round trip | Change the prompt-fragment rule from "call `read_workflow_docs(<name>)` before first use" to "call it only when the compact prompt fragment is insufficient"; for V1 `pursuit`, inject the compact operating contract directly. | This keeps `list_workflows` deleted without forcing a mandatory docs tool round trip for the only configured workflow. |
| Advisory round trips | State exactly which terminal tools are advisory-gated by default (`submit_main_outcome`, `submit_planner_outcome`, `submit_worker_outcome`) and which can opt out. | Planner/worker fanout currently implies an advisor call before every terminal submission, which is expensive and should be an explicit policy choice. |
| Workflow vocabulary | Decide whether `workflow` is allowed to be model-facing. If not, rename model-visible surfaces such as `read_workflow_docs` and `workflow:pursuit` to a neutral capability term; if yes, soften the "host-infrastructure only" rule. | The current naming rule says workflow is not pursuit domain vocabulary, but the model still sees workflow-labeled tools and task provenance. |
| Layout and sequencing | Separate "current checkout shape" from "target layout"; either add the missing `eos-coding-agent/package.json`/workspace root to the target work, or stop describing the root workspace bootstrap as done. Align target filenames with live names or list the intended renames. | The current tree has package groups, but not the target root package file, and live config files still use names such as `agent-profile-loader.ts`. |
| Migration checks | Expand §14 hygiene checks to cover profile/config cleanup: no active profile lists `ask_advisor`, no `.eos-agents/notification_rules.json`, no `.eos-agents/notification-rules/`, no legacy workflow script root, no `agent_kind` outside historical test fixtures. | Current checks focus on source symbols and miss several config-level stale surfaces that this spec explicitly retires. |

## 16. Open Questions

- Whether `read_agent_run` needs paging before extraction, since SDK records can grow
  large.
- Whether pursuit should remain a host-local package forever or move to a shared
  project when a second host needs it (§2).
- How the sandbox exec/file family is bridged (TypeScript `defineTool` wrappers over the
  sandbox daemon vs another mechanism). Out of this split's scope; the registry must
  include the names either way (§7).
- Idle/parked babysitting (the old `IdleParked` trigger rules): if still needed after
  the SDK's park/wake and owed-completion semantics, it returns as host runtime behavior
  over run events, not as config.
