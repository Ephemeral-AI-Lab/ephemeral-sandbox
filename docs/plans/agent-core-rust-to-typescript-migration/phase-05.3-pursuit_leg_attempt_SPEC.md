# EOS Agent Core Rust to TypeScript Migration - Phase 05.3 Pursuit / Leg / Attempt Vocabulary

Status: Complete
Date: 2026-06-12
Owner: eos-agent-core
Migration direction: Rust -> TypeScript
Project path: `/Users/yifanxu/machine_learning/LoVC/EphemeralOS/eos-agent-core`
Base specs:
- `phase-05-workflow-orchestration_SPEC.md`
- `phase-05.1-workflow-context-redesign_SPEC.md`
- `phase-05.2-workflow-outcome-context-rendering_SPEC.md`
Input notes:
- `note/workflow-vocabulary-judge-report.md`
- `note/leg-goal-without-focus-debate.md`
- `note/planner-worker-initial-messages.md`

## 0. Progress Tracker

Tracker status values:

| Status | Meaning |
| --- | --- |
| `Blocked` | Previous phase is not complete; do not start implementation. |
| `Pending` | Previous phase is complete, but this phase has not started. |
| `In progress` | This is the only active implementation phase. |
| `Complete` | Every checklist item is checked and phase verification passed. |

Gate rule:

```text
Only one phase may be In progress at a time.
Do not begin Phase N+1 until Phase N is Complete.
When a phase completes, update this tracker in the same change that records the
phase verification evidence.
```

Current phase status:

| Phase | Status | Completion evidence |
| --- | --- | --- |
| Phase 01 - Foundation rename and package layout | Complete | E1, E5, E6 |
| Phase 02 - Contracts, DB schema, and goal model | Complete | E1, E2, E5 |
| Phase 03 - Pursuit service creation and declaration semantics | Complete | E1, E2 |
| Phase 04 - Planner payload and dependency validation | Complete | E1, E2, E5 |
| Phase 05 - Attempt scheduler and failure propagation | Complete | E1, E2 |
| Phase 06 - Context projection, mirror, and snapshots | Complete | E1, E2, E6 |
| Phase 07 - Runtime, tool, and script wiring | Complete | E1, E2, E5 |
| Phase 08 - Verification, hygiene, and legacy removal | Complete | E1-E6 |

Verification evidence:

- E1 focused suites:
  `pnpm exec vitest run packages/contracts/tests/pursuit.test.ts packages/db/tests/schema.test.ts packages/pursuit/tests/package-boundary.test.ts packages/pursuit/tests/context.test.ts packages/pursuit/tests/mirror.test.ts packages/pursuit/tests/lifecycle.test.ts packages/tool/tests/pursuit-family.test.ts packages/agent-runtime/tests/agent-profile.test.ts packages/agent-runtime/tests/runtime.test.ts packages/agent-runtime/tests/pursuit-runtime.test.ts packages/agent-runtime/tests/pursuit-active-scripts.test.ts`
  passed with 11 test files and 116 tests.
- E2 full workspace gate: `pnpm run check` passed, including
  `pnpm run typecheck`, `pnpm run lint`, and `pnpm run test`; Vitest reported
  39 test files and 436 tests passed.
- E3 docs hygiene:
  `git diff --check -- docs/plans/agent-core-rust-to-typescript-migration eos-agent-core .eos-agents/pursuit/scripts`
  produced no output.
- E4 old workflow vocabulary scan:
  `rg -n "@eos/workflow|packages/workflow|delegate_workflow|workflow_context|workflowContextScript|workflowDb|workflowContextRoot|workflowScriptsDir|workflow_<id>|iteration_<id>|deferred_goal|archived/" eos-agent-core/packages .eos-agents/pursuit .eos-agents/profile -g '!node_modules/**' -g '!docs/code-inventory/**'`
  produced no matches.
- E5 active work-item/path legacy scan:
  `rg -n "\.needs\b|\bneeds\s*:|\"needs\"\s*:|work_item_spec|description\.md|focus\.md" eos-agent-core/packages/contracts/src/pursuit.ts eos-agent-core/packages/pursuit/src eos-agent-core/packages/tool/src/tools/submission eos-agent-core/packages/tool/src/tools/pursuit eos-agent-core/packages/agent-runtime/src .eos-agents/pursuit/scripts .eos-agents/profile -g '!node_modules/**'`
  produced no matches.
- E6 projection relocation: `test ! -d eos-agent-core/packages/pursuit/src/projection`
  succeeded; projection code lives under `context-engine/projection/`.

### Phase 01 - Foundation Rename and Package Layout

Acceptance checklist:

- [x] Rename `eos-agent-core/packages/workflow/` to
  `eos-agent-core/packages/pursuit/`.
- [x] Rename package metadata from `@eos/workflow` to `@eos/pursuit`.
- [x] Update workspace package references from `@eos/workflow` to
  `@eos/pursuit`.
- [x] Rename product source files according to the active file-level map:
  `launcher.ts` to `agent-launcher.ts`, every entity `transitions.ts` to
  singular `transition.ts`, `workflow-tree.ts` to `pursuit-tree.ts`, and
  `workflow-context.ts` to `pursuit-context.ts`.
- [x] Move projection code under `context-engine/projection/`:
  `context-projection.ts` to `context-engine/projection/mirror.ts`,
  `archive/listing.ts` to `context-engine/projection/listing.ts`,
  `archive/paths.ts` to `context-engine/projection/paths.ts`, and
  `archive/resolve.ts` to `context-engine/projection/resolve.ts`.
- [x] Keep package exports narrow: pursuit service, composer seam, launch port
  types, and public DTO/contract types only.
- [x] Ensure `@eos/pursuit` imports only allowed package dependencies:
  `@eos/contracts`, `@eos/db`, and standard/library modules.
- [x] Ensure `@eos/pursuit` does not import `@eos/agent-runtime`,
  `@eos/tool`, supervisor/background packages, profile loading, runtime
  composition, or engine internals.
- [x] Run a layout/import boundary scan and record evidence in the tracker.

Phase gate:

```text
Phase 01 is Complete only when package layout, package names, public exports,
and dependency direction match this spec. Phase 02 remains Blocked until then.
```

### Phase 02 - Contracts, DB Schema, and Goal Model

Acceptance checklist:

- [x] Rename public contract IDs, schemas, and DTOs from workflow/iteration to
  pursuit/leg vocabulary.
- [x] Add `CreatePursuitInput` with dynamic and predefined shapes.
- [x] Validate `leg_goal_mode` from payload shape and reject mismatches.
- [x] Treat every `leg_goals` entry as an opaque prose string, equivalent in kind
  to `pursuit_goal`.
- [x] Add `leg_goal`, `leg_goal_version`, `leg_goal_provenance`,
  `is_leg_goal_mutatable`, and `next_leg_goal` to leg contracts.
- [x] Add nullable `outcome` fields to pursuit, leg, attempt, and work-item
  snapshots.
- [x] Add work-item `Blocked` status without adding `Blocked` to pursuit, leg, or
  attempt status unions.
- [x] Replace scalar attempt `fail_reason` with list-shaped
  `failure_reasons`.
- [x] Rename DB row types and migration schema to `pursuits` / `legs`.
- [x] Persist predefined leg goals, plan declarations
  `declared_leg_goal` / `declared_next_leg_goal`, `leg_goal_version` audit
  stamps, work-item `title`, `spec`, leg-scoped dependency edges, `Blocked`
  status, and attempt failure-reason lists.
- [x] Add or update contract and schema tests covering dynamic input,
  predefined input, old field rejection, `Blocked`, and `leg_goal_version`.

Phase gate:

```text
Phase 02 is Complete only when contracts and DB schema can represent the full
Phase 05.3 model without compatibility-only workflow/iteration fields in active
product surfaces. Phase 03 remains Blocked until then.
```

### Phase 03 - Pursuit Service Creation and Declaration Semantics

Acceptance checklist:

- [x] Expose caller-agnostic `createPursuit(...)` / handle behavior with
  `pursuit_id`, `cancel(...)`, and `settle()`.
- [x] Keep human-mode router/frontend implementation out of this phase while
  avoiding agent-tool-only assumptions in the service.
- [x] Create the first leg and first attempt at pursuit creation time.
- [x] Dynamic mode sets first `leg_goal` from `pursuit_goal`.
- [x] Predefined mode sets each leg goal from `leg_goals[sequence - 1]`.
- [x] Dynamic successor legs inherit `leg_goal` from the previous successful
  leg's `next_leg_goal`.
- [x] Reject planner `leg_goal` / `next_leg_goal` declarations in predefined
  mode without consuming attempt budget.
- [x] Support dynamic refocus by `leg_goal`, incrementing `leg_goal_version`,
  relocating prior live attempts under `superseded/`, and clearing standing
  `next_leg_goal` when the same payload omits it.
- [x] Reject any attempt to clear standing `next_leg_goal` without a refocusing
  `leg_goal`.
- [x] Keep effective goal truth derived from append-only declarations, with
  `leg_goal_version` used only as audit metadata.
- [x] Add creation, refocus, predefined-mode, and declaration-derivation tests.

Phase gate:

```text
Phase 03 is Complete only when pursuit creation, dynamic/predefined leg goal
derivation, refocus, successor handling, superseded relocation, and handle
semantics all pass focused tests. Phase 04 remains Blocked until then.
```

### Phase 04 - Planner Payload and Dependency Validation

Acceptance checklist:

- [x] Replace planner work-item payload fields with `title`, `spec`, and
  `depends_on`.
- [x] Reject old planner work-item fields `description`, `work_item_spec`, and
  `needs` according to the active schema strictness policy.
- [x] Accept dynamic planner payloads that omit both `leg_goal` and
  `next_leg_goal`.
- [x] Accept successor-only dynamic payloads containing `next_leg_goal` without
  `leg_goal`.
- [x] Reject cross-attempt `depends_on` when the same planner payload submits a
  replacement `leg_goal`.
- [x] Validate `depends_on` targets against the current non-superseded
  `leg_goal_version`.
- [x] Permit `depends_on` on previous attempts only when the target is in the
  same leg, not superseded, and shares the same effective leg-goal version.
- [x] Reject `depends_on` targets from future attempts, another leg, superseded
  attempts, or earlier leg-goal versions.
- [x] Enforce work-item id uniqueness across the current attempt plus all
  non-superseded prior attempts in the same leg-goal version.
- [x] Stamp accepted work items and dependency edges with current
  `leg_goal_version` audit metadata.
- [x] Keep correctable validation failures in-run and avoid consuming attempt
  budget for correctable planner payload errors.
- [x] Add dependency-validation tests for same-attempt, prior-attempt,
  superseded, refocused, dangling, duplicate, and cyclic cases.

Phase gate:

```text
Phase 04 is Complete only when planner payload validation and materialization
can build a leg-scoped dependency graph that is unambiguous across attempts and
leg-goal versions. Phase 05 remains Blocked until then.
```

### Phase 05 - Attempt Scheduler and Failure Propagation

Acceptance checklist:

- [x] Implement scheduler operations as domain steps:
  `applyPlannerSettlement`, `applyWorkItemSettlement`,
  `propagateDependencyBlocks`, `claimReadyWorkItems`, and
  `reconcileAttemptStatus`.
- [x] Enforce the hard launch gate: a work item is claimable only when every
  direct `depends_on` target is terminal `Success`.
- [x] Recheck the launch gate in the claim query, post-commit launch guard, and
  stale-claim recheck.
- [x] Ensure a `Running` work item is never converted to `Blocked`.
- [x] Mark only `NotStarted` descendants as `Blocked` when dependency block
  propagation finds direct dependencies in `Failed` or `Blocked`.
- [x] Repeat dependency block propagation until stable so transitive dependents
  become `Blocked`.
- [x] Leave unrelated `Running` work items running after a sibling fails.
- [x] Allow unrelated `NotStarted` work items to launch later when their own
  dependencies become `Success`.
- [x] Derive attempt `Success` only when every work item is `Success`.
- [x] Derive attempt `Failed` only after at least one work item is
  `Failed` / `Blocked` and no work item remains `Running` or `NotStarted`.
- [x] Preserve planner-death behavior for attempts with no accepted work graph.
- [x] Create retry attempts only after `reconcileAttemptStatus` closes the
  current attempt `Failed` and retry budget remains.
- [x] Render and persist list-shaped failure reasons for planner failures,
  context-composition failures, failed work items, and blocked work items.
- [x] Add scheduler tests for launch gating, transitive blocking, unrelated work
  continuation, delayed failure close, retry creation, and no running-to-blocked
  transition.

Phase gate:

```text
Phase 05 is Complete only when attempt lifecycle is scheduler-derived and every
work-item dependency/failure scenario in the test matrix behaves deterministically.
Phase 06 remains Blocked until then.
```

### Phase 06 - Context Projection, Mirror, and Snapshots

Acceptance checklist:

- [x] Render context paths under `pursuit_<id>/leg_<id>/...`.
- [x] Render `leg_goal.md` at leg creation with provenance and current effective
  goal.
- [x] Render `next_leg_goal.md` only when an effective successor exists.
- [x] Render `superseded/attempt_<id>/` for attempts displaced by dynamic
  refocus.
- [x] Remove active rendered paths containing `/plan_`, `workflow_`,
  `iteration_`, `focus.md`, `deferred_goal.md`, or `archived/`.
- [x] Render work item static files as `title.md` and `spec.md`.
- [x] Render `failure_reasons.md` as a list only for failed attempts.
- [x] Render attempt, leg, and pursuit `outcome.md` according to Phase 05.2
  aggregation renamed to pursuit/leg vocabulary.
- [x] Expose snapshot `outcome` fields as `null` while their entities are not
  terminal.
- [x] Expose snapshot `leg_goal_version` audit stamps for legs, attempts, and
  work items.
- [x] Write disk mirror output under `.eos-agents/pursuit/context`.
- [x] Make context search exclude `superseded/` by default unless scoped.
- [x] Add projection, mirror, snapshot, search/listing, and creation-schedule
  tests.

Phase gate:

```text
Phase 06 is Complete only when DB-derived context projection, disk mirror,
snapshot DTOs, and search/listing semantics match the pursuit path universe.
Phase 07 remains Blocked until then.
```

### Phase 07 - Runtime, Tool, and Script Wiring

Acceptance checklist:

- [x] Rename `delegate_workflow` to `delegate_pursuit`.
- [x] Expose background session type `"pursuit"`.
- [x] Keep cancellation routed through `cancel_background_session`.
- [x] Make `delegate_pursuit` an adapter over `createPursuit(...)`.
- [x] Rename runtime config fields such as `workflowDb`,
  `workflowContextRoot`, and `workflowScriptsDir` to pursuit equivalents.
- [x] Implement the `@eos/pursuit` launch port in `@eos/agent-runtime` using
  `startRun(...)`, parent run stamping, cancellation signals, transcript wiring,
  and submission binding.
- [x] Rename profile field `workflow_context_script` to
  `pursuit_context_script`.
- [x] Move active scripts to `.eos-agents/pursuit/scripts/`.
- [x] Rewrite `planner.cjs`, `worker.cjs`, and `variable_reference_map.cjs` for
  pursuit/leg DTOs and path-addressed initial messages.
- [x] Ensure planner prompts include dynamic/predefined payload rules,
  successor-scope guidance, and the no-standalone-clear rule.
- [x] Ensure worker prompts include only assigned work, current leg goal, direct
  successful dependency outcomes, and the prohibition on planning/changing legs.
- [x] Update tool descriptions, terminal submission guidance, advisory prompts,
  runtime tests, and tool-family tests.

Phase gate:

```text
Phase 07 is Complete only when agent-launched pursuit runs use the new tool,
runtime config, launch port, scripts, and background-session vocabulary end to
end. Phase 08 remains Blocked until then.
```

### Phase 08 - Verification, Hygiene, and Legacy Removal

Acceptance checklist:

- [x] Run focused Vitest suites listed in this spec and record command output.
- [x] Run `pnpm run typecheck`.
- [x] Run `pnpm run lint`.
- [x] Run `pnpm run test`.
- [x] Run docs hygiene with `git diff --check -- docs/plans/agent-core-rust-to-typescript-migration eos-agent-core .eos-agents/pursuit/scripts`.
- [x] Run identifier-boundary scans for old active vocabulary.
- [x] Confirm no active `@eos/workflow`, `packages/workflow`,
  `delegate_workflow`, `workflow_context`, `workflow_<id>`, `iteration_<id>`,
  `focus.md`, `deferred_goal.md`, `archived/`, work-item `description.md`, or
  work-item `needs` remains in product code/scripts/tests.
- [x] Confirm historical specs/notes are the only remaining old-vocabulary
  references outside one-time migration aliases.
- [x] Confirm `packages/pursuit/src/projection` does not exist and projection
  lives under `context-engine/projection/`.
- [x] Update this tracker with final evidence and mark Phase 08 `Complete`.

Phase gate:

```text
Phase 08 is Complete only when focused checks, workspace checks, docs hygiene,
and identifier scans all pass or any remaining noise is documented as
pre-existing and unrelated.
```

## 1. Intent

Phase 05.1/05.2 implemented the durable planner/worker orchestration spine, but
its vocabulary still exposes the losing `workflow / iteration / deferred_goal`
model. Phase 05.3 replaces that vocabulary with the judged
`pursuit / leg / attempt` model and removes the separate `focus` concept.

The end-state behavior is:

- a delegated root objective is a `pursuit`,
- a vertical continuation unit is a `leg`,
- retries remain `attempt`s inside a leg,
- each leg has an effective `leg_goal` from creation time,
- `next_leg_goal` is the optional successor gate,
- a planner may omit `leg_goal` to accept the current leg goal,
- a planner submits `leg_goal` only to refocus the current leg,
- superseded attempts move under `superseded/`, not `archived/`.

This is a vocabulary and contract phase. It keeps the Phase 05 launch,
submission, retry, cancellation, mirror, and outcome mechanics unless this spec
explicitly replaces a name or validation rule.

## 2. Non-Negotiable Boundary

Initial-message scripting moves under the pursuit script root:

```text
/Users/yifanxu/machine_learning/LoVC/EphemeralOS/.eos-agents/pursuit/scripts
```

Repo-relative:

```text
.eos-agents/pursuit/scripts/
  planner.cjs
  worker.cjs
  variable_reference_map.cjs
```

The directory name, script content, input DTOs, variable map, and emitted
messages must all use pursuit vocabulary. Do not retain
`.eos-agents/workflow/scripts/` as an active profile or runtime script root.

## 3. Vocabulary Decisions

| Old surface | New surface | Notes |
| --- | --- | --- |
| workflow | pursuit | Product/session/tool/context vocabulary. |
| iteration | leg | Ordered vertical continuation unit. |
| attempt | attempt | Retry unit remains unchanged. |
| `iteration_focus` | removed | No separate focus concept. |
| `focus.md` | removed | Replaced by `leg_goal.md`. |
| `deferred_goal` | `next_leg_goal` | Successor gate. |
| `archived/` | `superseded/` | Attempts displaced by a later `leg_goal`. |
| `workflow_context` | `pursuit_context` | Script input root and DTO naming. |
| `workflow_context_script` | `pursuit_context_script` | Profile field; resolved under `.eos-agents/pursuit/scripts/`. |
| `delegate_workflow` | `delegate_pursuit` | Tool family becomes pursuit-facing. |
| background session type `"workflow"` | `"pursuit"` | Cancellation still rides `cancel_background_session`. |
| `@eos/workflow` / `packages/workflow` | `@eos/pursuit` / `packages/pursuit` | Package-level vocabulary follows product vocabulary. |

Allowed old spelling after Phase 05.3:

- historical spec/note text,
- migration aliases inside a one-time migration script, if a migration script is
  required.

Everything else in the TypeScript product surface should use pursuit terms.

## 4. Public API and Caller Model

`pursuit` is a caller-agnostic orchestration package. Phase 05.3 must expose a
service surface that an agent tool, machine scheduler, test harness, or future
server router can call without a new orchestration model. This phase does not
implement a human frontend or human-mode router; it only keeps that later wiring
cheap by avoiding agent-tool-only assumptions in the core service.

The public creation surface should be narrow:

```ts
type CreatePursuitInput =
  | {
      pursuit_goal: string;
      leg_goal_mode?: "dynamic";
      leg_goals?: undefined;
    }
  | {
      pursuit_goal: string;
      leg_goal_mode?: "predefined";
      leg_goals: readonly [string, ...string[]];
    };

interface PursuitHandle {
  pursuit_id: string;
  cancel(reason?: string): Promise<void>;
  settle(): Promise<PursuitSettlement>;
}
```

The handle semantics are independent of who called it:

- `cancel` cancels the pursuit and all non-terminal descendants.
- `settle` resolves when the pursuit reaches `Success`, `Failed`, or
  `Cancelled`.
- Background-supervisor registration remains a runtime/tool adapter concern;
  the pursuit package exposes a terminal handle that callers can register.

Package dependency rule:

```text
@eos/pursuit owns orchestration, DB-backed state transitions, and the launch
port type used to start planner and worker agents. It may depend on
@eos/contracts and @eos/db. It must not import @eos/agent-runtime, @eos/tool,
runtime composition, profile loading, supervisor state, or tool registration.

@eos/agent-runtime imports @eos/pursuit and implements the pursuit launch port
with startRun(...), profile resolution, transcript wiring, cancellation, and
submission binding.
```

`leg_goal_mode` is derived from the payload shape: omitting `leg_goals` selects
`"dynamic"`; providing non-empty `leg_goals` selects `"predefined"`. An explicit
`leg_goal_mode` may be accepted for diagnostics, but a mismatch between
`leg_goal_mode` and `leg_goals` must be rejected.

## 5. Leg Goal Modes

| Leg goal mode | Creation input | Leg goal source | Planner declaration rule | Next-leg rule |
| --- | --- | --- | --- | --- |
| Dynamic | `create_pursuit({ pursuit_goal })` | First leg inherits `pursuit_goal`; later legs inherit the previous successful leg's `next_leg_goal`. | Planner may omit `leg_goal`, submit `leg_goal` to refocus, and submit successor-only `next_leg_goal`. | A new leg is created only when the successful current leg has an effective `next_leg_goal`. |
| Predefined | `create_pursuit({ pursuit_goal, leg_goals })` | Each leg uses the caller-provided `leg_goals[sequence - 1]`. | Planner must not submit `leg_goal` or `next_leg_goal`; refocus is disallowed. | A new leg is created from the next predefined `leg_goals` entry until the list is exhausted. |

Predefined mode is for callers that already know the ordered leg list. In that
mode, `pursuit_goal` remains the umbrella objective, while `leg_goals` are the
fixed execution checkpoints.

Each `leg_goals` entry is the same kind of pure prose objective string as
`pursuit_goal`. The pursuit layer treats these strings as opaque instructions:
it trims only according to normal schema validation, does not parse structure
from them, and does not infer hierarchy from repeated or similar prose.

Dynamic mode is the default. It preserves the current Phase 05 behavior where
each successful leg may discover exactly one successor goal.

The `delegate_pursuit` prompt should not present both modes as equally common:

```text
Use dynamic leg goals by default. Provide only pursuit_goal when the planner
should discover or refocus legs during execution.

Use predefined leg goals only when the caller already knows the complete ordered
leg list. Provide pursuit_goal and leg_goals. In this mode planners cannot
submit leg_goal or next_leg_goal.
```

## 6. Leg Goal Model

`leg_goal` is the current effective goal of a leg.

Dynamic creation rule:

```text
first leg:
  leg_goal = pursuit.goal

next leg:
  leg_goal = previous successful leg.next_leg_goal
```

Predefined creation rule:

```text
leg_n:
  leg_goal = pursuit.leg_goals[n - 1]
```

Dynamic planner submission rule:

```text
leg_goal omitted:
  keep the current leg_goal

leg_goal present:
  replace the current leg_goal and mark older live attempts in the leg as
  superseded

next_leg_goal omitted:
  keep any standing next_leg_goal when leg_goal is also omitted

next_leg_goal present:
  if the leg succeeds, create the next leg with that value as its leg_goal

leg_goal present and next_leg_goal omitted:
  refocus the leg and reset the standing next_leg_goal to absent
```

There is no dynamic payload shape for clearing a standing `next_leg_goal`
without also submitting `leg_goal`. Clearing successor scope is a refocus act:
the planner must declare the replacement `leg_goal`, and omission of
`next_leg_goal` in that same payload clears the standing successor.

Success invariant:

```text
Success means the full effective leg_goal was achieved.
Success never means "leg_goal minus next_leg_goal".
```

Dynamic planner prompt invariant:

```text
If you cannot achieve the full leg_goal in this leg, submit a narrowed leg_goal
and defer the remainder as next_leg_goal.
```

Dynamic validation stays intentionally loose: `next_leg_goal` is valid without
a sibling `leg_goal`. The planner may complete the full current `leg_goal` while
declaring newly discovered successor scope.

Predefined planner submission rule:

```text
leg_goal present:
  reject; predefined leg goals cannot be refocused

next_leg_goal present:
  reject; predefined leg goals own the next-leg sequence

both omitted:
  accepted; planner moves directly to planning work items for the current
  predefined leg_goal
```

## 7. Effective Declaration Semantics

Plans remain execution state and the planner submission binding point. They are
not rendered as context folders.

Each plan may carry a declaration:

| Plan declaration field | Meaning |
| --- | --- |
| `declared_leg_goal` | Replace the current leg goal from this attempt onward. |
| `declared_next_leg_goal` | Set the successor goal for this leg. |
| declaration absent | Keep current `leg_goal` and standing `next_leg_goal`. |

The declaration view is append-only and ordered by attempt sequence. An attempt
is consistent with the current leg goal iff no later declaration in the same leg
submitted `leg_goal`.

Each leg also exposes a monotonic audit stamp called `leg_goal_version`:

```text
leg_goal_version starts at 1 when the leg is created.
dynamic refocus by declared_leg_goal increments it by 1.
predefined legs keep one version for the whole leg.
```

`leg_goal_version` is audit metadata, not the source of goal truth. Effective
goal and successor values still derive from the append-only declarations below.
The service stamps the current version onto attempts, plans, work items, and
dependency edges so tests, logs, and context snapshots can prove which leg-goal
version a decision belonged to.

Dynamic effective values:

```text
base_leg_goal(leg_1) = pursuit.goal
base_leg_goal(leg_n) = effective_next_leg_goal(leg_n-1)

effective_leg_goal(leg) =
  latest declared_leg_goal in leg, if present
  otherwise base_leg_goal(leg)

effective_next_leg_goal(leg) =
  latest declaration in leg that touched leg_goal or next_leg_goal:
    - if it declared next_leg_goal, that value
    - if it declared leg_goal but not next_leg_goal, absent
  otherwise absent
```

Predefined effective values:

```text
effective_leg_goal(leg_n) = pursuit.leg_goals[n - 1]
effective_next_leg_goal(leg_n) =
  pursuit.leg_goals[n], if present
  otherwise absent
```

In predefined mode, plan declarations for `declared_leg_goal` and
`declared_next_leg_goal` must remain absent. The next-leg preview is derived
from the caller-provided list, not from planner output.

`leg_goal.md` must include a provenance line:

```text
<effective leg goal>

Provenance: <inherited from pursuit goal | inherited from successful leg_<n> next_leg_goal | declared by attempt_<id> planner | predefined leg_goal[<n>]>
```

## 8. Context Path Universe

Rendered context paths switch from `workflow_<id>/iteration_<id>/...` to
`pursuit_<id>/leg_<id>/...`.

```text
pursuit_<id>/
  goal.md
  outcome.md                               pursuit Success/Failed; Cancelled marker only

  leg_<id>/
    leg_goal.md                            effective leg goal plus provenance; appears at leg creation
    next_leg_goal.md                       effective successor gate; absent if none
    outcome.md                             Success or final Failed only

    attempt_<id>/                          is_consistent_with_leg_goal only
      plan_summary.md                      accepted planner summary; absent on planner death
      failure_reasons.md                   failed attempts only; one entry per failed/blocked work item
      outcome.md                           successful or failed attempts only
      work_item_<id>/
        title.md                           accepted planner work-item title
        spec.md                            accepted planner work-item spec
        summary.md                         worker submitted summary
        outcome.md                         worker submitted or system terminal outcome

    superseded/
      attempt_<id>/                        displaced attempt, relocated whole
        leg_goal.md                        only if this attempt declared superseded leg_goal
        next_leg_goal.md                   only if that declaration carried one
        plan_summary.md                    same attempt-owned file as live shape
        failure_reasons.md
        outcome.md
        work_item_<id>/
          title.md
          spec.md
          summary.md
          outcome.md
```

Rules:

- No rendered path contains `/plan_`.
- No rendered path contains `workflow_`, `iteration_`, `focus.md`,
  `deferred_goal.md`, or `archived/`.
- Disk mirror context lives under `.eos-agents/pursuit/context/pursuit_<id>/`.
  No active context mirror path should remain under `.eos-agents/workflow/`.
- `leg_goal.md` appears at leg creation, before planner submission.
- `next_leg_goal.md` is absent until a dynamic declaration or later predefined
  leg exists; absence means no successor.
- Superseded attempts preserve the same attempt-owned files as live attempts.
- Declaration files under `superseded/attempt_<id>/` exist only on the attempt
  whose planner declared the displaced value.
- Status stays in `ContextPage` and listing rows, not file content.
- Search excludes `superseded/` by default unless scope explicitly names it.

## 9. Outcome Rendering

Outcome aggregation remains Phase 05.2 behavior with renamed headings:

| Old | New |
| --- | --- |
| Attempt outcome | Attempt outcome |
| Iteration outcome | Leg outcome |
| Workflow outcome | Pursuit outcome |

Attempt outcome:

```text
# Attempt outcome
- work_item_<id> [Success]: <worker_summary>
- work_item_<id> [Failed]: <worker_summary>
- work_item_<id> [Blocked]: blocked by work_item_<dependency_id>
- work_item_<id> [Cancelled]: (no summary)
```

Leg outcome:

```text
<closing attempt outcome content>
```

Pursuit outcome:

```text
# Pursuit outcome

## leg_<id> [Success]
<leg outcome content>

## leg_<id> [Failed]
<leg outcome content>
```

Cancelled pursuit marker:

```text
# Pursuit outcome
pursuit cancelled
```

Work-item scheduler state machine:

```text
A failed work item does not immediately cancel unrelated siblings.
```

Work items use the normal entity run statuses plus `Blocked`:

| Status | Meaning | Terminal for work item? |
| --- | --- | --- |
| `NotStarted` | Accepted by the planner but not launched. | No |
| `Running` | Claimed and launched. | No |
| `Success` | Worker submitted a passing result. | Yes |
| `Failed` | Worker submitted a failing result or died/failed before submission. | Yes |
| `Blocked` | Never launched because at least one hard dependency cannot succeed. | Yes |
| `Cancelled` | Cancel cascade reached the work item. | Yes |

Dependency policy:

```text
A work item may be claimed for launch only when every direct depends_on target
has terminal status Success.
```

This is a hard scheduler invariant, not prompt guidance. The launch claim query,
post-commit launch guard, and stale-claim recheck must all enforce it. A work
item whose dependency is not fully successful stays `NotStarted`; it never starts
optimistically.

Consequence:

```text
A running work item can never become Blocked because one of its dependencies
failed. If a dependency later fails, the dependent item was not running yet.
```

If implementation finds a `Running` work item whose dependency is not `Success`,
that is a state-corruption bug. The scheduler must not "repair" it by marking the
running item `Blocked`; it should let the running item finish or be cancelled by
an explicit cancel path and surface the invariant violation through tests/logs.

The attempt scheduler runs these domain operations after any planner or worker
settlement mutation:

```text
1. applyPlannerSettlement or applyWorkItemSettlement
   - planner success creates the accepted NotStarted work graph
   - planner failure records the planner failure source
   - worker success/failure/death records the work-item settlement
2. propagateDependencyBlocks
   - derive Blocked NotStarted descendants from Failed/Blocked dependencies
3. claimReadyWorkItems
   - claim NotStarted work whose direct dependencies are all Success
4. reconcileAttemptStatus
   - derive Success, Failed, or keep Running
```

After `applyWorkItemSettlement` records a work item as `Failed`, the scheduler
runs `propagateDependencyBlocks` over the same attempt:

1. Find every `NotStarted` work item with at least one direct dependency in
   `Failed` or `Blocked`.
2. Mark those work items `Blocked` and write an outcome explaining the failed or
   blocked dependency.
3. Repeat until no additional `NotStarted` work item can be blocked.
4. Leave `Running`, `Success`, `Failed`, and `Cancelled` work items unchanged.

`propagateDependencyBlocks` repeats until stable so transitive dependency chains
are handled without giving `depends_on` non-blocking historical semantics. If A
fails, B depends on A, and C depends on B, the pass marks B `Blocked`, then C
`Blocked`.

The scheduler then runs `claimReadyWorkItems`:

```text
claimable(work_item) =
  work_item.status == NotStarted
  and attempt.status == Running
  and every direct depends_on target is Success
```

Unrelated running work-item agents continue. Unrelated pending work items launch
when their own dependencies become `Success`.

`reconcileAttemptStatus` runs after dependency block propagation and ready-work
claiming:

```text
if plan failed before accepted work items:
  attempt = Failed

else if every work item is Success:
  attempt = Success

else if any work item is Failed or Blocked
  and no work item is Running
  and no work item is NotStarted:
    attempt = Failed

else:
  attempt remains Running
```

The "no `NotStarted`" condition is evaluated after
`propagateDependencyBlocks`. A `NotStarted` item with failed dependencies should
already have become `Blocked`; a `NotStarted` item with incomplete but
still-possible dependencies keeps the attempt open. This keeps retry creation
aligned with actual exhaustion: retries start only after there is no ready,
running, or still-possible work left in the attempt.

`failure_reasons.md` is a list, not a scalar. It includes every attempt-level
failure source. For work execution, it includes every failed work item and every
work item blocked by a failed dependency:

```text
# Failure reasons

- work_item_<id> [Failed]: <summary or outcome first line>
- work_item_<id> [Blocked]: blocked by work_item_<dependency_id>
```

## 10. Planner Payload Contract

`submit_planner_outcome` remains the terminal planner submission tool unless a
later phase renames all terminal tools. Its payload changes vocabulary:

```ts
const PlannerWorkItemSpecSchema = z.object({
  id: z.string().min(1),
  agent_name: z.string().min(1),
  title: z.string().min(1),
  spec: z.string().min(1),
  depends_on: z.array(z.string()).default([]),
});

const PlannerOutcomePayloadSchema = z.object({
  summary: z.string().min(1),
  leg_goal: z.string().min(1).optional(),
  next_leg_goal: z.string().min(1).optional(),
  work_items: z.array(PlannerWorkItemSpecSchema).min(1),
});
```

`depends_on` is leg-scoped, not attempt-scoped. It is both an execution edge and
a context-injection edge.

`depends_on` entries resolve by work-item id within the current non-superseded
leg-goal version. To keep that reference unambiguous, accepted planner work-item
ids must be unique across the current attempt plus all non-superseded prior
attempts that share the same effective leg-goal version.

Every accepted work item and persisted dependency edge is stamped with the
current `leg_goal_version` for audit. Dependency validation still derives the
current non-superseded leg-goal version from leg declarations; the stamp proves
what the service accepted and makes logs/tests readable.

Allowed `depends_on` targets:

- a work item from the same attempt,
- a work item from an earlier attempt in the same leg, only when that earlier
  attempt is not under `superseded/` and shares the same effective leg-goal
  version.

Rejected `depends_on` targets:

- work items from another leg,
- work items from a future attempt,
- work items from a superseded attempt,
- work items from an earlier leg-goal version after a dynamic refocus,
- work items from any previous attempt when the planner submits a new `leg_goal`
  in the same payload.

If the planner needs non-blocking historical context from a failed or superseded
work item, that must be modeled later as a separate `context_refs` field.
`depends_on` remains a hard dependency: the dependent work item can run only
after all dependencies have terminal `Success`.

Dynamic-mode validation changes:

| Case | Result |
| --- | --- |
| first planner omits `leg_goal` | accepted; uses existing leg goal |
| `next_leg_goal` without `leg_goal` | accepted |
| `leg_goal` without `next_leg_goal` | accepted; refocuses and clears standing successor |
| neither `leg_goal` nor `next_leg_goal` | accepted; keeps both current values |
| request to clear `next_leg_goal` without `leg_goal` | rejected; successor clearing requires refocus |
| unknown worker `agent_name` | rejected in-run |
| duplicate/dangling/cyclic work-item ids or invalid `depends_on` ids in the current leg-goal version | rejected in-run |
| cross-attempt `depends_on` plus new `leg_goal` in same payload | rejected in-run |
| old work-item `description` or `needs` fields | rejected in-run |

Predefined-mode validation changes:

| Case | Result |
| --- | --- |
| first planner omits `leg_goal` and `next_leg_goal` | accepted; uses the predefined current leg goal |
| retry planner omits `leg_goal` and `next_leg_goal` | accepted; uses the predefined current leg goal |
| planner submits `leg_goal` | rejected in-run; no attempt budget is consumed for a correctable payload error |
| planner submits `next_leg_goal` | rejected in-run; no attempt budget is consumed for a correctable payload error |
| successful non-final predefined leg | next leg is created from the next `leg_goals` entry |
| successful final predefined leg | pursuit closes `Success` |

The tool result on success should keep returning the summary payload as today.

## 11. Script Input DTOs

Context scripts receive pursuit-named DTOs on stdin. The script directory is
`.eos-agents/pursuit/scripts/`.

```ts
interface PlannerPursuitContextInput {
  kind: "planner";
  pursuit_context: PursuitContextSnapshot;
  current: {
    pursuit_id: string;
    leg_id: string;
    attempt_id: string;
    plan_id: string;
  };
}

interface WorkerPursuitContextInput {
  kind: "worker";
  pursuit_context: PursuitContextSnapshot;
  current: {
    pursuit_id: string;
    leg_id: string;
    attempt_id: string;
    work_item_id: string;
  };
}
```

Snapshot shape:

```ts
type PursuitWorkItemRunStatus = PursuitEntityRunStatus | "Blocked";

interface AttemptFailureReason {
  work_item_id: string | null;
  kind:
    | "planner_failed"
    | "context_composition_failed"
    | "failed"
    | "blocked_by_failed_dependency";
  message: string | null;
  summary: string | null;
  outcome: string | null;
  blocked_by?: string[];
}

interface PursuitContextSnapshot {
  pursuit: {
    id: string;
    goal: string;
    leg_goal_mode: "dynamic" | "predefined";
    predefined_leg_count: number | null;
    status: PursuitEntityRunStatus;
    outcome: string | null;
    context_path: string; // pursuit_<id>
    legs: PursuitContextLeg[];
  };
}

interface PursuitContextLeg {
  id: string;
  sequence: number;
  origin: "initial" | "next_leg_goal" | "predefined";
  status: PursuitEntityRunStatus;
  leg_goal: string;
  leg_goal_version: number;
  leg_goal_provenance: string;
  is_leg_goal_mutatable: boolean;
  next_leg_goal: string | null;
  max_attempts: number;
  outcome: string | null;
  context_path: string;
  attempts: PursuitContextAttempt[];
}

interface PursuitContextAttempt {
  id: string;
  sequence: number;
  status: PursuitEntityRunStatus;
  leg_goal_version: number;
  failure_reasons: AttemptFailureReason[];
  is_consistent_with_leg_goal: boolean;
  outcome: string | null;
  context_path: string;
  plan: PursuitContextPlan;
  work_items: PursuitContextWorkItem[];
}

interface PursuitContextPlan {
  id: string;
  status: PursuitEntityRunStatus;
  agent_run_id: string | null;
  summary: string | null;
  declared_leg_goal: string | null;
  declared_next_leg_goal: string | null;
}

interface PursuitContextWorkItem {
  id: string;
  agent_name: string;
  status: PursuitWorkItemRunStatus;
  agent_run_id: string | null;
  leg_goal_version: number;
  title: string;
  spec: string;
  depends_on: string[];
  summary: string | null;
  outcome: string | null;
  context_path: string;
}
```

`PursuitContextPlan` keeps plan metadata but no plan `context_path`, matching
Phase 05.2. `leg_goal_version` fields are audit stamps for the version active
when the row was created or accepted. `PursuitContextWorkItem.depends_on`
contains work-item ids scoped to the current non-superseded leg-goal version, so
it may reference prior attempts inside the same leg when the leg goal has not
been refocused. Snapshot
`outcome` fields are `null` until their owning attempt, leg, pursuit, or work
item reaches a terminal condition and the corresponding `outcome.md` content
exists. Snapshot `failure_reasons` may accumulate while the attempt is still
running after a work item failure; rendered `failure_reasons.md` appears only
when the attempt closes `Failed`.

## 12. Initial Message Scripting

The runtime must resolve profile-selected context scripts under:

```text
.eos-agents/pursuit/scripts/
```

Expected files:

```text
.eos-agents/pursuit/scripts/planner.cjs
.eos-agents/pursuit/scripts/worker.cjs
.eos-agents/pursuit/scripts/variable_reference_map.cjs
```

The existing scripts currently use workflow/iteration variable names. Phase 05.3
must move them to the pursuit script folder and update them to produce
pursuit/leg messages from the new `pursuit_context` DTO.

Script output stays hook-parity JSON:

```json
{
  "initial_messages": [
    {
      "role": "user",
      "content": [{ "type": "text", "text": "<message 1>" }]
    }
  ]
}
```

Planner launch messages:

1. pursuit and current leg context,
2. current leg evidence,
3. planner directive.

Worker launch messages:

1. leg and attempt context,
2. assigned work and dependencies,
3. worker directive.

The exact content contract is the one recorded in
`note/planner-worker-initial-messages.md`.

## 13. Initial Message Directive Invariants

Dynamic-mode planner messages must include:

```text
A new dynamic leg exists only because the previous leg closed successfully and
declared next_leg_goal.
```

All planner messages must include:

```text
Success means the full effective leg_goal is achieved.
```

Dynamic-mode planner messages must also include:

```text
If you cannot achieve the full leg_goal in this leg, submit a narrowed leg_goal
and put the remainder in next_leg_goal.
```

Predefined-mode planner messages must also include:

```text
If the predefined leg_goal is too broad or wrong, do not submit leg_goal or
next_leg_goal. Plan only work that completes the current predefined leg_goal.
```

Planner payload guidance:

```text
Dynamic mode:
- Omit leg_goal when you accept the current leg_goal.
- Include leg_goal only to refocus this leg.
- Refocus supersedes prior live attempts and resets the standing next_leg_goal.
- Include next_leg_goal only for work that should become a future leg after
  this leg succeeds.
- next_leg_goal is a goal to be planned later; it is never a plan and never a
  summary of work delivered by this leg.

Predefined mode:
- The caller predefined this leg_goal.
- Omit leg_goal and next_leg_goal.
- Do not refocus this leg.
- Do not declare future legs; the predefined list owns leg progression.
```

Worker messages must include:

```text
Stay inside the current leg_goal and this work item. Do not plan new legs,
change leg_goal, or decide next_leg_goal.
```

## 14. Implementation Boundary

Expected package changes:

| Package | Required work |
| --- | --- |
| `@eos/contracts` | Rename workflow/iteration DTOs and ids to pursuit/leg; remove focus fields; add `leg_goal`, `leg_goal_version`, `leg_goal_provenance`, `is_leg_goal_mutatable`, `next_leg_goal`, nullable snapshot `outcome`, work-item `Blocked`, and attempt `failure_reasons`; update planner payload schema to use work-item `title` and leg-scoped `depends_on`. |
| `@eos/db` | Rename row types and migration schema to `pursuits` / `legs`; replace `origin: "deferred_goal"` with `"next_leg_goal"` and add `"predefined"` where the leg-goal mode requires it; replace plan declaration columns with `declared_leg_goal` / `declared_next_leg_goal`; persist work-item `title`, leg-scoped dependency edges, `leg_goal_version` audit stamps, blocked status, and attempt failure-reason lists. |
| `@eos/pursuit` | Rename `packages/workflow` and package name from `@eos/workflow`; expose caller-agnostic create/cancel/settle handles; define the planner/worker launch port; use `@eos/db` for the authoritative store; derive effective leg goal and successor goal for dynamic and predefined modes; validate leg-scoped dependency edges by non-superseded leg-goal version; render `pursuit_<id>` / `leg_<id>` / `superseded`; preserve plan flattening and delayed attempt-failure behavior. |
| `@eos/tool` | Rename `delegate_workflow` to `delegate_pursuit`; accept `pursuit_goal` and optional `leg_goals`; expose background session type `"pursuit"`; update planner tool prompt and payload content. |
| `@eos/agent-runtime` | Wire pursuit service and context input DTOs; implement the `@eos/pursuit` launch port with `startRun(...)`; rename runtime config such as `workflowDb` / `workflowContextRoot` to pursuit equivalents; load `.eos-agents/pursuit/scripts`; profile-selected scripts emit pursuit initial messages. |
| `.eos-agents/pursuit/scripts` | Move/rewrite `planner.cjs`, `worker.cjs`, and `variable_reference_map.cjs` to use pursuit/leg names and the Phase 05.3 initial-message contract. |

Target package tree:

```text
packages/pursuit/
  package.json
  src/
    index.ts
    service.ts
    agent-launcher.ts
    pursuit-tree.ts
    pursuit-context.ts
    pursuit/
      context.ts
      state.ts
      transition.ts
    leg/
      context.ts
      state.ts
      transition.ts
    attempt/
      context.ts
      state.ts
      transition.ts
    plan/
      state.ts
      transition.ts
    work-item/
      context.ts
      state.ts
      transition.ts
    context-engine/
      composer.ts
      input.ts
      projection/
        listing.ts
        paths.ts
        resolve.ts
        mirror.ts
  tests/
    context.test.ts
    lifecycle.test.ts
    mirror.test.ts
    package-boundary.test.ts
    support.ts
```

Expected file-level rename map:

| Current | Target |
| --- | --- |
| `packages/workflow/` | `packages/pursuit/` |
| `workflow/context.ts` | `pursuit/context.ts` |
| `iteration/context.ts` | `leg/context.ts` |
| `iteration/state.ts` | `leg/state.ts` |
| `iteration/transitions.ts` | `leg/transition.ts` |
| `workflow/state.ts` | `pursuit/state.ts` |
| `workflow/transitions.ts` | `pursuit/transition.ts` |
| `attempt/transitions.ts` | `attempt/transition.ts` |
| `plan/transitions.ts` | `plan/transition.ts` |
| `work-item/transitions.ts` | `work-item/transition.ts` |
| `workflow-tree.ts` | `pursuit-tree.ts` |
| `workflow-context.ts` | `pursuit-context.ts` |
| `context-projection.ts` | `context-engine/projection/mirror.ts` |
| `archive/listing.ts` | `context-engine/projection/listing.ts` |
| `archive/paths.ts` | `context-engine/projection/paths.ts` |
| `archive/resolve.ts` | `context-engine/projection/resolve.ts` |
| `tools/workflow/delegate-workflow.ts` | `tools/pursuit/delegate-pursuit.ts` |

Avoid compatibility shims unless needed for a single migration boundary. If a
shim is unavoidable, delete it in the same phase after callers are moved.

## 15. Logical Creation Schedule

| Time | Event | Stable Assertion Meaning |
| --- | --- | --- |
| T0 | `delegate_pursuit` commits | `pursuit_<id>/`, first `leg_<id>/`, first `attempt_<id>/`, `goal.md`, and `leg_goal.md` exist. In dynamic mode the first `leg_goal.md` inherits `pursuit_goal`; in predefined mode it uses `leg_goals[0]`. No work items or summary files exist. |
| T1 | Planner submits valid dynamic payload | `plan_summary.md` appears; work-item directories and static files appear; `next_leg_goal.md` appears or updates if declared. |
| T1P | Planner submits valid predefined payload | `plan_summary.md` appears; work-item directories and static files appear; `leg_goal.md` remains predefined; `next_leg_goal.md` is derived only from the next predefined entry when one exists. |
| T1R | Dynamic planner submits replacement `leg_goal` | Prior live attempts relocate under `superseded/`; `leg_goal.md` updates; standing `next_leg_goal.md` resets unless the same payload declares a new one. |
| T1PR | Predefined planner submits `leg_goal` or `next_leg_goal` | Submission is rejected as a correctable payload error; no work items appear and no attempt budget is consumed. |
| T1F | Planner dies or context composition fails before valid payload | Attempt fails with `failure_reasons.md`; `plan_summary.md` is absent; attempt `outcome.md` appears with `(no work items)`; retry attempt appears if budget remains. |
| T2 | Worker submits one work item result | That work item gains `summary.md` and `outcome.md`; attempt stays running unless all completion rules are satisfied. |
| T3 | All work items in an attempt succeed | Attempt becomes `Success`; attempt `outcome.md` appears; leg closes or promotes according to dynamic `next_leg_goal` or the predefined list. |
| T4 | A work item fails | That work item gains `summary.md` and `outcome.md`; pending dependents become `Blocked`; unrelated running or launchable work items continue. Attempt remains running while any unrelated work item can still run. |
| T4B | Failed dependencies block remaining work | Blocked work items gain `outcome.md` explaining the failed dependency; they do not launch workers. |
| T4C | Dependency block propagation leaves no `Running` or `NotStarted` work | Attempt becomes `Failed`; `failure_reasons.md` lists all failed and blocked work items; attempt `outcome.md` appears. |
| T5 | Failed attempt has retry budget left | New retry attempt directory appears; leg remains running; no leg outcome yet. |
| T6 | Failed attempt exhausts retry budget | Leg becomes `Failed`; leg `outcome.md` appears; pursuit becomes `Failed`; pursuit `outcome.md` appears. |
| T7 | Successful dynamic leg has `next_leg_goal` | Current leg `outcome.md` appears; next leg and its first attempt directory appear with `leg_goal.md` inherited from previous successful leg's `next_leg_goal`. |
| T7P | Successful predefined leg has another predefined goal | Current leg `outcome.md` appears; next leg and its first attempt directory appear with `leg_goal.md` from the next `leg_goals` entry. |
| T8 | Successful dynamic leg has no `next_leg_goal` | Leg `outcome.md` appears; pursuit becomes `Success`; pursuit `outcome.md` appears. |
| T8P | Successful predefined final leg | Leg `outcome.md` appears; pursuit becomes `Success`; pursuit `outcome.md` appears. |
| T10 | Pursuit is cancelled | Non-terminal entities become `Cancelled`; business outcome files are not created for cancelled attempts/legs; pursuit cancellation marker may appear. |

## 16. Unit Test Matrix

Each row should be covered by a focused Vitest case or an `it.each` case table.
Prefer package-local unit tests over broad e2e unless the assertion requires
real runtime wiring.

| ID | Test target | Scenario | Assertions |
| --- | --- | --- | --- |
| C01 | `@eos/contracts` planner payload | Dynamic payload omits `leg_goal` and `next_leg_goal`. | Schema accepts; parsed payload has no focus/deferred fields. |
| C02 | `@eos/contracts` planner payload | Dynamic payload has `next_leg_goal` without `leg_goal`. | Schema accepts successor-only declaration. |
| C03 | `@eos/contracts` planner payload | Payload uses old `iteration_focus`, `focus`, or `deferred_goal`. | Schema rejects or strips old fields according to existing strictness policy; no public type exports them. |
| C04 | `@eos/contracts` creation payload | `create_pursuit` dynamic input has only `pursuit_goal`. | Schema accepts and resolves `leg_goal_mode` to `dynamic`. |
| C05 | `@eos/contracts` creation payload | Predefined input has non-empty `leg_goals`. | Schema accepts and resolves `leg_goal_mode` to `predefined`. |
| C06 | `@eos/contracts` creation payload | Predefined input has empty `leg_goals`. | Schema rejects; predefined mode never starts without a first predefined leg. |
| C07 | `@eos/contracts` work-item payload | Work item uses `title` and leg-scoped `depends_on`. | Schema accepts `title`/`depends_on` and rejects old `description`/`needs` fields. |
| C08 | `@eos/contracts` work-item status | Work item is blocked by a failed dependency. | Work-item status accepts `Blocked`; attempt/leg/pursuit status unions do not gain `Blocked`. |
| C09 | `@eos/pursuit` dependency validation | Planner references previous-attempt work items in `depends_on`. | Accepted only when the target attempt is in the same leg, not superseded, and shares the same effective leg-goal version. |
| C10 | `@eos/pursuit` dependency validation | Planner submits new `leg_goal` and cross-attempt `depends_on` together. | Rejected because refocus breaks the previous leg-goal version. |
| B01 | `@eos/pursuit` package boundary | Package imports are inspected. | Package may import `@eos/db` and `@eos/contracts`; package does not import `@eos/agent-runtime`, `@eos/tool`, supervisor/background packages, runtime composition, profile loading, or engine internals. |
| B02 | Source/file layout | Renamed files and folders are inspected. | `packages/pursuit`, `agent-launcher.ts`, singular `transition.ts`, and `context-engine/projection/` exist; `packages/workflow`, `launcher.ts`, `transitions.ts`, top-level `projection/`, and source `archive/` do not remain active. |
| A01 | Pursuit creation | Dynamic pursuit starts with `pursuit_goal`. | First leg exists immediately; `leg_goal.md` equals `pursuit_goal`; provenance is inherited from pursuit goal; `is_leg_goal_mutatable` is true. |
| A02 | Planner submission | First dynamic planner omits `leg_goal`. | Submission succeeds; work items are created against existing `leg_goal`; no refocus occurs. |
| A03 | Planner submission | Dynamic planner submits successor-only `next_leg_goal`. | Submission succeeds; `next_leg_goal.md` appears; successful leg creates next leg with that value as `leg_goal`. |
| A04 | Planner submission | Dynamic retry omits both goal fields after a standing successor exists. | Current `leg_goal` and standing `next_leg_goal` are preserved. |
| A05 | Planner submission | Dynamic planner submits new `leg_goal` without `next_leg_goal`. | Current leg refocuses; prior live attempts move to `superseded/`; standing `next_leg_goal` is cleared. |
| A06 | Planner submission | Dynamic planner submits new `leg_goal` and `next_leg_goal`. | Current leg refocuses; prior live attempts move to `superseded/`; new successor is set. |
| A07 | Declaration derivation | Multiple dynamic declarations touch goals. | Latest declaration wins; `leg_goal_version` increments only on refocus; `is_consistent_with_leg_goal` is false only for displaced attempts. |
| A08 | Planner failure | Planner dies before valid dynamic payload. | Attempt fails with `failure_reasons.md` containing a planner/context failure reason; `plan_summary.md` is absent; retry budget behavior is unchanged. |
| S01 | Pursuit creation | Predefined pursuit starts with `pursuit_goal` and `leg_goals`. | First leg exists immediately; `leg_goal.md` equals `leg_goals[0]`; provenance is predefined; `is_leg_goal_mutatable` is false. |
| S02 | Planner submission | Predefined planner omits `leg_goal` and `next_leg_goal`. | Submission succeeds; work items are created against predefined current leg goal. |
| S03 | Planner submission | Predefined planner submits `leg_goal`. | Submission is rejected as correctable; no work items are created; attempt budget is not consumed. |
| S04 | Planner submission | Predefined planner submits `next_leg_goal`. | Submission is rejected as correctable; predefined list remains the only next-leg source. |
| S05 | Retry behavior | Predefined leg retries after an attempt failure. | Retry attempt keeps the same predefined `leg_goal`; planner still cannot refocus. |
| S06 | Leg promotion | Predefined non-final leg succeeds. | Next leg is created from the next `leg_goals` entry; provenance is predefined. |
| S07 | Pursuit success | Predefined final leg succeeds. | Pursuit closes `Success`; no extra leg is created. |
| S08 | Pursuit failure | Predefined leg exhausts retry budget before final leg. | Current leg and pursuit close `Failed`; later predefined legs are not created. |
| P01 | Context path universe | Load context tree after creation and submissions. | Paths use `pursuit_<id>/leg_<id>/attempt_<id>`; no path contains `workflow_`, `iteration_`, `/plan_`, `focus.md`, `deferred_goal.md`, or `archived/`. |
| P02 | Projection mirror | Disk mirror writes context tree. | Mirror root is `.eos-agents/pursuit/context/pursuit_<id>/`; stale `.eos-agents/workflow/context` output is not written. |
| P03 | `leg_goal.md` rendering | Render all provenance sources. | First dynamic, dynamic successor, dynamic refocus, and predefined legs render the correct provenance line; snapshots expose the matching `leg_goal_version` audit stamp. |
| P04 | `next_leg_goal.md` rendering | Compare absent, dynamic declared, dynamic reset, and predefined preview cases. | File absence/presence/content matches effective successor semantics for each mode. |
| P05 | Superseded relocation | Dynamic refocus displaces older live attempts. | Whole attempt subtree moves under `superseded/`; live location is pruned from DB projection and disk mirror. |
| P06 | Projection listing/search | Query context with and without superseded scope. | Search excludes `superseded/` by default and includes it only when explicitly scoped. |
| P07 | Work-item field rendering | Work item static files are rendered. | Uses `title.md` and `spec.md`; no `description.md` path is rendered. |
| O01 | Attempt outcome | Work items finish success/failure/cancelled in planner order. | `# Attempt outcome` renders ordered rows with worker summaries or `(no summary)`. |
| O02 | Planner-death outcome | Planner dies before work items. | Attempt `outcome.md` renders `(no work items)` and no plan context folder appears. |
| O03 | Leg outcome | Retry attempt fails before budget exhaustion. | Leg `outcome.md` is absent until success or final failure. |
| O04 | Pursuit outcome | Multi-leg dynamic and predefined pursuits close. | Root `# Pursuit outcome` renders closed leg sections in sequence order with `leg_<id>` labels. |
| O05 | Cancellation | Running pursuit is cancelled. | Non-terminal descendants are `Cancelled`; business outcome files are not created for cancelled attempts/legs; root cancellation marker may render. |
| O06 | Snapshot outcomes | Running and terminal attempt/leg/pursuit snapshots are loaded. | `outcome` is null while running and equals rendered `outcome.md` content after terminal closure. |
| O07 | Work-item launch gate | A work item has incomplete dependencies. | Scheduler does not claim it until every direct `depends_on` target is `Success`; post-commit launch guard rechecks the same policy. |
| O08 | Work-item failure propagation | One work item fails while unrelated siblings are running or launchable. | Failed item closes immediately; `propagateDependencyBlocks` marks only `NotStarted` descendants `Blocked`; unrelated running or launchable items continue. |
| O09 | Attempt failure close | Failed attempt still has unrelated running or still-possible work. | Attempt remains `Running` until no work item is `Running` or `NotStarted`; then closes `Failed` with `failure_reasons.md` listing every failed and blocked work item. |
| M01 | Script variable map | Load `.eos-agents/pursuit/scripts/variable_reference_map.cjs`. | Exposes pursuit/leg names only; no workflow/iteration/focus/deferred variables remain. |
| M02 | Planner script | Dynamic first-leg input is rendered. | Initial messages include pursuit context, current leg goal, omit-`leg_goal` guidance, and no old vocabulary. |
| M03 | Planner script | Dynamic retry with standing `next_leg_goal` is rendered. | Message says omission preserves standing successor and refocus resets it. |
| M04 | Planner script | Predefined input is rendered. | Message says caller predefined the leg goal and instructs omission of both `leg_goal` and `next_leg_goal`. |
| M05 | Worker script | Worker input with dependencies is rendered. | Initial messages include assigned work and direct `depends_on` targets from the same non-superseded leg-goal version; they prohibit planning legs or deciding `next_leg_goal`. |
| L01 | Agent launcher | Planner launch is claimed after pursuit commit. | Launch port receives current pursuit/leg/attempt/plan locator and `pursuit_context`; no launch occurs before commit. |
| L02 | Agent launcher | Worker launches after accepted plan. | Launch port receives current work item locator and direct dependency context. |
| L03 | Agent launcher | Context composition fails. | Service synthesizes a failed planner/worker settlement through existing failure path. |
| H01 | Caller handle | Non-agent caller creates pursuit and calls `settle`. | Settlement resolves to terminal pursuit result without requiring background-supervisor ownership. |
| H02 | Caller handle | Non-agent caller calls `cancel`. | Pursuit and non-terminal descendants cancel; repeated cancel is idempotent. |
| H03 | Tool adapter | Agent caller delegates pursuit. | Tool adapter registers the pursuit handle as background session type `"pursuit"` and exposes normal cancel behavior. |
| V01 | Identifier scan | Product TypeScript source and active scripts are scanned. | No active `iteration_focus`, `deferred_goal`, `workflow_context`, `workflow_<id>`, `iteration_<id>`, `archived/`, `focus.md`, `description.md`, work-item `needs`, `delegate_workflow`, `@eos/workflow`, or `.eos-agents/workflow/scripts` remains. |

## 17. Verification Commands

Focused commands:

```bash
cd /Users/yifanxu/machine_learning/LoVC/EphemeralOS/eos-agent-core
pnpm exec vitest run \
  packages/contracts/tests/pursuit.test.ts \
  packages/db/tests/schema.test.ts \
  packages/pursuit/tests/package-boundary.test.ts \
  packages/pursuit/tests/context.test.ts \
  packages/pursuit/tests/mirror.test.ts \
  packages/pursuit/tests/lifecycle.test.ts \
  packages/tool/tests/pursuit-family.test.ts \
  packages/agent-runtime/tests/agent-profile.test.ts \
  packages/agent-runtime/tests/runtime.test.ts \
  packages/agent-runtime/tests/pursuit-runtime.test.ts \
  packages/agent-runtime/tests/pursuit-active-scripts.test.ts
pnpm run typecheck
pnpm run lint
pnpm run test
```

Docs hygiene:

```bash
git diff --check -- docs/plans/agent-core-rust-to-typescript-migration eos-agent-core .eos-agents/pursuit/scripts
```

Identifier-boundary scans:

```bash
rg -n "iteration_focus|deferred_goal|workflow_context|workflow_<id>|iteration_<id>|archived/|focus\\.md|description\\.md|fail_reason\\.md" eos-agent-core .eos-agents/pursuit/scripts
rg -n "delegate_workflow|type: \"workflow\"|workflow_id|iteration_id|@eos/workflow|packages/workflow|workflowDb|workflowContextRoot|\\.eos-agents/workflow/scripts" eos-agent-core .eos-agents/pursuit/scripts
rg -n "\\bneeds\\b" eos-agent-core/packages/contracts eos-agent-core/packages/pursuit .eos-agents/pursuit/scripts
test ! -d eos-agent-core/packages/pursuit/src/projection
```

## 18. Acceptance Criteria

Phase 05.3 is accepted when:

- product-facing contracts use pursuit/leg vocabulary,
- planner payloads use `leg_goal` and `next_leg_goal`,
- planner work items use `title`, `spec`, and leg-scoped `depends_on`;
  `description` and `needs` are not active work-item fields,
- `depends_on` may target previous non-superseded attempts in the same
  leg-goal version and is rejected across refocus, superseded attempts, future
  attempts, or other legs,
- first planner submissions no longer require a focus/leg-goal declaration,
- `next_leg_goal` is accepted without a sibling `leg_goal`,
- refocus with `leg_goal` supersedes prior live attempts and resets standing
  successor scope,
- there is no payload shape for clearing a standing `next_leg_goal` without a
  refocusing `leg_goal`,
- `create_pursuit(pursuit_goal, [leg_goal...])` uses predefined leg goals and
  rejects planner refocus/successor declarations,
- `leg_goal_version` is exposed as audit metadata and increments only when a
  dynamic `leg_goal` refocus creates a new version,
- each leg exposes `is_leg_goal_mutatable`,
- pursuit, leg, and attempt snapshots expose nullable `outcome` that remains
  null until terminal closure,
- work-item failure blocks only dependents, lets unrelated running or launchable
  siblings continue, and closes the attempt only after dependency block
  propagation leaves no work item `Running` or `NotStarted`,
- work items are never claimed or launched until every direct `depends_on`
  target is `Success`; running work items are never converted to `Blocked`,
- failed attempts render list-shaped `failure_reasons.md`,
- pursuit handles expose caller-agnostic `cancel` and `settle` behavior,
- rendered context paths use `pursuit_<id>/leg_<id>/superseded/`,
- the context mirror root is `.eos-agents/pursuit/context`,
- `leg_goal.md` exists at leg creation and includes provenance,
- `focus.md`, `deferred_goal.md`, and `archived/` are gone from the live context
  universe,
- `@eos/workflow` / `packages/workflow` are replaced by `@eos/pursuit` /
  `packages/pursuit`,
- package/file names use `agent-launcher.ts`, singular `transition.ts`,
  `context-engine/projection/`, and `.eos-agents/pursuit/scripts`,
- `Plan` remains DB/launch/submission state and does not reappear as a rendered
  context entity,
- `.eos-agents/workflow/scripts` is no longer an active initial-message script
  root,
- planner and worker scripts emit the Phase 05.3 pursuit/leg initial messages,
- focused tests plus `pnpm run test` pass.
