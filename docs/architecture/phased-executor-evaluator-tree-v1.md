# Phased Executor-Evaluator Tree v1

Status: in progress (TaskCenter US-009 era). Companion to `overlay-sandbox-plan.md`
(OCC) and `background-tasks-and-subagents.md`.

This doc describes the EphemeralOS approach to long-horizon, high-concurrency
multi-agent execution and contrasts it with three contemporary alternatives:
**Claude Code teams**, **Qoder expert teams**, and **oh-my-claudecode** (OMC).
The goal is to make the design choices defensible, not to declare a winner.

---

## Architecture at a glance

```
═══════════════════════ TASK GRAPH (per session) ═══════════════════════

                          ┌─────────┐
                          │  root   │  status: HANDOFF
                          │executor │  acceptance_criteria
                          └────┬────┘
              full_handoff     │
        ┌──────┬───────┬───────┴───────┬─────────┐
        ▼      ▼       ▼               ▼         ▼
   ┌────────┐┌──────┐┌──────┐      ┌──────┐ ┌──────────┐
   │ exec A ││exec B││exec C│      │exec D│ │evaluator │
   │ READY  ││PEND. ││PEND. │      │PEND. │ │closes_for│
   │needs:∅ ││needs:││needs:│      │needs:│ │  = root  │
   │        ││ {A}  ││ {A}  │      │{B,C} │ │needs:    │
   └───┬────┘└──┬───┘└──┬───┘      └──┬───┘ │ sinks(D) │
       │        │       │             │     └────┬─────┘
       └────────┴───────┴─────────────┘          │ fires when
                  DAG (typed needs)              │ sinks DONE
                                                 ▼
                              ──── summary propagates up closes_for ───
                                  evaluator → root → DONE


═════════════════ WORKSPACE LAYER (OCC per tool call) ═════════════════

   exec A tool call    exec B tool call    exec C tool call   exec D tool call
   ┌─ overlay ──┐     ┌─ overlay ──┐     ┌─ overlay ──┐    ┌─ overlay ──┐
   │ buffered   │     │ buffered   │     │ buffered   │    │ buffered   │
   │ writes     │     │ writes     │     │ writes     │    │ writes     │
   └─────┬──────┘     └─────┬──────┘     └─────┬──────┘    └─────┬──────┘
         │ commit when      │ commit           │ commit          │
         │ this tool call   │                  │                 │
         ▼ ends             ▼                  ▼                 ▼
   ┌─────────────────────────────────────────────────────────────────┐
   │           CANONICAL SANDBOX (one shared filesystem)              │
   │  agent's next tool call sees all sibling commits since last read │
   │  conflict = two tool calls wrote same region in the same instant │
   └─────────────────────────────────────────────────────────────────┘


══════════════════════════════ STATUS FSM ══════════════════════════════

  PENDING ──▶ READY ──▶ RUNNING ──┬──▶ HANDOFF ──(propagation)──▶ DONE
     │           │         │      ├──▶ DONE       (terminal tool)
     └───────────┴─────────┴──────┴──▶ FAILED
              (invariant 14: bypasses transition())
```

Two channels: the **task graph** carries planning + verification (typed
`needs`, sink-bound evaluator, summary propagation). The **workspace
layer** carries collaboration (one shared sandbox; OCC opens a
short-lived overlay around every tool call so concurrent tool calls
don't tear each other's writes). They are orthogonal: the DAG knows
nothing about the workspace, and OCC knows nothing about tasks. The
DAG provides parallelism with verification gates; OCC provides
per-tool-call atomicity so siblings can edit the same codebase
without coordinating manually.

## Comparison at a glance

| Dimension | EphemeralOS v1 | Claude Code teams | Qoder expert team | oh-my-claudecode |
|---|---|---|---|---|
| Concurrency unit | Sibling executors in DAG phase | `Agent()`-spawned subagents | Role-specialized agents | tmux panes / worker pool |
| Dependency model | Typed `needs` (validated DAG) | None — strings returned to parent | Implicit via roles | Task-list ordering |
| Verification gate | Sink-bound evaluator (single-shot) | Parent's prose check | Reviewer role | `/ralph` loop / operator |
| Workspace sharing | One shared sandbox; OCC overlay per tool call | Worktree per agent (session-isolated) | Shared FS (uncontrolled) | Shared repo, operator-disciplined |
| Cross-sibling visibility | Next tool call sees all prior sibling commits | None (worktree-isolated) | Yes, but racy | Yes, but racy |
| Iteration primitive | `submit_continue_work_handoff` (typed) | Re-spawn from parent | Reviewer round-trip | Self-loop until verifier passes |
| Failure mode | Typed `FAILED`, propagates to root | Parent gets error string | Reviewer rejects | Loop guard / cancel |
| Audit trail | Closure chain + summaries (reconstructible) | Parent's transcript | Operator-driven | Task list + git history |
| Best fit | Long-horizon unattended | Bounded fan-out, short tasks | Role-driven quality | Operator-in-the-loop work |

See §3 for the per-axis prose argument and §3.5 for the weighted score.

---

## 1. The three load-bearing pieces

The v1 architecture rests on three independent primitives that compose:

### 1.1 Phased executor-evaluator DAG (the "tree")

Implemented in `backend/src/task_center/`.

- Every node is a `Task` with role `executor | evaluator`, a status
  `PENDING / READY / RUNNING / HANDOFF / DONE / FAILED`, an optional
  `closes_for` pointer, a `needs: frozenset[TaskId]` direct-dep set, and a
  `subtree_kind ∈ {handoff, continuation}`.
- An executor closes one of three ways:
  1. `submit_task_completion(summary)` — leaf closure.
  2. `submit_full_handoff(tasks, task_specs, acceptance_criteria)` — fan
     out a child DAG plus a sink-bound evaluator; parent goes `HANDOFF`.
  3. `submit_partial_handoff(...)` — same, plus a `handoff_note`
     describing what the parent already did.
- An evaluator runs **only after every sink of the child DAG is `DONE`**
  (`needs = sinks(deps)`), then either:
  - `submit_task_completion` — the parent's acceptance criteria is met;
    summary propagates up the `closes_for` chain.
  - `submit_continue_work_handoff(summary)` — spawn a continuation executor
    under the evaluator (a new fan-in point on the same parent).
- The DAG is per-task (not per-session): each handoff produces its own
  isolated subgraph. The session graph is the union, rooted at one task.
- The plan is a **flat list of `{id, deps}` entries** validated by
  `dag.compile_dag` — direct deps only, no cycles, no self-deps,
  duplicates rejected. Producing deeper DAGs is delegated to recursive
  handoffs rather than asking the model to emit a transitive plan.

This is the "tree" in the name: it is a tree of DAGs, not a tree of tasks.
The DAG layer gives parallelism inside a phase; the tree layer gives
phase-by-phase verification via evaluators.

### 1.2 OCC as per-tool-call atomicity over a shared workspace

Implemented in `backend/src/code_intelligence/routing/` (overlay_run,
mutation_service, overlay_command_committer, overlay_auditor) and the
daytona overlay sandbox.

The thesis: **OCC is per-tool-call concurrency control over a single
shared workspace.** Each tool call is a short-lived transaction; the
overlay is its working set; the commit is its write phase. The point
is not to isolate executors from each other — there is no per-executor
isolation — but to make every tool invocation atomic against
concurrent invocations from sibling agents.

| Model | Conflict unit | Sibling visibility | Failure mode |
|---|---|---|---|
| Worktree isolation (Claude-team-style) | per-session merge | per-session — agents fly blind until merge | git merge conflicts, out-of-band |
| Raw shared FS | per-syscall | per-syscall — instantly visible | races, torn writes, silent corruption |
| **v1 OCC overlays** | **per tool call** | **as fast as the other agent's next tool call commits** | typed FAILED at commit; two tool calls wrote the same region in the same instant |

How it works in v1:

- All executors share **one canonical sandbox** — there is no
  per-executor isolation layer.
- Every tool call (`Edit`, `Write`, `Bash`, etc.) opens an **overlay**
  for its own execution. Reads fall through overlay → sandbox. Writes
  are buffered.
- When the tool call finishes, the overlay command committer flushes
  the overlay back to the sandbox via the arbiter
  (`code_intelligence/editing/arbiter.py`).
- If two tool calls running at the same instant wrote overlapping
  regions, the second commit is rejected as a typed conflict — the
  failing tool call surfaces it; the agent retries or reports.
- Outside the tool call, the agent has no overlay. Between tool calls
  it sees the canonical sandbox state, which already includes every
  sibling's committed edits.

So the visibility loop is: agent A's tool call commits → sandbox
updated → agent B's next read (inside its next tool call's overlay)
sees A's edits. The granularity of "when can B observe A's work" is
**A's tool-call duration**, not A's task duration. There is no
per-task fence.

This is what makes shared-workspace collaboration safe: every write
goes through a transaction, conflicts are typed at the tool-call
layer, and the rest of the architecture (DAG, evaluator, summary
propagation) does not have to reason about workspace state at all.

### 1.3 Blackboard via summary propagation

Implemented in `backend/src/task_center/propagation.py` and the
`closes_for` invariant on `Task`.

The summary blackboard is the **verification channel**, distinct from
the workspace which is the **collaboration channel** (§1.2). Siblings
collaborate by writing to the shared workspace; evaluators verify by
reading typed summaries.

- The "blackboard" is the `summary` field on each task, plus the
  `acceptance_criteria` and `handoff_note` fields wired through the
  evaluator.
- The summary's *shape* is not a schema. Agents emit prose conditioned
  by their role prompt, which keeps summaries consistently shaped without
  crystallizing a typed contract. This trades programmatic accessors for
  migration freedom.
- When a leaf calls `submit_task_completion(summary)`, the summary
  propagates up the `closes_for` chain — leaf → evaluator → parent —
  all transitioning to `DONE`. This is invariant 14: waiting-state →
  `DONE` only happens via propagation, never via `transition()`.
- Downstream siblings read upstream summaries via the prompt builder
  (`task_center/context/task_prompt.py`) **and** see upstream code
  changes via the OCC-shared workspace. Summaries are for the
  evaluator's verification budget; the workspace is for live
  collaboration.

This is a *structured* blackboard for verification — write-once-per-task,
propagation-routed — combined with a *free-form* shared workspace for
collaboration. It trades schema rigor for prompt-level evolution and a
clean audit trail.

---

## 2. Why this combination, for long-horizon high-concurrency work

Long-horizon agent runs fail in three characteristic ways:

1. **Context rot.** The single agent's context window fills with
   exploration noise; later decisions get worse.
2. **Coordination drift.** Sibling agents make incompatible
   assumptions because they were briefed at slightly different
   moments and never reconciled.
3. **Workspace corruption.** Two agents edit the same file, last write
   wins, silent regression.

The phased executor-evaluator tree addresses (1) and (2): each
executor sees a fresh prompt scoped to *its* spec plus the typed
summaries of its `needs`; the evaluator is the explicit reconciliation
point that runs *after* every sink, so siblings are checked against
the parent's `acceptance_criteria` before any further work is allowed.

OCC addresses (3) *and* contributes to (2): siblings share one
sandbox without corrupting it, because every tool call is wrapped in
its own overlay — concurrent tool calls cannot tear each other's
writes. Crucially, siblings *do* see each other's work: as soon as
agent A's tool call commits, agent B's next tool call reads the new
state. They are not flying blind the way worktree-isolated agents
would be. A conflict (two tool calls writing the same region in the
same instant) surfaces as a typed failure on the losing tool call,
not as silent corruption.

The summary-propagation blackboard lets evaluators reason without
re-reading every child's full transcript: it caps the cost of
verification at O(direct children) instead of O(tree).

---

## 3. The comparison

The three reference systems differ in concurrency primitive,
coordination point, and workspace discipline. The table summarizes;
the prose below justifies the cells.

| Axis | EphemeralOS v1 | Claude Code teams | Qoder expert team | oh-my-claudecode |
|---|---|---|---|---|
| Primary unit of parallelism | Sibling tasks in a DAG phase | Spawned subagents | Expert roles | tmux panes / worker pool |
| Decomposition shape | Flat DAG per phase, recursive | Coordinator → leaf subagents | Role specialization | Flat task list per run |
| Synchronization point | Sink-bound evaluator | Coordinator awaits subagent return | Coordinator polling/merge | Quality gate / loop check |
| Workspace sharing model | One shared sandbox; OCC overlay per tool call | Worktree isolation (per session) | Shared workspace (per docs) | Shared repo + tmux discipline |
| Long-horizon mechanism | Continuation under evaluator | Re-spawn from coordinator | Round-trip with reviewer | `/ralph` self-referential loop |
| State sharing | Typed summary blackboard | Subagent returns final string | Implicit via shared FS | Shared task list + filesystem |
| Failure containment | Per-task `FAILED`, root fails | Subagent error returned | Reviewer can reject | Loop guard / cancel |

### 3.1 Claude Code teams (spawned-subagent / Agent tool model)

What it gives you:
- Explicit hierarchy: a parent agent calls `Agent(...)` and gets a
  string back. Worktree isolation is available with `isolation:
  "worktree"`.
- Strong context hygiene — each subagent has a fresh context.

What it does *not* give you that v1 needs:
- **No DAG between siblings.** Subagents are independent leaves; the
  parent serializes results. To express "B depends on A's output" you
  must finish A in the parent's turn before launching B, which
  serializes work that is logically parallelizable.
- **No persistent evaluator role.** The "did the children meet
  acceptance" check is whatever the parent agent decides to do in
  prose; it is not a typed gate with a typed continuation primitive.
- **No OCC.** Worktrees give isolation but merging back is
  out-of-band; conflicts surface as git problems, not as a structured
  signal a parent can react to.
- **Shallow long-horizon support.** The continuation under an
  evaluator (with the prior summary attached) is the v1 answer to
  "the result was close but not quite there." The Claude teams answer
  is "the parent re-prompts" — same idea, but the state lives in the
  parent's context (which rots) instead of in a typed task node.

When Claude teams wins: bounded fan-out, short tasks, where the
parent's prose summary is enough and you don't need a phase-level
quality gate. The v1 tree is overkill for "search the repo three
ways."

### 3.2 Qoder expert team

Qoder's "expert team" concept (per the public docs as I understand
them — flag this if my read is dated) leans on **role
specialization**: PM, architect, dev, reviewer agents that round-trip
on a shared workspace with a coordinator.

Mapping to v1:
- "Reviewer" ≈ v1 evaluator. v1's evaluator differs in two ways: it is
  *bound to the closing of a specific subtree* (not a free-floating
  reviewer), and its failure path is a typed `submit_continue_work_handoff`
  primitive rather than a prose handoff.
- "Architect" / "PM" ≈ v1 root executor. v1 doesn't fix the role; the
  decomposition is done by whichever executor calls `submit_*_handoff`,
  and the role is just `executor`. v1 trades role specialization for
  uniform reentrance — every executor can recursively decompose.
- Workspace: Qoder operates on a shared FS. v1's OCC layer is the
  thing that lets *concurrent* siblings run; without OCC, expert
  teams typically serialize or single-thread the dev role.

When Qoder-style wins: tasks where role expertise is the dominant
axis of quality (e.g., "make the architect think hard before code is
written"). v1 sacrifices that for parallelism inside a phase.

### 3.3 oh-my-claudecode (`/team`, `/ralph`, `/ultrawork`, dmux)

OMC's strongest concurrency primitive is the **tmux-pane worker pool**
(dmux-workflows): N panes share a task list and a repo, with manual
or scripted coordination. `/ralph` is a self-referential loop until
verification passes; `/ultrawork` is a parallel batch executor.

Mapping to v1:
- `/ralph` ≈ v1 evaluator + continuation, but loop-based and per-task
  rather than tree-structured. It is great for converging a single
  task; it does not give you DAG parallelism inside a phase.
- `/ultrawork` ≈ v1 sibling fan-out, but without typed `needs`
  between siblings; ordering is by task list position, not by data
  dependency. v1's `compile_dag` rejects cycles and self-deps and
  forces direct-only deps — `/ultrawork` punts that to the operator.
- Workspace discipline in OMC is operator-enforced (one pane edits
  one slice of the repo at a time, by convention). v1 enforces it
  programmatically with OCC overlays.
- Blackboard: OMC uses the shared task list + filesystem; v1 uses
  typed `summary` fields with propagation invariants.

When OMC wins: interactive, operator-in-the-loop runs where the human
is the final coordinator. v1 is built for the case where the human is
*not* watching every step and the system needs to fail loud, fail
typed, and fail at a gate.

---

## 3.5 Scored comparison

Scores are 1–5 (1 = absent / operator-supplied, 3 = adequate, 5 = native
and enforced by the system). They are the author's calibrated read for
**unattended long-horizon multi-agent runs**, which is v1's target
workload — a different workload (e.g., interactive single-agent edits)
would re-rank these.

| Capability | Weight | EphemeralOS v1 | Claude Code teams | Qoder expert team | oh-my-claudecode |
|---|---:|---:|---:|---:|---:|
| Sibling parallelism inside a phase | 5 | 5 | 3 | 2 | 4 |
| Typed dependency between siblings | 4 | 5 | 1 | 2 | 2 |
| Phase-level verification gate | 5 | 5 | 2 | 4 | 3 |
| Long-horizon continuation primitive | 5 | 5 | 2 | 3 | 4 |
| Workspace conflict isolation (OCC / worktree) | 5 | 5 | 4 | 2 | 2 |
| Typed structured blackboard | 3 | 5 | 2 | 2 | 2 |
| Failure containment / typed FAILED | 4 | 5 | 3 | 3 | 2 |
| Replan-on-partial-failure semantics | 3 | 4 | 2 | 3 | 3 |
| Operator ergonomics (dev-time) | 2 | 2 | 4 | 3 | 5 |
| Time-to-first-result on small tasks | 2 | 2 | 5 | 4 | 4 |
| Auditability (post-hoc, what happened) | 4 | 5 | 3 | 3 | 3 |
| Context-rot resistance | 5 | 5 | 4 | 3 | 3 |
| **Weighted total (Σ weight × score)** | **47** | **221** | **129** | **130** | **141** |
| **Normalized (÷ 47, max 5.0)** |  | **4.70** | **2.74** | **2.77** | **3.00** |

How to read this:

- v1 dominates on the *long-horizon, unattended* axes: typed deps,
  phase gates, OCC, structured blackboard, auditability. That is the
  workload it was designed for, so a high score here is unsurprising
  rather than vindicating.
- v1 *loses* on the operator-ergonomics and time-to-first-result
  rows. Standing up a typed plan + acceptance criteria + evaluator
  has fixed overhead; for a five-minute fix it is the wrong tool.
- **Claude Code teams** scores well on isolation (worktrees) and
  small-task latency but is structurally thin on phase gates and
  typed deps — it is a parent-child *spawn* primitive, not a
  coordination primitive.
- **Qoder expert team** scores adequately on phase gates (the
  "reviewer" role) but loses on OCC and parallelism — the role-based
  decomposition tends to serialize the dev role.
- **oh-my-claudecode** scores well on operator ergonomics and
  long-horizon (`/ralph`) but the discipline is operator-supplied,
  not enforced by the system — fine when a human is in the loop,
  fragile when not.

The weights are deliberately tilted toward v1's target workload. If
you re-weight for "single-developer pair-coding" (operator ergonomics
× 5, time-to-first-result × 5, typed deps × 1) the ranking flips and
OMC wins. The scoring is therefore a statement about *fitness for a
specific workload class*, not a global ranking.

### Per-axis sanity check

A few cells worth defending explicitly because they're the load-bearing
ones:

- **v1 = 5 on typed deps**: enforced by `dag.compile_dag` — direct
  deps only, cycles rejected, duplicates rejected, self-deps rejected.
  Not aspirational; it is in the validator.
- **Claude teams = 1 on typed deps**: the Agent tool returns a string;
  there is no first-class "B needs A's output" edge. You serialize in
  the parent's turn or you don't.
- **v1 = 5 on phase-level gate**: the evaluator's `needs = sinks(deps)`
  *forces* every sink to `DONE` before the gate fires. It is not "the
  parent decides to call a reviewer" — it is graph topology.
- **OMC = 4 on long-horizon continuation**: `/ralph` is a real
  primitive, not just a prose loop. Knocked from 5 because the
  termination criterion is verifier-defined by prompt, not
  structurally tied to acceptance criteria the way v1's evaluator is.
- **v1 = 2 on operator ergonomics**: writing `acceptance_criteria` +
  a flat task plan up front is friction. v2 should consider letting
  the root executor synthesize trivial criteria for one-shot tasks.

---

## 4. What v1 is intentionally *not* doing

To keep the design defensible:

- **No phases as first-class objects.** The previous design (PhaseEntry)
  was dropped in favor of `TaskDependencyEntry` because deeper DAGs
  emerge naturally from chained `deps`, and the planner only has to
  emit direct deps.
- **No max depth cap.** Cycle detection is mandatory; depth is not.
  Recursive handoffs are how depth happens, and they are bounded by
  the evaluator's willingness to accept.
- **No coordinator-spawned evaluators.** Evaluators are
  **sink-driven**: when every sink of a subgraph is `DONE`, the
  evaluator becomes ready. The executor that submitted the handoff
  does not orchestrate evaluator timing; the graph does.
- **No streaming / re-runnable evaluators.** The evaluator runs once,
  when its sink-deps are all `DONE`. Re-running it as sinks complete
  would overload it from "verdict" into "progress monitor" — and the
  legitimate iteration case is already covered by
  `submit_continue_work_handoff`, which spawns a fresh executor with full
  planning power.
- **No mutation of the closed DAG.** Once a sibling closes and its
  summary propagates, it is fact. The evaluator cannot revise prior
  child tasks; it can only accept or spawn a continuation. Continuation
  is the *only* iteration primitive, and it always extends — never
  rewrites.
- **No typed artifact schema.** The shape of `summary` is guided by
  per-role prompts, not by a Pydantic-style contract. This trades
  programmatic accessors for prompt-level evolution; new task types
  do not require schema migrations.
- **No cross-task summary references.** Siblings do not read each
  other's summaries; they collaborate through the OCC-shared
  workspace (§1.2). The summary blackboard is for evaluator
  verification, not for inter-sibling communication.
- **No live cross-sibling tool-call streaming.** Siblings see each
  other's *committed* tool-call results in the workspace, not the
  in-flight overlay state of a tool call still running. The
  per-tool-call commit is the visibility boundary, by design.
- **No workspace isolation between agents.** No worktrees, no
  per-agent FS sandbox. All executors share one filesystem because
  *agents knowing what other agents are doing is part of the design*
  — isolation would force coordination through prose summaries
  alone, which is exactly the failure mode (coordination drift)
  this architecture exists to avoid. OCC's job is to make sharing
  safe at the tool-call level; visibility is a feature, not a
  side-effect.

---

## 5. Open questions tracked for v2

- **Replan semantics under partial failure.** The
  `replan_dependents_must_be_pending` invariant is asserted; v2 should
  formalize *who* triggers replans. The current answer is
  "continuation-only" — but a sibling failure mid-DAG creates a
  partially-completed subtree the evaluator cannot fully judge until
  it closes. Open: should evaluator-on-partial-failure be allowed to
  fail the subtree fast, or always wait for sinks?
- **OCC conflict surface to the evaluator.** A per-tool-call
  conflict today fails the tool call locally; the executor sees it
  and decides what to do. Most of the time that is the right scope,
  but high-frequency conflicts on the same region across siblings
  are a structural signal worth surfacing further up — possibly
  through the summary, possibly to the evaluator. Open: do we need
  a typed conflict-stat channel, or is per-tool-call retry enough?
- **Throughput ceiling.** Empirically, how does per-tool-call
  conflict rate scale with concurrent executors per session?
  Conflict density is a function of *tool-call concurrency* on
  overlapping regions, not of executor count directly. Needs a load
  test analogous to the 100-load number cited for git-workspace.
- **Rubric-form acceptance criteria.** Acceptance is currently prose
  judged by the evaluator. Replacing it with a typed rubric (list of
  pass/fail checks) would reduce LLM-as-judge variance without
  expanding the evaluator's role. Tradeoff: rubric authorship cost
  vs. judgment stability.

---

## 6. TL;DR

v1 = **DAG inside a phase** (parallelism) + **evaluator at every
sink** (verification gate, single-shot) + **one shared workspace
with per-tool-call OCC** (so siblings can see each other's work
without tearing it) + **prompt-guided summary propagation**
(auditable verification channel).

The two-channel split is the design center: the **workspace is the
collaboration channel** — one sandbox, no isolation, agents are
*meant* to see each other's committed edits, with OCC making each
tool call atomic. The **summary chain is the verification channel**
— evaluators read typed summaries, not transcripts. Continuation
under an evaluator is the only iteration primitive; closed tasks are
facts, never revised.

Claude teams give you worktree isolation but agents fly blind to
each other's work and there are no phase gates. Qoder gives you
roles but no structural parallelism. OMC gives you operator-driven
concurrency but no programmatic conflict story. v1 is the bet that
*long-horizon unattended multi-agent runs need DAG parallelism,
sink-bound verification gates, and a shared workspace where
visibility is intentional* — and that the cost is paid back by
evaluators reasoning in O(direct children) over typed summaries
while siblings collaborate live on a single filesystem.
