# EOS Agent Core Rust to TypeScript Migration - Phase 05.1 Workflow Context Redesign

Status: Proposed
Date: 2026-06-11
Owner: eos-agent-core
Migration direction: Rust -> TypeScript
Project path: `/Users/yifanxu/machine_learning/LoVC/EphemeralOS/eos-agent-core`
Base spec: `phase-05-workflow-orchestration_SPEC.md` (Phase 05 is not yet
implemented; this spec amends its context model, projection layer, and
launch-context policy before implementation - where the two conflict, this
spec wins, and both land as one combined effort)
Companion spec status: `docs/plans/workflow_context_projection_SPEC.md` is
retired as the rendering contract for this surface (§4)
Depends on: Phase 04.5 (`@eos/agent-runtime` profiles, hooks, launch shape),
Phase 04 (`@eos/tool`, submission seam), Phase 03 (`@eos/engine`),
Phase 02 (`@eos/contracts`)

## 1. Intent

Phase 05 gives every retry plan its own `plan_spec`, so a closed iteration's
achievement is a collection of attempts whose intents may all differ - the
iteration has no stable identity to judge its closure against. Phase 05.1
replaces that model with a planner-declared **iteration focus**:

- the workflow splits its goal into an immutable `original_goal` and a derived
  `current_goal` (what remains to be done),
- each iteration commits to one `focus` - the slice of `current_goal` it will
  complete - optionally peeling off a `deferred_goal` (the declared remainder,
  promoted to the next iteration's goal on success),
- declarations are append-only rows on plans; the current focus, the current
  goal, and every archived predecessor are all derived views over them,
- the closure outcome of an iteration comes from its last attempt, and only
  attempts consistent with the final focus count as the iteration's
  achievement.

The projection layer is rebuilt to match: composed `spec.md`/`brief.md`
projections are replaced by a per-field file universe (one fact, one path)
with derived `archived/` sections, persisted to disk as a post-commit
mirror (§2.17), and the fixed launch-context policy is replaced by
full-variable snapshots composed either by a built-in default policy or by
a user-configured context script with the same ergonomics as the existing
`.eos-agents/hooks` command hooks.

Phase 05's orchestration spine is untouched: the scheduler cell, serial
reconcile queue, claim-in-transaction/launch-after-commit, settlement
synthesis (§2.7 there), the one-session supervisor story, the one-open-workflow
guard, `AgentLaunchPort`, revision stamping, and `(revision, path)` render
memoization all survive exactly as specified.

## 2. Design Decisions

1. **Goal model: immutable `original_goal`, derived `current_goal`.** The
   workflow stores only the caller's ask. `current_goal` is the head of the
   deferral chain: it equals `original_goal` until an iteration closes
   `Success` carrying a `deferred_goal`, at which point it advances to that
   deferral. It is computed in `loadAggregate`, never stored - storing it
   beside a reconstructible chain would create a second copy to keep in sync.
2. **Focus is iteration-scoped and planner-declared.** `plan_spec` is
   deleted. The first planner submission of an iteration must declare
   `focus`; until then the iteration has no focus and the planner's job is
   exactly to peel one off `current_goal`. Scoping moves from
   creation-time inheritance (Phase 05 `iterations.goal`) to plan-time
   declaration.
3. **Declarations are append-only and atomic.** A declaration is the pair
   `(iteration_focus, deferred_goal?)` - one peel of `current_goal` -
   recorded on the submitting plan row as `declared_focus` /
   `declared_deferred_goal`. Submitting the pair resets both; omitting it
   keeps the standing declaration. No iteration column mutates: the
   iteration's current focus and deferred goal are views over its ordered
   plan rows, which is also what makes the §9 archives derivable. A
   `deferred_goal` can never exist without the focus that produced it.
4. **Refocus supersedes in place; the budget counts all attempts.** A retry
   planner that re-declares focus supersedes the prior declaration inside
   the same iteration - it does not open a new one (a new iteration would
   refresh the attempt budget after a failure, letting a pivoting planner
   loop forever). An attempt is **consistent** iff no later plan in its
   iteration declared a focus. `max_attempts` counts every attempt in the
   iteration, refocused or not; a planner that wants a fresh budget has the
   honest path of declaring the work in `deferred_goal`.
5. **`deferred_goal` is a handoff declaration, not load-bearing state.**
   `current_goal` advances only at successful iteration close, so a refocus
   that drops the previous deferral loses nothing: the next planner re-peels
   from an unchanged `current_goal`. This is also why retry-planner context
   omits the standing `deferred_goal` by default (§2.13) - it is not part of
   the iteration's focus.
6. **Plan survives as the planning-act record.** With `plan_spec` gone a
   plan carries `status`, `planner_summary`, the declared pair, and
   `agent_run_id`. Folding it into Attempt was considered and rejected: the
   plan row keeps the launch queue uniform (`kind: 'plan' | 'work_item'`)
   and gives the planner run its binding point.
7. **Per-field projection: one field, one file.** Composed `spec.md` /
   `brief.md` are not implemented. Every entity-local field projects as one
   file named for the field; an absent field is an absent path, never a
   placeholder. Status never gets a file - it rides directory listings. A
   field-file render is the §2.19 (Phase 05) revision stamp, the owning
   entity's status line, then the field text verbatim.
8. **Archives hold what the parent's achievement story excludes.** Two
   kinds, both derived:
   - `workflow/archived/iteration_<k>/current_goal.md` is the superseded
     goal *value* iteration `k` pursued; it exists iff a successor
     iteration exists. The iteration folder itself stays live - closed
     iterations are the workflow's achievement chain, not abandoned work.
   - `iteration/archived/attempt_<a>/` is a *drifted attempt* - one
     superseded by a later focus declaration - relocated whole: its
     `fail_reason.md`, plan summary, and work items render there in their
     live shapes, plus `focus.md` / `deferred_goal.md` at the attempt root
     when attempt `a`'s plan made the now-superseded declaration.
     Non-declaring drifted attempts relocate without declaration files;
     they ran under the nearest preceding sibling's declaration.
   Nothing archives at mutation time: "archived" is purely derived (a
   later declaration exists), so archives stay automatically correct under
   idempotent transitions and cancel races, and the live attempt set under
   `iteration_<id>/` is exactly the consistent set - an iteration's folder
   always reads as the current focus's story. A refocus is the one event
   that changes an entity's path; §9 names the recovery rules.
9. **The tree listing is the overview projection.** A `read_workflow_context`
   path resolving to a directory (the root by default) returns the subtree
   listing: one row per path with the owning entity's status and, where a
   summary field exists, its first line. This replaces the Phase 05 default
   root `brief.md`.
10. **One fact, one path - `archived/` excluded from search by default.**
    With no composed projections, every fact has exactly one live path, so
    Phase 05 §2.17's dedup rule holds by construction and `field` in a
    search hit is simply the filename. The archive reintroduces controlled
    duplication (an archived `current_goal` repeats the predecessor
    iteration's `deferred_goal` declaration in a different role), so
    `query_workflow_context` skips `archived/` subtrees unless `scope`
    names a path inside one. Drifted attempts ride the exclusion:
    abandoned-direction outcomes stop surfacing to current-focus agents by
    default, while retry planners still receive them through the §2.11
    variables, which read the aggregate, not paths.
11. **Launch context = full variable snapshot + pluggable composer.** The
    runtime-side variable builders produce a versioned, typed snapshot per
    agent kind containing *all* facts - including ones the default policy
    hides (standing `deferred_goal` on retry, superseded declarations).
    Hiding is policy; the composer decides. The scheduler takes one injected
    `composeLaunchContext(agentName, input)` function and calls it after
    commit, before `port.launch`.
12. **Context scripts are hook-parity subprocesses bound by agent kind.**
    Scripts live in `.eos-agents/workflow/scripts/` and bind by filename:
    `planner.(cjs|mjs)` / `worker.(cjs|mjs)`, falling back to the built-in
    default policy. Kind is also the input shape (§7), so one script
    serves every profile of its kind; per-profile (agent-name) overrides
    stay a deferred seam (§5). The runtime spawns the bound script
    per launch with the JSON snapshot on stdin and parses
    `{ messages: [{ role: "user", content }] }` from stdout - the same
    mental model, trust level, and execution discipline as the
    `.eos-agents/hooks` command hooks (spawned, never imported). The
    script's output IS the launch's complete ordered `initialMessages` -
    replace, never merge: the runtime appends no preamble or directive,
    and the only other model-visible context is the profile's system
    prompt and tool exposure. Without a matching script, the built-in
    default policy (an in-package pure function) composes - so the
    workflow suite spawns no processes.
13. **Default composition policy.** Initial planner: `current_goal`, then a
    directive to declare focus (and optionally `deferred_goal`) and plan
    work items. Retry planner: `current_goal`, the standing focus, the
    *consistent* failed attempts (work items with summaries/outcomes and
    `fail_reason`), then a directive to re-plan within the focus or refocus
    (naming that refocus resets both fields) and to read the failed
    attempt's paths via `read_workflow_context` before planning - the
    standing `deferred_goal` and superseded attempts are deliberately
    omitted (§2.5). Worker: the iteration focus, dependency outcomes, own
    `work_item_spec`, submit directive.
14. **Compose failures ride the §2.7 uniform rule.** A script that exits
    non-zero, times out, or emits output failing the Zod parse means the
    launch never happens: the scheduler synthesizes a failed settlement for
    the claimed entity, recording `fail_reason: "context_script_error: …"`,
    and the ordinary retry path runs. `max_attempts` bounds the damage from
    a broken user script; nothing can wedge in `Running`.
15. **Conditional payload rules validate at materialization.** The
    submission tools stay service-free, so "the iteration's first
    declaration is required" cannot be checked in-run. Like Phase 05's
    unknown-`agent_name` rule, a first planner settlement whose payload
    lacks `focus` fails the attempt with a recorded `fail_reason`; the
    retry planner sees it.
16. **Closure outcomes derive from the last attempt.** An iteration's
    `outcome.md` is composed at render time from its closing attempt's plan
    summary and work-item summaries/outcomes. Prior iterations collapse to
    a status row in listings (the Phase 05 §2.20 rule), and rows under
    `archived/` subtrees render as status rows only; with drifted attempts
    relocated (§2.8) the live iteration subtree needs no further collapse -
    it contains only consistent attempts. The workflow terminal summary
    mechanism is unchanged.
17. **The context tree persists to disk as a post-commit mirror.** Each
    reconcile job, after commit and before launches, re-renders the §9
    universe from the fresh aggregate and mirrors it under
    `<workflowContextRoot>/workflow_<id>/` (default
    `.eos-agents/workflow/context/`): temp-file + atomic rename per file,
    and paths that left the universe (a refocus relocation) are pruned.
    The DB stays authoritative, rendering never reads these files, and the
    tools keep rendering from the aggregate - the mirror serves humans
    tailing a workflow and the deferred sandboxed-worker seam. The serial
    reconcile queue makes the writer single-threaded per workflow and
    per-field files keep each write small, which is what retires Phase 05
    §2.2's write-amplification and stale-race objections. A write failure
    is non-fatal: logged, state untouched, healed by the next mutation's
    re-projection. `.eos-agents/workflow/` splits cleanly: `scripts/` is
    user-authored, `context/` is machine-written.

## 3. Phase 05 Amendments

Recorded deltas against `phase-05-workflow-orchestration_SPEC.md`;
everything not listed is implemented as written there.

| Phase 05 item | Amendment | Decision |
| --- | --- | --- |
| §2.2 projection is virtual only; the physical writer is a deferred seam | the disk mirror is in scope as a post-commit cache under `.eos-agents/workflow/context/`; virtual rendering stays the tool contract | §2.17 |
| §2.13 ten brief/spec renderers + two combinators; companion §8 templates | replaced by field-file renders and the tree listing; no composed projections exist | §2.7, §2.9 |
| §2.14 goals ride the launch directive, briefs stay goal-free | moot - the composer owns all placement over full variables | §2.11, §2.13 |
| §2.20 prior iterations collapse in the workflow brief | becomes listing policy; drifted attempts relocate under `archived/` instead of collapsing in place | §2.8, §2.16 |
| §2.17 search over entity-local fields only, dedup rule | one fact one path by construction; `field` = filename; `archived/` excluded by default | §2.10 |
| §6 `PlannerOutcomePayloadSchema` (`plan_spec`, top-level `deferred_goal_for_next_iteration`) | atomic optional `focus` group replaces both; `plan_spec` deleted | §7 |
| §6 schema: `workflows.goal`, `iterations.goal`, `plans.plan_spec`/`deferred_goal` | `workflows.original_goal`; iterations carry no goal/focus columns; plans gain `declared_focus`/`declared_deferred_goal` | §8 |
| §7 `context.ts` fixed launch policy | variable builders + injected composer + default policy + kind-bound `workflow_context/` scripts | §2.12, §10 |
| §7 default read at workflow root = `brief.md` | directory paths (root included) return subtree listings | §2.9 |
| §13 step 3 renderer tests bind the companion §12 criteria | replaced by the §15 projection/derivation tables | §15 |
| §14 case 3 rendering assertions | replaced by §15 case 3 | §15 |

Unchanged and re-affirmed: §2.3 status enum, §2.4 minted IDs, §2.7-2.12
scheduler/settlement/session machinery, §2.16 `AgentLaunchPort`, §2.17 read
paging + revision pinning, §2.18 bound functions, §2.19 revision stamp.

## 4. Companion Spec Status

`docs/plans/workflow_context_projection_SPEC.md` remains the historical
record of the entity model and the §9 lifecycle flows, but its rendering
contract (§1 spec/brief model, §6, §8, the §12 rendering criteria, and
invariants 6-15) is retired for the TypeScript surface: this spec's per-field
projection replaces it. The Phase 05 §3 amendment rows that adjusted that
rendering contract are subsumed by §3 here.

## 5. Scope

In scope (all as amendments to the Phase 05 packages, landed together with
Phase 05):

- `@eos/contracts`: the reshaped planner payload schema, context-script IO
  DTOs (`PlannerContextInput`, `WorkerContextInput`, `ContextScriptOutput`),
- `@eos/db`: the reshaped schema and the derived views in `loadAggregate`,
- `@eos/workflow`: per-field projection + tree listing (replacing
  `render/`), the §2.17 disk mirror, variable builders + default
  composition policy (replacing `context.ts`), the composer seam on the
  scheduler, materialization-time declaration rules, the §14
  entity-oriented module layout,
- `@eos/tool`: the narrowed `submit_planner_outcome` schema; read/query
  behavior over the new path universe,
- `@eos/agent-runtime`: the `.eos-agents/workflow/scripts/` registry
  (loaded and validated at startup), the script-runner composer adapter,
  the `workflowContextRoot` mirror dependency.

Out of scope: everything Phase 05 §11 defers except the physical projector
(now in scope, §2.17), plus context-script sandboxing beyond the hook
trust model, per-profile (agent-name) context-script overrides (one extra
registry lookup when wanted), non-workflow uses of the composer,
dirty-subtree mirror optimization (the mirror re-projects the workflow per
mutation), and any stored focus history beyond the plan rows (none is
needed).

## 6. Goal and Focus Model

```text
delegate_workflow(goal)
  Workflow: original_goal  (immutable, the caller's ask)
            current_goal   (derived head of the deferral chain)
    │
    ▼
  Iteration: focus = none until the first planner declares
    │
    ├─ Attempt 1 → planner sees (current_goal)
    │              submits (focus, deferred_goal?, work_items)   focus REQUIRED
    │
    ├─ Attempt n (retry) → planner sees (current_goal, focus,
    │              consistent prior attempts + fail_reasons)
    │              submits (work_items)                          keep focus
    │              or (focus, deferred_goal?, work_items)        refocus: resets
    │                                                            BOTH, supersedes
    │                                                            prior attempts
    └─ closes Success from the last attempt:
         deferred_goal declared → current_goal := deferred_goal,
                                  next Iteration (origin 'deferred_goal')
         none                   → Workflow Success
       closes Failed (budget exhausted) → Workflow Failed
```

Invariants:

1. `current_goal` advances only when an iteration closes `Success` carrying
   a `deferred_goal`; it never changes mid-iteration.
2. Every non-first iteration's predecessor closed `Success` with a deferral,
   so the goal chain has no gaps.
3. `(focus, deferred_goal)` declare and reset atomically; a deferral never
   exists without the focus that produced it.
4. Declarations are append-only; the current focus/deferred pair is the
   latest declaration among the iteration's plans.
5. An attempt is consistent iff no later plan in its iteration declared;
   closure outcomes and retry context consider only consistent attempts,
   and the live attempt paths are exactly the consistent attempts -
   drifted attempts resolve under `archived/` (§2.8).
6. `max_attempts` bounds the iteration's total attempts across refocuses.
7. The iteration's first materialized plan must carry a declaration
   (§2.15); the first declaration may come from a later attempt when an
   earlier planner died before submitting.

## 7. Contracts (`@eos/contracts`)

```ts
const PlannerOutcomePayloadSchema = z.object({
  summary: z.string().min(1),
  focus: z.object({                          // one atomic peel of current_goal
    iteration_focus: z.string().min(1),
    deferred_goal: z.string().min(1).optional(),
  }).optional(),                             // required for the iteration's
                                             // first declaration (§2.15);
                                             // optional (= keep) on retries
  work_items: z.array(z.object({
    id: z.string().min(1),
    agent_name: z.string().min(1),
    work_item_spec: z.string().min(1),
    needs: z.array(z.string()).default([]),
  })).min(1),
});
// WorkerOutcomePayloadSchema is unchanged from Phase 05 §6.
```

Context-script IO, versioned (snake_case serialized DTOs):

```ts
interface PlannerContextInput {
  input_version: 1;
  kind: "planner";
  revision: number;                               // the §2.19 stamp value
  workflow: { id: string; original_goal: string; current_goal: string };
  iteration: {
    id: string; sequence: number; origin: "initial" | "deferred_goal";
    focus: string | null;                          // null ⇔ no declaration yet
    deferred_goal: string | null;                  // present even on retry (§2.11)
    max_attempts: number;
  };
  attempt: { id: string; sequence: number };
  prior_attempts: Array<{                          // same iteration, ordered
    id: string; status: string; fail_reason: string | null;
    consistent: boolean;                           // §6 invariant 5
    declared_focus: string | null;                 // null = kept
    declared_deferred_goal: string | null;
    work_items: Array<{ id: string; agent_name: string; spec: string;
                        status: string; summary: string | null;
                        outcome: string | null }>;
  }>;
  prior_iterations: Array<{ focus: string; status: string; summary: string }>;
  paths: { iteration: string; last_attempt: string | null; archived: string };
}

interface WorkerContextInput {
  input_version: 1;
  kind: "worker";
  revision: number;
  workflow: { id: string; original_goal: string; current_goal: string };
  iteration: { id: string; sequence: number; focus: string;
               deferred_goal: string | null };
  work_item: { id: string; agent_name: string; spec: string };
  dependencies: Array<{ id: string; spec: string; status: string;
                        summary: string | null; outcome: string | null }>;
  paths: { attempt: string; work_item: string };
}

const ContextScriptOutputSchema = z.object({
  messages: z.array(z.object({
    role: z.literal("user"),
    content: z.string().min(1),
  })).min(1),
});
```

`ContextPage` / `ContextSearch` keep their Phase 05 shapes; a search hit's
`field` is the filename of the matched file.

## 8. Store (`@eos/db`)

```text
workflows    id PK, parent_run_id, original_goal, status, revision,
             created_at, updated_at, closed_at
iterations   id PK, workflow_id, sequence, origin ('initial'|'deferred_goal'),
             max_attempts, status, timestamps          -- no goal/focus columns
attempts     id PK, workflow_id, iteration_id, sequence, status, fail_reason,
             timestamps                                 -- unchanged
plans        id PK, workflow_id, iteration_id, attempt_id, agent_run_id,
             status, declared_focus, declared_deferred_goal,   -- null = kept
             planner_summary, timestamps                -- plan_spec deleted
work_items   unchanged from Phase 05 §6
launch_queue unchanged from Phase 05 §6
```

`loadAggregate` computes the derived views once per load and exposes them on
the frozen aggregate (renderers and variable builders never re-derive):

| View | Derivation |
| --- | --- |
| goal in effect for iteration `k` | `original_goal` for the first iteration; otherwise iteration `k-1`'s effective `deferred_goal` (§6 invariant 2) |
| `current_goal` | goal in effect for the latest iteration |
| iteration focus / deferred goal | latest plan in the iteration with non-null `declared_focus` |
| attempt consistency | no later plan in the iteration declared |
| workflow archive set | every iteration with a successor (it advanced the goal) |
| iteration archive set | every non-latest declaration, keyed by its declaring attempt |
| iteration outcome | closing attempt's plan summary + work-item summaries/outcomes |

## 9. Context Path Universe and Projection

```text
workflow_<id>/
  original_goal.md
  current_goal.md                          head of the goal chain (derived)
  outcome.md                               terminal only (derived)
  archived/
    iteration_<id>/
      current_goal.md                      the goal in effect DURING that
                                           iteration; exists iff a successor
                                           iteration exists
  iteration_<id>/
    focus.md                               latest declaration (derived)
    deferred_goal.md                       absent if none declared
    outcome.md                             terminal only (derived, §2.16)
    archived/
      attempt_<id>/                        a drifted attempt, relocated whole
        focus.md                           the superseded declaration; both
        deferred_goal.md                   files only on the attempt whose
                                           plan declared it (deferred file
                                           absent if none was carried)
        fail_reason.md                     …plus the attempt's full content,
        plan_<id>/                         identical shapes to a live attempt
          summary.md
        work_item_<id>/
          spec.md
          summary.md
          outcome.md
    attempt_<id>/                          consistent attempts only (§2.8)
      fail_reason.md                       failed attempts only
      plan_<id>/
        summary.md
      work_item_<id>/
        spec.md
        summary.md
        outcome.md
```

Rules:

- One field, one file; an absent field is an absent path (§2.7). Status
  never projects as a file.
- Every file render opens with the Phase 05 §2.19 revision stamp, then the
  owning entity's status line, then the field text verbatim.
- A path resolving to a directory (the workflow root by default) renders the
  subtree listing: per row the relative path, the owning entity's status,
  and the first line of the owning entity's summary field where one exists.
  Prior iterations and rows under `archived/` subtrees appear as their
  status row only (§2.16); their files remain readable at full fidelity.
- Archive labels are the scopes that ran under the value (§2.8): the
  workflow archive by iteration id; the iteration archive keeps every
  drifted attempt under its own id, with the declaration files riding the
  attempt that declared.
- A refocus is the one event that changes entity paths: from the next
  render the drifted attempts resolve only under `archived/`. A fresh read
  against an old live path errors naming the valid children (`archived/`
  among them), and a paging continuation across the move already fails its
  revision pin - the refocusing materialization bumped the revision.
- The same universe persists on disk: the §2.17 mirror writes it 1:1 under
  `<workflowContextRoot>/workflow_<id>/` (default
  `.eos-agents/workflow/context/`), where real directories play the
  listing's role. Tools never read the mirror; it exists for humans
  tailing a workflow and for the deferred sandboxed-worker seam.
- Renders stay memoized per `(revision, path)`; unknown paths error naming
  the valid children at the deepest resolved segment (both unchanged from
  Phase 05).

## 10. Launch Context Pipeline

The reconcile job's post-commit launch step becomes:

```text
for each claimed entity:
  input    = buildPlannerVariables(aggregate, plan)        // or buildWorker…
  messages = composeLaunchContext(agent_name, input)       // injected (§2.11)
  port.launch(agent_name, messages)                        // unchanged
  …stamp agent_run_id, track in liveRuns, settle as in Phase 05 §8
```

`buildPlannerVariables` / `buildWorkerVariables` are pure functions in
`@eos/workflow` over the frozen aggregate, producing the §7 snapshots with
every variable populated. The composer is one injected async function; the
package default is the §2.13 policy as a pure function (no subprocess, so
the workflow suite stays engine-free and spawn-free).

The runtime's composer adapter owns script resolution. At startup it loads
the context-script registry from `.eos-agents/workflow/scripts/`:

```text
.eos-agents/workflow/
  scripts/                       user-authored composers
    planner.cjs                  binds every agent_kind: planner profile
    worker.cjs                   binds every agent_kind: worker profile
  context/                       machine-written §2.17 mirror
    workflow_<id>/…              the §9 path universe on disk
```

Resolution per launch: `<agent_kind>` match, else the package default
policy. `.cjs` and `.mjs` both load - scripts are spawned, never imported,
so module flavor is the script's own business. The registry is validated
at `createAgentRuntime`: every filename must name an agent kind, so a typo
(`planer.cjs`) fails at startup, never mid-run - the Phase 04.5
static-validation discipline.

Per launch the adapter spawns the resolved script with the JSON-serialized
snapshot on stdin and parses stdout against `ContextScriptOutputSchema`,
under the same execution discipline as Phase 04.5 command hooks (bounded
timeout). A non-zero exit, timeout, or parse failure is a compose failure
handled by §2.14. The parsed `messages` are the launch's complete
`initialMessages` - replace, never merge (§2.12): a script that drops the
submit directive has removed it; the default policy always carries it.

Reference script shape (the user-side contract, mirroring the existing hook
scripts):

```js
// .eos-agents/workflow/scripts/planner.cjs — stdin: PlannerContextInput JSON
//                                   stdout: { messages: [{role:"user",content}] }
let input = "";
process.stdin.on("data", (c) => (input += c));
process.stdin.on("end", () => {
  const ctx = JSON.parse(input);
  const user = (content) => ({ role: "user", content });
  const messages = [user(`# Workflow goal\n${ctx.workflow.current_goal}`)];
  if (ctx.iteration.focus === null) {
    messages.push(user("Declare this iteration's focus …"));
  } else {
    messages.push(user(`# Iteration focus\n${ctx.iteration.focus}`));
    // ctx.prior_attempts.filter((a) => a.consistent) …
  }
  process.stdout.write(JSON.stringify({ messages }));
});
```

## 11. Lifecycle Deltas

Against Phase 05 §8; everything not named is unchanged.

- `delegate` stores `original_goal`; the first iteration is created with no
  focus (origin `'initial'`).
- Planner materialization: payload `focus` present → record the pair on the
  plan row (this supersedes any prior declaration, resets both fields, and
  relocates the now-drifted attempts' projections under `archived/` purely
  by derivation - no mutation step exists); absent → the plan keeps the
  standing declaration. A first-declaration-less
  payload, like an unknown `agent_name`, fails the attempt at
  materialization with a recorded `fail_reason` (§2.15). Work-item
  materialization and ready-launch are unchanged.
- Iteration close (`Success`, from the last attempt): derive the outcome
  (§2.16); if the effective declaration carries a `deferred_goal`, create
  the next iteration (origin `'deferred_goal'`) - `current_goal` advances by
  derivation, and the closing iteration's goal becomes archived by
  construction; otherwise close the workflow `Success`.
- Failure/retry: unchanged, except the retry planner's variables carry only
  consistent prior attempts in expanded form and the budget counts all
  attempts (§2.4).
- Every reconcile job re-projects the disk mirror after commit and before
  launches (§2.17), so a launched agent's filesystem view - once the
  sandboxed-worker seam is consumed - is never older than its own claim.
- Compose failures synthesize failed settlements with
  `fail_reason: "context_script_error: …"` (§2.14).
- Cancel cascade, reconcile serialization, terminal resolution: unchanged.

## 12. Tool Deltas (`@eos/tool`)

- `submit_planner_outcome` adopts the §7 schema; in-run structural
  validation (unique local ids, declared `needs`, no cycles) is unchanged;
  the focus-group conditional rule is materialization-side (§2.15).
  `submit_worker_outcome` is unchanged.
- `read_workflow_context`: same input, paging, revision pinning, and
  unknown-path errors; paths now resolve to field files and directories
  (subtree listings) per §9.
- `query_workflow_context`: `field` = filename; matches the path universe
  and file contents; one hit per fact at its single live path; skips
  `archived/` subtrees unless `scope` names a path inside one; explicit
  truncation unchanged.
- `delegate_workflow` and the one-open-workflow guard: unchanged
  (`goal` maps to `original_goal`).

## 13. Runtime Wiring Deltas (`@eos/agent-runtime`)

- `createAgentRuntime` loads and validates the `workflow/scripts/`
  registry (§10) beside the existing hook config loading; profiles are
  untouched.
- `AgentRuntimeDependencies` gains `workflowContextRoot?` (default
  `.eos-agents/workflow/context/`), passed to the `WorkflowService` for
  the §2.17 mirror.
- The composer adapter (name → kind → default resolution; script
  subprocess when bound, package default otherwise) is injected into the
  `WorkflowService` scheduler beside the launch-port adapter.
- Everything else in Phase 05 §10 (workflowDb, per-run `workflowTools`,
  name-universe validation, disposal cascade) is unchanged.

## 14. Workspace Changes

Delta to the Phase 05 §12 layout:

```text
packages/workflow/src/
├─ workflow/           root aggregate view + goal-chain derivation (§8);
│                      original_goal / current_goal / outcome files;
│                      terminal close
├─ iteration/          declaration views; focus / deferred_goal / outcome
│                      files; close + deferred-goal promotion (§11)
├─ attempt/            consistency; fail_reason file; retry creation
├─ plan/               declaration recording (§11); summary file; work-item
│                      materialization
├─ work_item/          spec / summary / outcome files; worker-outcome
│                      recording; readiness
├─ archive/            live/archived partition + path addressing/resolution
│                      + tree listing (pure; no archive table, mutation, or
│                      event exists)
├─ context_engine/     variable builders (§7) + default composers (§2.13) +
│                      the composeLaunchContext seam (§10)
├─ file_projection/    the §2.17 disk mirror: render-all, temp-file +
│                      atomic rename, prune of paths that left the universe
├─ scheduler.ts        cell, serial reconcile, claims, compose → project →
│                      launch
├─ service.ts          delegate / cancel / read / search (renders from the
│                      aggregate, never from disk)
├─ launch-port.ts
└─ index.ts
```

Each entity module owns its slice - view types and derivations, its field
renderers, and its local status transitions over `(trx, aggregate)`; the
scheduler's reconcile job sequences the cross-entity cascade, keeping
Phase 05 §2.15's functions-not-classes rule, distributed by owner.

`@eos/contracts` adds the §7 DTOs; `@eos/db` reshapes the migration and
`loadAggregate`; `@eos/agent-runtime` adds the `workflow/scripts/` registry
loader, the script-runner composer adapter, and `workflowContextRoot`. No new third-party dependencies. The dependency graph
is unchanged.

## 15. Migration Steps and Progress

These replace the corresponding Phase 05 §13 rows; the combined effort lands
under the Phase 05 step list with these substitutions.

| # | Step | Verify | Status |
| --- | --- | --- | --- |
| 1 | Contracts: payload focus group, context-script IO DTOs | §16 case 1 | Planned |
| 2 | `@eos/db`: reshaped schema, derived views in `loadAggregate` | §16 case 2 on `:memory:` | Planned |
| 3 | Projection: field renders, listings, archives, disk mirror | §16 cases 3 + 13 | Planned |
| 4 | Lifecycle + scheduler: declaration rules, composer seam, compose-failure synthesis | §16 cases 4-9, engine-free | Planned |
| 5 | Service read/search over the new path universe | §16 cases 10-11 | Planned |
| 6 | `@eos/tool`: submission schema swap, read/query behavior | §16 case 11 | Planned |
| 7 | Runtime: `workflow/scripts/` registry + composer adapter, end-to-end | §16 case 12 | Planned |
| 8 | Workspace wiring + index row | `pnpm run check`; `git diff --stat -- agent-core` empty | Planned |

## 16. Verification

Same harness rules as Phase 05 §14: scripted `AgentLaunchPort`, `:memory:`
databases, engine-free except case 12. Case 12 additionally spawns one real
context script fixture.

| # | Case | Asserts |
| --- | --- | --- |
| 1 | Contracts | focus group accepts/rejects documented shapes; `deferred_goal` never validates without `iteration_focus`; `ContextScriptOutputSchema` rejects empty/role-less messages |
| 2 | Store + derivations | goal chain across iterations (first = original, then each deferral); focus/deferred views track the latest declaration; consistency flags flip on a later declaration; archive sets per §8 table; budget counts attempts across refocuses |
| 3 | Projection | field render = stamp + status + verbatim text; absent field = absent path; directory paths render listings with status and summary first lines; prior iterations and `archived/` rows collapse to status rows; drifted attempts render whole under `archived/` with declaration files only on the declarer; an iteration's `outcome.md` derives from the closing attempt; live `current_goal.md` is never simultaneously archived |
| 4 | Delegation | unchanged Phase 05 case 4, plus: the launched planner's messages come from the default initial policy (goal present, focus-declaration directive present) |
| 5 | First declaration | a valid first payload records the pair and materializes items; a first payload without `focus` fails the attempt with `fail_reason`, retry launches |
| 6 | Keep vs refocus | keep: focus view unchanged, attempt consistent, paths stable; refocus: both fields reset, prior attempts relocate whole under `archived/` at the next render, a fresh read of the old live path errors naming `archived/` among valid children, the retry directive carries only consistent attempts and omits the standing `deferred_goal` |
| 7 | Success cascade | unchanged Phase 05 case 6, plus: the next planner's `current_goal` is the promoted deferral; the closing iteration's goal appears under `archived/iteration_<id>/`; no deferral → workflow `Success` with `current_goal.md` still live |
| 8 | Failure and retry | unchanged Phase 05 case 7, with the budget spanning refocuses; exhaustion mid-refocus closes iteration and workflow `Failed` |
| 9 | Death + compose synthesis | unchanged Phase 05 case 8, plus: a composer that throws/times out/returns garbage synthesizes a failed settlement with `context_script_error` recorded; no entity stays `Running` |
| 10 | Serialization + cancel | Phase 05 cases 9-10 re-run unchanged against the new model |
| 11 | Tools | read: field paths, directory listings, paging + revision pinning, unknown-path children; query: filename fields, one hit per fact, `archived/` (drifted attempts included) excluded by default and reachable via `scope`, explicit truncation; submission: §7 schema tables; guard cases unchanged |
| 12 | Runtime end-to-end | Phase 05 case 12 amended: a fixture `workflow/scripts/planner.cjs` composes the planner's complete initial messages (proven by transcript inspection - nothing merged around them); a per-name fixture overrides the kind script; a broken fixture script drives the case-9 synthesis path live; registry load fails fast on a filename matching no profile name or agent kind |
| 13 | Disk mirror | after each scripted lifecycle step the on-disk tree under the context root equals the rendered universe byte-for-byte; a refocus prunes the old live attempt folder and writes the archived one; a write failure (read-only root) leaves DB state and the run unaffected and the next mutation heals the mirror; tools render identically with the mirror deleted |

Commands (unchanged):

```bash
cd /Users/yifanxu/machine_learning/LoVC/EphemeralOS/eos-agent-core
pnpm install
pnpm run check
```

- Rust boundary hygiene: `git diff --stat -- agent-core` stays empty.
- Docs hygiene: `git diff --check -- docs/plans/agent-core-rust-to-typescript-migration eos-agent-core`.

## 17. Coexistence and Rollback

- Coexistence: Phase 05 has no landed implementation; this spec changes
  paper only until the combined effort lands. The Rust implementation
  remains live and unchanged throughout.
- Rollback: delete this spec and its index row; Phase 05 stands as written.
  After implementation, rollback follows Phase 05 §15 unchanged.

## 18. Acceptance Criteria

Phase 05.1 is accepted when, in the combined Phase 05 + 05.1 implementation:

- iterations are governed by planner-declared focus end to end: required
  first declaration (materialization-enforced), keep vs refocus with atomic
  resets, in-place supersession with consistency flags, and a budget that
  spans refocuses,
- `current_goal`, iteration focus/deferred views, and both archive sections
  are derived views over append-only declarations - no mutable goal/focus
  columns and no archive state exist anywhere in the schema,
- the context surface is the §9 per-field path universe: one fact one path,
  revision-stamped field files, directory listings as the overview, derived
  archives labeled by iteration/attempt, and no composed `spec.md`/`brief.md`
  anywhere,
- the context tree persists as the §2.17 post-commit mirror under
  `.eos-agents/workflow/context/workflow_<id>/`, byte-identical to the
  virtual renders, pruned on relocation, with non-fatal write failures and
  the DB remaining the only source of truth,
- launch context flows through full-variable snapshots and one composer
  seam: the default policy implements §2.13, `.eos-agents/workflow/scripts/`
  scripts override it by agent name then kind with hook-parity subprocess
  semantics owning the complete initial messages, and every compose
  failure synthesizes a failed settlement through the Phase 05 §2.7 path,
- read/query tools work over the new universe with paging, revision
  pinning, default `archived/` exclusion, and explicit truncation,
- Phase 05's orchestration spine passes its suite unmodified except where
  §3 amends it, under `pnpm run check`,
- the Rust `agent-core/` tree is byte-for-byte unchanged,
- and the migration `index.md` lists Phase 05.1 with status and
  verification.
