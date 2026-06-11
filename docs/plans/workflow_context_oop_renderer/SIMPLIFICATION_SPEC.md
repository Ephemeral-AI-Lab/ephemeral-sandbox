# Workflow Context OOP Renderer - Simplification SPEC

Status: Implemented (2026-06-11)
Date: 2026-06-11
Parent contract: `docs/plans/workflow_context_projection_SPEC.md` (behavioral source of truth)
Artifact under change: `docs/plans/workflow_context_oop_renderer/` (interactive demo only)

Scope:

- collapse the demo's 22-file, window-global, class-per-concept structure into one
  self-contained `index.html`, matching the sibling artifacts
  (`workflow_context_projection_renderer.html`, `workflow_context_emulator.html`)
- remove structural ceremony that exists only to mirror the parent spec's
  production object graph class-for-class
- preserve rendered projection output and UI behavior byte-for-byte

Non-goals:

- no change to the parent spec's invariants, lifecycle semantics, or its §7
  production decomposition (any revision there is a separate proposal)
- no framework, bundler, dev server, or test infrastructure
- no visual redesign; styles and markup carry over unchanged
- no directory rename (the parent spec links
  `workflow_context_oop_renderer/index.html`; the path stays valid)

## 1. Intent

The demo exists to prove the projection contract interactively: delegate a
workflow, submit planner/worker outcomes, and watch deterministic `spec.md` /
`brief.md` projections re-render from the latest aggregate. It currently also
demonstrates a second thing — a one-class-per-file OOP decomposition with a
homegrown module system — and that second thing now costs more than it teaches:

- 2,141 LOC across 22 files for a demo whose siblings are single HTML files
- a `window.WorkflowContextOop` global registry with 21 `<script>` tags that
  must stay in topological order (`index.html:464-484`)
- a `WorkflowFactory` (141 LOC) that forwards to constructors and hand-rolls a
  field-by-field deep clone that must be edited on every schema change
- three orchestrator classes plus a scheduler wired with a circular
  `bindAttemptOrchestrator` setter, none of which hold state the aggregate does
  not already hold
- duplicated helpers (`escapeHtml` defined 3x, `statusClass` 2x), three separate
  tree-walk resolvers, and dead methods

Target: one `index.html`, the same behavior, roughly 40% less JavaScript, and
zero load-order or registry machinery.

## 2. Binding behavior (must not change)

1. Every acceptance criterion in parent spec §12 that the demo currently
   satisfies, including: auto-launch of planners and ready workers (no manual
   `launch_agent`), retry attempt creation under `max_try`, iteration/workflow
   failure on exhaustion, deferred-goal next-iteration creation, and terminal
   workflow closure.
2. Projection parity: for any action sequence, every projected file's `path`
   and `content` are byte-identical to the current implementation (§6).
3. UI affordances: goal textarea + `delegate_workflow`, file tree with status
   pills, file viewer, context-sensitive action buttons for running
   plans/work items, error strip, version pill, entity-count strip, DB event
   log capped at 30 entries with `scheduler: launch_agent(...)` suffixes.
4. Store semantics: snapshot isolation (mutations run on a loaded copy; the
   store owns the committed aggregate; reload-after-commit drives rendering).
5. The `window.workflowContextApp` console hook.

## 3. Current inventory and verdicts

| Area | Files | LOC | Verdict |
| --- | --- | --- | --- |
| Entities (`Workflow`, `Iteration`, `Attempt`, `Plan`, `WorkItem`, `WorkflowEntityBase`, `RunStatus`, `Markdown`) | 8 | 376 | Keep as thin classes with `renderSpec`/`renderBrief` (mirrors parent §6); drop base-class abstract throws and dead helpers |
| `WorkflowFactory` | 1 | 141 | Delete; creation moves into lifecycle functions, cloning becomes a structural revive (§4.2) |
| `WorkflowOrchestrator`, `IterationOrchestrator`, `AttemptOrchestrator`, `AttemptAgentLaunchScheduler` | 4 | 400 | Collapse into one lifecycle section of plain functions (§4.3) |
| `InMemoryWorkflowStore` | 1 | 37 | Keep; loses its factory dependency |
| `WorkflowProjector` | 1 | 46 | Keep as a single `projectFiles(workflow)` function |
| `SampleData` | 1 | 54 | Keep as-is |
| UI (`WorkflowApp`, `LifecycleActionsView`, `FileTreeView`, `FileViewer`) | 4 | 548 | Keep tree/actions as sections; fold `FileViewer` (15 LOC) into the app; deduplicate helpers; simplify selection policy (§4.4) |
| `main.js` wiring | 1 | 53 | Shrinks to a few lines once the object graph is gone |
| `index.html` (styles, markup, 21 script tags) | 1 | 486 | Styles/markup unchanged; script tags replaced by one inline `<script>` |

Dead code (verified — defined, never called):

- `AttemptOrchestrator.launchNextWorker` (`AttemptOrchestrator.js:68`)
- `Workflow.activeIteration` (`Workflow.js:11`)
- `Iteration.activeAttempt` (`Iteration.js:21`)

## 4. Simplifications

### 4.1 Module system: 22 files → one `index.html`

Every file wraps an IIFE that reads from and writes to
`window.WorkflowContextOop`, and `index.html` must list scripts in dependency
order. ES modules would fix the registry but break double-click-to-open
(`file://` module loads are CORS-blocked), and the sibling demos already set
the convention: one self-contained HTML file. Inline all JavaScript into a
single `<script>` block organized by section comments; delete `src/` after
parity passes. The page `<title>`, `<h1>`, and subhead (which currently
advertise "One JS module per class") are reworded to describe the demo, not
the file layout.

### 4.2 Factory → creation in lifecycle + structural revive

`createWorkflow`/`createIteration`/`createAttempt`/`createPlan`/`createWorkItem`
add only folder-path derivation and move into the lifecycle functions that call
them. The 68-LOC `cloneWorkflow`/`clonePlan`/`cloneWorkItem` chain — which
enumerates every field and silently re-defaults missing ones (`maxTry || 3`) —
is replaced by snapshot + revive in the store:

```js
const snapshot = JSON.parse(JSON.stringify(workflow));
function reviveWorkflow(plain) {
  const workflow = new Workflow(plain);
  workflow.iterations = plain.iterations.map(it => {
    const iteration = new Iteration(it);
    iteration.attempts = it.attempts.map(at => {
      const attempt = new Attempt(at);
      attempt.plan = at.plan ? new Plan(at.plan) : undefined;
      attempt.workItems = at.workItems.map(wi => new WorkItem(wi));
      return attempt;
    });
    return iteration;
  });
  return workflow;
}
```

Revive walks the structure, not the field lists: adding an entity field no
longer requires touching clone code. Entity constructors already accept plain
objects, so this is the entire mechanism.

### 4.3 Three orchestrators + scheduler → one lifecycle section

The orchestrator classes hold no state (only references to each other) and the
scheduler's queue is drained synchronously inside every mutation:
`scheduleReadyAgents` re-scans the whole aggregate (`enqueuePlan` +
`enqueueReadyWorkItems` per attempt) before flushing, so the mid-mutation
enqueues, the dedup check, the `bindAttemptOrchestrator` cycle, and the
folder-path task locator are all redundant in a synchronous demo. Replace the
four classes with plain functions over the aggregate:

```text
delegateWorkflow(goal)                      // workflow + first iteration/attempt/plan
launchIteration(workflow, goal, opts)
launchAttempt(workflow, iteration, opts)    // max_try guard, attempt + NotStarted plan
materializeWorkItems(scope, plannerOutcome) // validation + work item creation
submitWorkerOutcome(scope, id, outcome, ok) // work item + attempt status recompute
reconcileAttemptResult(scope)               // retry / iteration close / deferred
                                            // iteration / workflow close
scheduleReadyAgents(workflow) -> string[]   // scan: NotStarted plan -> Running
                                            // (planner), ready NotStarted work item
                                            // in Running attempt -> Running (worker)
```

Transition rules, guard errors, and error messages move over verbatim; the
dispatch order (mutate → reconcile → schedule → commit → reload → render) is
unchanged. **Fidelity tradeoff, accepted:** the demo stops modeling the
durable launch queue and its workflow-unique attempt locator (parent spec §7.2,
invariant 20). Those remain production requirements about the outbox; the demo
only ever observed the logical outcome (records flip to `Running`,
`launch_agent(...)` strings appear in the event log), and that observable
behavior is preserved.

### 4.4 Selection policy from state, not threaded result kinds

`reconcileAttemptResult` currently returns nested result DTOs
(`{kind: "workflow_deferred", iterationResult: {kind: "retry_created", ...}}`)
that exist only so `WorkflowApp.nextSelectionAfterWorkerResult` can pick the
next selected file. Replace with one `defaultSelection(workflow)` that derives
the focus from the fresh aggregate (first `Running` plan brief, else first
ready/`Running` work item brief, else workflow spec when terminal), preserving
the current observable focus behavior for: worker success with remaining work,
retry creation, deferred next iteration, and terminal workflow.

Implementation note: parity replay showed the old policy already broke the
"worker success with remaining work" case inside retry attempts —
`findWorkItem` resolved the submitted ID against the whole workflow, work-item
IDs repeat across attempts, so it hit the stale copy in the terminal first
attempt and fell back to the submitted item's `spec.md`. The state-derived
policy follows the next running worker there too (same as first-attempt
behavior); this three-step focus divergence is accepted, and projection/event
parity is unaffected.

### 4.5 Entity cleanup

Entities stay as five thin classes owning `renderSpec`/`renderBrief` (the shape
the parent spec's §6 render contract prescribes). `WorkflowEntityBase` remains
only as the shared constructor plus `specPath`/`briefPath`/`statusLine`/
`isTerminal`/`appendTerminalReference`; the abstract `renderSpec`/`renderBrief`
throws are deleted. The three dead methods listed in §3 are deleted. All render
output strings are untouched.

### 4.6 Helper deduplication

One `escapeHtml`/`escapeAttr` pair (currently three copies), one `statusClass`
(currently two), and one tree-walk scope resolver: `resolveSelectedContext`,
`findAttemptScope`, and `findWorkItem` become a single
`resolveScope(workflow, predicate)` walker. `Markdown` becomes three plain
functions (`joinMd`, `shiftHeadings`, `pendingOr`).

## 5. Target layout

```text
index.html  (~1,450 lines total; JS ~1,000, down from 1,655 across 21 files)
├─ <style>            unchanged (~400)
├─ <body> markup      unchanged except title/subhead wording (~60)
└─ <script>
   ├─ // -- helpers --       RunStatus, statusClass, isTerminal, md + escape fns
   ├─ // -- entities --      Workflow, Iteration, Attempt, Plan, WorkItem (+ base)
   ├─ // -- lifecycle --     creation, transitions, reconcile, scheduleReadyAgents,
   │                         work-item validation
   ├─ // -- store --         InMemoryWorkflowStore + reviveWorkflow
   ├─ // -- projection --    projectFiles(workflow)
   ├─ // -- sample data --   defaultGoal, samplePlanSpec, sampleWorkItems, ...
   ├─ // -- ui --            renderFileTree, renderActions, renderHeader/events
   └─ // -- app --           state {selectedPath}, action handlers, reloadAndRender,
                             window.workflowContextApp hook
```

| Current | Target home |
| --- | --- |
| `RunStatus.js`, `Markdown.js` | helpers section |
| `WorkflowEntityBase.js` + 5 entity files | entities section |
| `WorkflowFactory.js` | dissolved: creation → lifecycle, clone → store revive |
| 3 orchestrators + scheduler | lifecycle section (plain functions) |
| `InMemoryWorkflowStore.js` | store section |
| `WorkflowProjector.js` | projection section (`projectFiles`) |
| `SampleData.js` | sample data section |
| `FileTreeView.js`, `LifecycleActionsView.js` | ui section |
| `FileViewer.js` | folded into app (`renderFileView`) |
| `WorkflowApp.js`, `main.js` | app section |

## 6. Parity verification

The refactor is verified by projection-output equality, captured through the
existing console hook before any code changes:

1. Baseline capture (current implementation): in the browser console, after
   each step of the canonical script below, record
   `JSON.stringify(workflowContextApp.projector.project(workflowContextApp.store.loadFreshWorkflow()))`
   and the event log strings.
2. Canonical script (drives every lifecycle branch):
   - `delegate_workflow` with the default goal
   - select the plan brief → `submit_planner_outcome(deferred)`
   - submit worker success for `work_item_schema`, then `work_item_renderers`,
     then `work_item_projector`, then `work_item_verification`
     (covers dependency unblocking and deferred next-iteration creation)
   - in the new iteration: `submit_planner_outcome(final)`, then one worker
     failure (covers retry attempt creation), then planner + all-success on the
     retry (covers iteration/workflow success)
   - separately: a run with repeated failures to `max_try` exhaustion
     (covers iteration/workflow failure)
3. After the rewrite, replay the same script: every captured projection dump
   must be byte-identical; event log strings identical except that scheduler
   launch ordering within a single flush may not differ (it must not — scan
   order is document order, same as today's enqueue order).

Executed 2026-06-11 via a headless Node replay (fake `window`/`document`,
both implementations driven through the `workflowContextApp` hook): 50 steps
covering the canonical script, error probes, and re-delegation. All projection
dumps, event-log strings, store versions, and rendered UI surfaces (status
strip, file tree, actions, DB log, file view) were byte-identical, except the
three retry-attempt focus steps documented in §4.4.

## 7. Migration steps

1. Capture the §6 baseline dumps from the current implementation.
2. Build the new inline `<script>` inside `index.html`, porting sections in
   the §5 order; keep `src/` untouched while porting.
3. Replay the §6 script against the new file; fix divergences until dumps are
   byte-identical.
4. Delete `src/`, the 21 script tags, and update the page title/subhead
   wording.
5. Confirm the parent spec's companion link
   (`docs/plans/workflow_context_oop_renderer/index.html`) still resolves.

## 8. Acceptance criteria

- One file: `index.html` opens directly via `file://` with no server, no
  `src/` directory, no `window.WorkflowContextOop` registry.
- §6 parity: byte-identical projection dumps for the canonical script,
  identical event-log strings.
- All §2 behaviors demonstrably intact (manual pass over the UI affordance
  list).
- No dead methods, single definitions for `escapeHtml`/`statusClass`, one
  tree-walk resolver, no manual field-list clone.
- JavaScript total is materially smaller (target ≈1,000 lines, from 1,655);
  no section reimplements behavior that another section already owns.
- Parent spec file is unmodified by this change.
